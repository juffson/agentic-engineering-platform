//! Read-only GitHub and GitLab API access through the system curl.
use super::secret::valid_token;
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeMap,
    fmt,
    io::Write,
    process::{Command, Output, Stdio},
    time::Duration,
};
use url::{Host, Url, form_urlencoded};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    GitHub,
    GitLab,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Open,
    Draft,
    Merged,
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Pull {
    pub branch: String,
    pub number: u64,
    pub title: String,
    pub state: State,
    pub url: String,
    pub created_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Unreachable,
    Auth,
    Forbidden,
    Cert,
    RateLimited,
    Timeout,
    NotFound,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ApiError {
    pub kind: ErrorKind,
    pub message: String,
    pub status: Option<u16>,
    pub reset_at: Option<i64>,
}

impl ApiError {
    fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            status: None,
            reset_at: None,
        }
    }

    fn other(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Other, message)
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

pub fn normalize_host(input: &str) -> Result<String> {
    let input = input.trim();
    let authority = match input.split_once("://") {
        Some((scheme, authority)) => {
            ensure!(scheme.eq_ignore_ascii_case("https"), "HTTPS is required");
            authority
        }
        None => input,
    }
    .trim_end_matches('/');
    ensure!(
        !authority.is_empty()
            && !authority.contains(['/', '\\', '?', '#', '@'])
            && !authority
                .chars()
                .any(|c| c.is_whitespace() || c.is_control()),
        "invalid API host"
    );
    let url = Url::parse(&format!("https://{authority}")).context("invalid API host")?;
    let host = url.host().context("missing API host")?;
    if let Host::Domain(domain) = host {
        ensure!(
            domain.trim_end_matches('.').split('.').all(|label| {
                !label.is_empty()
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            }) && !domain.ends_with(".."),
            "invalid API host"
        );
    }
    Ok(url.origin().ascii_serialization())
}

pub struct Forge {
    kind: Kind,
    api_base: Option<Url>,
    timeout: Duration,
    #[cfg(test)]
    curl_program: Option<std::path::PathBuf>,
}

impl Forge {
    pub fn new(kind: Kind, host_url: &str) -> Self {
        let api_base = normalize_host(host_url).ok().and_then(|origin| {
            let base = match kind {
                Kind::GitHub if origin == "https://github.com" => {
                    "https://api.github.com".to_owned()
                }
                Kind::GitHub => format!("{origin}/api/v3"),
                Kind::GitLab => format!("{origin}/api/v4"),
            };
            Url::parse(&base).ok()
        });
        Self::configured(kind, api_base)
    }

    pub fn with_base(kind: Kind, host: &str, base: &str) -> Result<Self> {
        normalize_host(host)?;
        let raw_authority = base
            .split_once("://")
            .map(|(_, rest)| rest.split('/').next().unwrap_or_default())
            .unwrap_or_default();
        let base = Url::parse(base).context("invalid API base URL")?;
        ensure!(
            !raw_authority.is_empty()
                && !raw_authority.contains(['@', '\\'])
                && base.host().is_some()
                && base.query().is_none()
                && base.fragment().is_none()
                && (base.scheme() == "https"
                    || (base.scheme() == "http"
                        && raw_authority
                            .strip_prefix("127.0.0.1:")
                            .and_then(|port| port.parse::<u16>().ok())
                            .is_some()
                        && base.host_str() == Some("127.0.0.1"))),
            "API base must use HTTPS or loopback HTTP with a port"
        );
        Ok(Self::configured(kind, Some(base)))
    }

    fn configured(kind: Kind, api_base: Option<Url>) -> Self {
        Self {
            kind,
            api_base,
            timeout: Duration::from_secs(15),
            #[cfg(test)]
            curl_program: None,
        }
    }

    #[cfg(all(test, target_os = "macos"))]
    fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn account(&self, token: &str) -> Result<String, ApiError> {
        #[derive(Deserialize)]
        struct Account {
            login: Option<String>,
            username: Option<String>,
        }
        let body = self.get(token, "/user")?;
        let account: Account = decode(&body)?;
        let name = match self.kind {
            Kind::GitHub => account.login,
            Kind::GitLab => account.username,
        }
        .filter(|name| !name.is_empty())
        .ok_or_else(|| ApiError::other("API response is missing the account name"))?;
        reject_reflected_token(token, &[&name])?;
        Ok(name)
    }

    pub fn pulls(&self, token: &str, path: &str) -> Result<Vec<Pull>, ApiError> {
        if path.is_empty() || path.split('/').any(|part| matches!(part, "" | "." | "..")) {
            return Err(ApiError::other("invalid repository path"));
        }
        match self.kind {
            Kind::GitHub => {
                let (owner, repo) = path
                    .split_once('/')
                    .filter(|(_, repo)| !repo.contains('/'))
                    .ok_or_else(|| ApiError::other("invalid GitHub repository path"))?;
                let endpoint = format!(
                    "/repos/{}/{}/pulls?state=all&sort=updated&direction=desc",
                    encode_segment(owner),
                    encode_segment(repo)
                );
                self.list(token, &endpoint, |row: GitHubPull| row.into_pull(path))
            }
            Kind::GitLab => {
                let endpoint = format!(
                    "/projects/{}/merge_requests?state=all&order_by=updated_at&sort=desc",
                    encode_segment(path)
                );
                self.list(token, &endpoint, GitLabPull::into_pull)
            }
        }
    }

    fn list<T: DeserializeOwned>(
        &self,
        token: &str,
        endpoint: &str,
        mut parse: impl FnMut(T) -> Result<Option<Pull>, ApiError>,
    ) -> Result<Vec<Pull>, ApiError> {
        const PAGE_SIZE: usize = 100;
        let mut pulls = Vec::new();
        for page in 1..=3 {
            let body = self.get(
                token,
                &format!("{endpoint}&per_page={PAGE_SIZE}&page={page}"),
            )?;
            let rows: Vec<T> = decode(&body)?;
            let size = rows.len();
            for row in rows {
                if let Some(pull) = parse(row)? {
                    reject_reflected_token(token, &[&pull.branch, &pull.title, &pull.url])?;
                    pulls.push(pull);
                }
            }
            if size < PAGE_SIZE {
                break;
            }
        }
        Ok(pulls)
    }

    fn request(&self, token: &str, path: &str) -> Result<(Vec<String>, String), ApiError> {
        if !valid_token(token) {
            return Err(ApiError::new(ErrorKind::Auth, "invalid plugin token"));
        }
        let base = self
            .api_base
            .as_ref()
            .ok_or_else(|| ApiError::other("invalid API host"))?;
        let timeout = self.timeout.as_secs_f64().to_string();
        let protocols = if base.scheme() == "http" {
            "=http"
        } else {
            "=https"
        };
        let mut args = vec![
            "--disable",
            "--silent",
            "--show-error",
            "--max-time",
            &timeout,
            "--proto",
            protocols,
            "--globoff",
            "--suppress-connect-headers",
            "--request",
            "GET",
            "--dump-header",
            "-",
            "--write-out",
            r"\n%{http_code}",
            "--config",
            "-",
        ];
        if base.scheme() == "http" {
            args.extend(["--noproxy", "127.0.0.1"]);
        }
        let base = base.as_str().trim_end_matches('/');
        let authorization = match self.kind {
            Kind::GitHub => format!("Authorization: Bearer {token}"),
            Kind::GitLab => format!("PRIVATE-TOKEN: {token}"),
        };
        let config = format!(
            "url = \"{base}{path}\"\nheader = \"{authorization}\"\nheader = \"Accept: application/json\"\nuser-agent = \"aep-studio\"\n"
        );
        Ok((args.into_iter().map(str::to_owned).collect(), config))
    }

    fn get(&self, token: &str, path: &str) -> Result<Vec<u8>, ApiError> {
        let (args, config) = self.request(token, path)?;
        #[cfg(test)]
        let program = self
            .curl_program
            .as_deref()
            .unwrap_or(std::path::Path::new("/usr/bin/curl"));
        #[cfg(not(test))]
        let program = "/usr/bin/curl";
        let output = run_curl(Command::new(program), &args, &config)?;
        Ok(response_body(&output, token)?.to_vec())
    }
}

fn run_curl(mut command: Command, args: &[String], config: &str) -> Result<Output, ApiError> {
    let mut child = command
        .args(args)
        .env("LC_ALL", "C")
        .env_remove(crate::CONTROL_OPERATOR_TOKEN)
        .env_remove(crate::CONTROL_DEVICE_TOKEN)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| ApiError::other(format!("start curl: {err}")))?;
    let written = child.stdin.take().unwrap().write_all(config.as_bytes());
    let output = child
        .wait_with_output()
        .map_err(|err| ApiError::other(format!("wait for curl: {err}")))?;
    if output.status.success() {
        written.map_err(|err| ApiError::other(format!("write curl input: {err}")))?;
    }
    Ok(output)
}

fn response_body<'a>(output: &'a Output, token: &str) -> Result<&'a [u8], ApiError> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr.lines().map(str::trim).find(|line| !line.is_empty());
        return Err(ApiError::new(
            curl_error_kind(output.status.code()),
            message
                .map(|line| safe_message(line, token))
                .unwrap_or_else(|| format!("curl failed ({})", output.status)),
        ));
    }
    let (status, headers, body) = parse_http(&output.stdout)
        .ok_or_else(|| ApiError::other("invalid HTTP response from curl"))?;
    if (200..300).contains(&status) {
        Ok(body)
    } else {
        Err(http_error(status, &headers, body, token))
    }
}

fn curl_error_kind(code: Option<i32>) -> ErrorKind {
    match code {
        Some(6 | 7) => ErrorKind::Unreachable,
        Some(28) => ErrorKind::Timeout,
        Some(35 | 51 | 58 | 59 | 60 | 77 | 83 | 90 | 91) => ErrorKind::Cert,
        _ => ErrorKind::Other,
    }
}

type Headers = BTreeMap<String, String>;

fn parse_http(output: &[u8]) -> Option<(u16, Headers, &[u8])> {
    let status_offset = output.iter().rposition(|&b| b == b'\n')?;
    let (mut response, status_line) = output.split_at(status_offset);
    let status = std::str::from_utf8(&status_line[1..]).ok()?.parse().ok()?;
    loop {
        let mut offset = 0;
        let end = response
            .split_inclusive(|&byte| byte == b'\n')
            .find_map(|line| {
                offset += line.len();
                matches!(line, b"\r\n" | b"\n").then_some(offset)
            })?;
        let header = String::from_utf8_lossy(&response[..end]);
        let mut lines = header.lines();
        let code = lines
            .next()?
            .strip_prefix("HTTP/")?
            .split_whitespace()
            .nth(1)?
            .parse::<u16>()
            .ok()?;
        response = &response[end..];
        if code == status {
            let headers = lines
                .filter_map(|line| line.split_once(':'))
                .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
                .collect();
            return Some((status, headers, response));
        }
    }
}

fn http_error(status: u16, headers: &Headers, body: &[u8], token: &str) -> ApiError {
    let exhausted = headers
        .get("x-ratelimit-remaining")
        .is_some_and(|value| value == "0");
    let kind = match status {
        401 => ErrorKind::Auth,
        403 if exhausted => ErrorKind::RateLimited,
        403 => ErrorKind::Forbidden,
        404 => ErrorKind::NotFound,
        429 => ErrorKind::RateLimited,
        _ => ErrorKind::Other,
    };
    let reset_at = (kind == ErrorKind::RateLimited)
        .then(|| {
            let timestamp = |key| headers.get(key)?.parse::<i64>().ok().filter(|&n| n >= 0);
            timestamp("x-ratelimit-reset")
                .or_else(|| timestamp("ratelimit-reset"))
                .or_else(|| {
                    timestamp("retry-after").map(|secs| Utc::now().timestamp().saturating_add(secs))
                })
        })
        .flatten();
    let message = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|json| {
            json.get("message")?
                .as_str()
                .map(|msg| safe_message(msg, token))
        })
        .unwrap_or_else(|| format!("HTTP {status}"));
    ApiError {
        kind,
        message,
        status: Some(status),
        reset_at,
    }
}

fn safe_message(message: &str, token: &str) -> String {
    message
        .replace(token, "[redacted]")
        .chars()
        .take(120)
        .collect()
}

fn reject_reflected_token(token: &str, fields: &[&str]) -> Result<(), ApiError> {
    if fields.iter().any(|field| field.contains(token)) {
        return Err(ApiError::other(
            "API response contains the request credential",
        ));
    }
    Ok(())
}

fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(body).map_err(|_| ApiError::other("invalid JSON response from API"))
}

fn encode_segment(segment: &str) -> String {
    form_urlencoded::byte_serialize(segment.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

fn creation_time(value: &str) -> Result<i64, ApiError> {
    DateTime::parse_from_rfc3339(value)
        .map(|time| time.timestamp())
        .map_err(|_| ApiError::other("invalid pull request creation date"))
}

#[derive(Deserialize)]
struct GitHubPull {
    number: u64,
    title: String,
    state: String,
    #[serde(default)]
    draft: bool,
    merged_at: Option<String>,
    html_url: String,
    created_at: String,
    head: GitHubHead,
}

#[derive(Deserialize)]
struct GitHubHead {
    #[serde(rename = "ref")]
    branch: String,
    repo: Option<GitHubRepo>,
}

#[derive(Deserialize)]
struct GitHubRepo {
    full_name: String,
}

impl GitHubPull {
    fn into_pull(self, path: &str) -> Result<Option<Pull>, ApiError> {
        if !self.head.repo.is_some_and(|repo| repo.full_name == path) {
            return Ok(None);
        }
        let state = match (self.state.as_str(), self.merged_at.is_some(), self.draft) {
            (_, true, _) => State::Merged,
            ("closed", _, _) => State::Closed,
            (_, _, true) => State::Draft,
            _ => State::Open,
        };
        Ok(Some(Pull {
            branch: self.head.branch,
            number: self.number,
            title: self.title,
            state,
            url: self.html_url,
            created_at: creation_time(&self.created_at)?,
        }))
    }
}

#[derive(Deserialize)]
struct GitLabPull {
    iid: u64,
    title: String,
    state: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    work_in_progress: bool,
    web_url: String,
    created_at: String,
    source_branch: String,
    source_project_id: Option<u64>,
    target_project_id: u64,
}

impl GitLabPull {
    fn into_pull(self) -> Result<Option<Pull>, ApiError> {
        if self.source_project_id != Some(self.target_project_id) {
            return Ok(None);
        }
        let state = match self.state.as_str() {
            "merged" => State::Merged,
            "closed" => State::Closed,
            _ if self.draft || self.work_in_progress => State::Draft,
            _ => State::Open,
        };
        Ok(Some(Pull {
            branch: self.source_branch,
            number: self.iid,
            title: self.title,
            state,
            url: self.web_url,
            created_at: creation_time(&self.created_at)?,
        }))
    }
}

pub fn latest_by_branch(pulls: Vec<Pull>) -> BTreeMap<String, (Pull, usize)> {
    use std::collections::btree_map::Entry;
    let mut grouped = BTreeMap::new();
    for pull in pulls {
        match grouped.entry(pull.branch.clone()) {
            Entry::Vacant(entry) => {
                entry.insert((pull, 0));
            }
            Entry::Occupied(mut entry) => {
                let (latest, others) = entry.get_mut();
                *others += 1;
                if pull.created_at > latest.created_at {
                    *latest = pull;
                }
            }
        }
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::{
        fs,
        os::unix::{fs::PermissionsExt, process::ExitStatusExt},
        process::ExitStatus,
    };

    const TOKEN: &str = "studio_Fixture_Token";
    const BASE: &str = "http://127.0.0.1:18948";
    const GITHUB_LIST: &str = "/repos/o/r/pulls?state=all&sort=updated&direction=desc&per_page=100";
    const GITLAB_LIST: &str = "/projects/g%2Fsub%20group%2Fr%2Bwith/merge_requests?state=all&order_by=updated_at&sort=desc&per_page=100";

    struct Stub {
        dir: tempfile::TempDir,
        forge: Forge,
    }

    impl Stub {
        fn new(kind: Kind) -> Result<Self> {
            let dir = tempfile::tempdir()?;
            let program = dir.path().join("curl");
            fs::write(
                &program,
                r#"#!/bin/sh
set -eu
root=${0%/*}
token=studio_Fixture_Token
case "$*" in *"$token"*) exit 10 ;; esac
case " $* " in *" --request GET "*) ;; *) exit 11 ;; esac
case " $* " in *" --config - "*) ;; *) exit 12 ;; esac
IFS= read -r url
IFS= read -r authorization
IFS= read -r header < "$root/authorization"
[ "$authorization" = "header = \"$header $token\"" ] || exit 13
for fixture in "$root"/request-*; do
    IFS= read -r expected_url < "$fixture/url"
    if [ "$url" = "$expected_url" ]; then
        printf '%s\n' "$url" >> "$root/calls"
        exec /bin/cat "$fixture/response"
    fi
done
printf '%s\n' 'unexpected request' >&2
exit 14
"#,
            )?;
            fs::set_permissions(&program, fs::Permissions::from_mode(0o700))?;
            let header = match kind {
                Kind::GitHub => "Authorization: Bearer",
                Kind::GitLab => "PRIVATE-TOKEN:",
            };
            fs::write(dir.path().join("authorization"), format!("{header}\n"))?;
            let mut forge = Forge::with_base(kind, "example.invalid", BASE)?;
            forge.curl_program = Some(program);
            Ok(Self { dir, forge })
        }

        fn respond(&self, id: u8, path: &str, code: u16, headers: &str, json: Value) -> Result<()> {
            let dir = self.dir.path().join(format!("request-{id}"));
            fs::create_dir(&dir)?;
            fs::write(dir.join("url"), format!("url = \"{BASE}{path}\"\n"))?;
            fs::write(
                dir.join("response"),
                http_output(code, headers, &json.to_string()),
            )?;
            Ok(())
        }

        fn calls(&self) -> Result<usize> {
            Ok(fs::read_to_string(self.dir.path().join("calls"))?
                .lines()
                .count())
        }
    }

    fn http_output(status: u16, headers: &str, body: &str) -> String {
        format!("HTTP/2 {status}\r\n{headers}\r\n{body}\n{status}")
    }

    fn response(status: u16, headers: &str, body: &str) -> Output {
        Output {
            status: ExitStatus::from_raw(0),
            stdout: http_output(status, headers, body).into_bytes(),
            stderr: Vec::new(),
        }
    }

    #[test]
    fn forge_normalizes_host_and_uses_read_only_protocols() -> Result<()> {
        assert_eq!(
            normalize_host(" GitLab.Example.COM/ ")?,
            "https://gitlab.example.com"
        );
        assert_eq!(
            normalize_host("HTTPS://gitlab.example.com:8443///")?,
            "https://gitlab.example.com:8443"
        );
        assert_eq!(normalize_host("[::1]:8443")?, "https://[::1]:8443");
        for invalid in [
            "",
            "http://x",
            "https://",
            "https://x/user",
            "https://x?query",
            "https://x#fragment",
            "https://u:p@x",
            "https://@x",
            "x y",
            "x..example",
            "x:65536",
            "x\\path",
        ] {
            assert!(normalize_host(invalid).is_err(), "{invalid}");
        }
        for (kind, host, endpoint) in [
            (Kind::GitHub, "github.com", "https://api.github.com/user"),
            (
                Kind::GitHub,
                "enterprise.example:8443",
                "https://enterprise.example:8443/api/v3/user",
            ),
            (
                Kind::GitLab,
                "gitlab.example",
                "https://gitlab.example/api/v4/user",
            ),
        ] {
            let (argv, config) = Forge::new(kind, host).request(TOKEN, "/user")?;
            assert_eq!(argv.first().map(String::as_str), Some("--disable"));
            assert!(argv.windows(2).any(|args| args == ["--proto", "=https"]));
            assert!(argv.windows(2).any(|args| args == ["--max-time", "15"]));
            assert!(argv.windows(2).any(|args| args == ["--request", "GET"]));
            assert!(!argv.iter().any(|arg| arg.contains(TOKEN)));
            assert!(config.starts_with(&format!("url = \"{endpoint}\"\n")));
            assert!(config.contains("Accept: application/json"));
            assert!(config.contains("aep-studio"));
        }
        for invalid in [
            "file:///tmp/user",
            "http://example.invalid:1234",
            "http://127.0.0.1",
            "http://0x7f.1:1234",
            "https://@example.invalid",
        ] {
            assert!(
                Forge::with_base(Kind::GitHub, "github.com", invalid).is_err(),
                "{invalid}"
            );
        }
        assert!(Forge::with_base(Kind::GitHub, "github.com", "http://127.0.0.1:80").is_ok());
        let invalid = Forge::new(Kind::GitHub, "http://x")
            .request(TOKEN, "/user")
            .unwrap_err();
        assert_eq!(invalid.kind, ErrorKind::Other);
        Ok(())
    }

    #[test]
    fn forge_account_uses_the_expected_header_and_redacts_auth_failures() -> Result<()> {
        let github = Stub::new(Kind::GitHub)?;
        github.respond(1, "/user", 200, "", json!({"login":"octocat"}))?;
        assert_eq!(github.forge.account(TOKEN)?, "octocat");
        assert_eq!(github.calls()?, 1);

        let gitlab = Stub::new(Kind::GitLab)?;
        gitlab.respond(1, "/user", 200, "", json!({"username":"gitlab-user"}))?;
        assert_eq!(gitlab.forge.account(TOKEN)?, "gitlab-user");
        let failure = Stub::new(Kind::GitLab)?;
        failure.respond(
            1,
            "/user",
            401,
            "",
            json!({"message": format!("Invalid credential {TOKEN}")}),
        )?;
        let err = failure.forge.account(TOKEN).unwrap_err();
        assert_eq!((err.kind, err.status), (ErrorKind::Auth, Some(401)));
        assert_eq!(err.message, "Invalid credential [redacted]");
        assert!(!serde_json::to_string(&err)?.contains(TOKEN));
        assert_eq!(
            failure
                .forge
                .account("invalid\ncredential")
                .unwrap_err()
                .kind,
            ErrorKind::Auth
        );
        assert_eq!(failure.calls()?, 1);

        let reflected = Stub::new(Kind::GitHub)?;
        reflected.respond(1, "/user", 200, "", json!({"login": TOKEN}))?;
        let path = &format!("{GITHUB_LIST}&page=1");
        let mut pull = github_pull(1, "open", false, None, "main");
        pull["title"] = json!(TOKEN);
        reflected.respond(2, path, 200, "", json!([pull]))?;
        for err in [
            reflected.forge.account(TOKEN).unwrap_err(),
            reflected.forge.pulls(TOKEN, "o/r").unwrap_err(),
        ] {
            assert_eq!(err.kind, ErrorKind::Other);
            assert!(!serde_json::to_string(&err)?.contains(TOKEN));
        }
        Ok(())
    }

    #[test]
    fn forge_classifies_http_errors_and_rate_limit_resets() {
        let errors = [
            (403, "", ErrorKind::Forbidden, None),
            (404, "", ErrorKind::NotFound, None),
            (503, "", ErrorKind::Other, None),
            (
                403,
                "X-RateLimit-Remaining: 0\r\nX-RateLimit-Reset: 1764000000\r\nRateLimit-Reset: 1764000001\r\n",
                ErrorKind::RateLimited,
                Some(1764000000),
            ),
            (
                429,
                "RateLimit-Reset: 1764000002\r\nRetry-After: 60\r\n",
                ErrorKind::RateLimited,
                Some(1764000002),
            ),
        ];
        for (status, headers, kind, reset) in errors {
            let out = response(status, headers, r#"{"message":"from API"}"#);
            let err = response_body(&out, TOKEN).unwrap_err();
            assert_eq!(
                (err.kind, err.status, err.reset_at),
                (kind, Some(status), reset)
            );
            assert_eq!(err.message, "from API");
        }
        let before = Utc::now().timestamp();
        let out = response(429, "Retry-After: 60\r\n", "");
        let err = response_body(&out, TOKEN).unwrap_err();
        assert_eq!(err.kind, ErrorKind::RateLimited);
        assert!((before + 60..=Utc::now().timestamp() + 60).contains(&err.reset_at.unwrap()));
        assert_eq!(err.message, "HTTP 429");
        let long = "字".repeat(200);
        let out = response(501, "", &json!({"message": long}).to_string());
        assert_eq!(
            response_body(&out, TOKEN)
                .unwrap_err()
                .message
                .chars()
                .count(),
            120
        );
    }

    #[test]
    fn forge_classifies_curl_transport_errors_and_reads_final_response() {
        for (code, kind) in [
            (6, ErrorKind::Unreachable),
            (7, ErrorKind::Unreachable),
            (28, ErrorKind::Timeout),
            (35, ErrorKind::Cert),
            (51, ErrorKind::Cert),
            (58, ErrorKind::Cert),
            (59, ErrorKind::Cert),
            (60, ErrorKind::Cert),
            (77, ErrorKind::Cert),
            (83, ErrorKind::Cert),
            (90, ErrorKind::Cert),
            (91, ErrorKind::Cert),
            (3, ErrorKind::Other),
        ] {
            let output = Output {
                status: ExitStatus::from_raw(code << 8),
                stdout: Vec::new(),
                stderr: format!("curl error {TOKEN}\nsecond line\n").into_bytes(),
            };
            let err = response_body(&output, TOKEN).unwrap_err();
            assert_eq!((err.kind, err.status), (kind, None));
            assert_eq!(err.message, "curl error [redacted]");
        }
        let output = b"HTTP/1.1 103 Early Hints\r\nLink: </hint>\r\n\r\nHTTP/2 200\r\nContent-Type: application/json\r\n\r\n{}\n200";
        let (status, headers, body) = parse_http(output).unwrap();
        assert_eq!(status, 200);
        assert_eq!(headers["content-type"], "application/json");
        assert_eq!(body, b"{}");
    }

    fn github_pull(
        n: u64,
        state: &str,
        draft: bool,
        merged_at: Option<&str>,
        branch: &str,
    ) -> Value {
        json!({
            "number": n, "title": format!("Pull {n}"), "state": state, "draft": draft,
            "merged_at": merged_at, "html_url": format!("https://github.com/o/r/pull/{n}"),
            "created_at": "2026-05-15T12:00:00Z", "head": {"ref": branch, "repo": {"full_name": "o/r"}}
        })
    }

    #[test]
    fn forge_github_filters_forks_and_returns_latest_pull_per_branch() -> Result<()> {
        let stub = Stub::new(Kind::GitHub)?;
        let branch = "feature/中文+fix";
        let open = github_pull(2, "open", false, None, branch);
        let mut newer = github_pull(1, "open", true, None, branch);
        newer["created_at"] = json!("2026-05-16T12:00:00Z");
        let draft = github_pull(3, "open", true, None, "draft");
        let merged = github_pull(4, "closed", false, Some("2026-06-01T00:00:00Z"), "merged");
        let closed = github_pull(5, "closed", true, None, "closed");
        let mut fork = github_pull(6, "open", false, None, "fork");
        fork["head"]["repo"]["full_name"] = json!("fork/r");
        let mut deleted_fork = github_pull(7, "open", false, None, "deleted-fork");
        deleted_fork["head"]["repo"] = Value::Null;
        stub.respond(
            1,
            &format!("{GITHUB_LIST}&page=1"),
            200,
            "",
            json!([open, newer, draft, merged, closed, fork, deleted_fork]),
        )?;
        let pulls = stub.forge.pulls(TOKEN, "o/r")?;
        assert_eq!(
            pulls.iter().map(|pull| pull.state).collect::<Vec<_>>(),
            [
                State::Open,
                State::Draft,
                State::Draft,
                State::Merged,
                State::Closed
            ]
        );
        let latest = latest_by_branch(pulls);
        assert_eq!(latest.len(), 4);
        let (pull, others) = &latest[branch];
        assert_eq!(
            (pull.number, pull.url.as_str(), *others),
            (1, "https://github.com/o/r/pull/1", 1)
        );
        assert_eq!(stub.calls()?, 1);
        Ok(())
    }

    #[test]
    fn forge_gitlab_encodes_nested_projects_and_uses_merge_request_iids() -> Result<()> {
        let stub = Stub::new(Kind::GitLab)?;
        let row = |iid, state, draft, wip, project| {
            json!({
                "id": 1000 + iid, "iid": iid, "title": "Merge request", "state": state,
                "draft": draft, "work_in_progress": wip, "source_project_id": project,
                "target_project_id": 8, "web_url": format!("https://gitlab.example.invalid/g/r/-/merge_requests/{iid}"),
                "source_branch": "aep-stage/中文+fix", "created_at": "2026-05-15T12:00:00.456+08:00"
            })
        };
        let rows = json!([
            row(1, "opened", false, false, 8),
            row(2, "opened", false, true, 8),
            row(3, "merged", true, false, 8),
            row(4, "closed", true, false, 8),
            row(5, "opened", false, false, 9),
        ]);
        stub.respond(1, &format!("{GITLAB_LIST}&page=1"), 200, "", rows)?;
        let pulls = stub.forge.pulls(TOKEN, "g/sub group/r+with")?;
        assert_eq!(
            pulls
                .iter()
                .map(|pull| (pull.number, pull.state))
                .collect::<Vec<_>>(),
            [
                (1, State::Open),
                (2, State::Draft),
                (3, State::Merged),
                (4, State::Closed)
            ]
        );
        assert!(pulls.iter().all(|pull| pull.branch == "aep-stage/中文+fix"));
        assert_eq!(pulls[0].created_at, creation_time("2026-05-15T04:00:00Z")?);
        Ok(())
    }

    #[test]
    fn forge_paginates_by_unfiltered_page_size_and_stops_after_three_pages() -> Result<()> {
        let stub = Stub::new(Kind::GitHub)?;
        let mut fork = github_pull(1, "open", false, None, "fork");
        fork["head"]["repo"]["full_name"] = json!("fork/r");
        let page_one = json!(vec![fork; 100]);
        let page_two = json!(vec![github_pull(2, "open", false, None, "page-two"); 100]);
        let page_three = json!(vec![github_pull(3, "open", false, None, "page-three"); 100]);
        for (index, page) in [(1, page_one), (2, page_two), (3, page_three)] {
            stub.respond(index, &format!("{GITHUB_LIST}&page={index}"), 200, "", page)?;
        }
        let pulls = stub.forge.pulls(TOKEN, "o/r")?;
        assert_eq!(pulls.len(), 200);
        assert_eq!(stub.calls()?, 3);
        Ok(())
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn forge_system_curl_reports_an_unreachable_endpoint() -> Result<()> {
        let forge = Forge::with_base(Kind::GitHub, "github.com", "http://127.0.0.1:0")?
            .with_timeout(Duration::from_secs(1));
        let error = forge.account(TOKEN).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Unreachable);
        assert_eq!(error.status, None);
        assert!(!error.message.contains(TOKEN));
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires permission to listen on a loopback socket"]
    fn forge_system_curl_reads_from_a_loopback_server() -> Result<()> {
        use std::{
            io::{BufRead, BufReader},
            net::{TcpListener, TcpStream},
            thread,
        };

        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        thread::scope(|scope| -> Result<()> {
            listener.set_nonblocking(true)?;
            let server = scope.spawn(move || -> Result<()> {
                let accept = || -> Result<TcpStream> {
                    let start = std::time::Instant::now();
                    loop {
                        match listener.accept() {
                            Ok((stream, _)) => return Ok(stream),
                            Err(err)
                                if err.kind() == std::io::ErrorKind::WouldBlock
                                    && start.elapsed() < Duration::from_secs(2) =>
                            {
                                thread::sleep(Duration::from_millis(10));
                            }
                            Err(err) => return Err(err.into()),
                        }
                    }
                };
                let mut stream = accept()?;
                stream.set_read_timeout(Some(Duration::from_secs(1)))?;
                let request = BufReader::new(&mut stream);
                let mut lines = request.lines();
                assert!(
                    lines
                        .next()
                        .transpose()?
                        .unwrap_or_default()
                        .starts_with("GET /user ")
                );
                let mut authenticated = false;
                for line in lines {
                    let line = line?;
                    if line.is_empty() {
                        break;
                    }
                    authenticated |= line == format!("Authorization: Bearer {TOKEN}");
                }
                assert!(authenticated);
                let body = r#"{"login":"loopback"}"#;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )?;
                drop(stream);
                let _stalled = accept()?;
                thread::sleep(Duration::from_millis(450));
                Ok(())
            });
            let forge = Forge::with_base(Kind::GitHub, "github.com", &format!("http://{addr}"))?;
            assert_eq!(forge.account(TOKEN)?, "loopback");
            let timeout = forge
                .with_timeout(Duration::from_millis(150))
                .account(TOKEN);
            server.join().unwrap()?;
            assert_eq!(timeout.unwrap_err().kind, ErrorKind::Timeout);
            Ok(())
        })
    }
}
