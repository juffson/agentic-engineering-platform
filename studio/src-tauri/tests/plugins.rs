//! Studio's plugin commands over a temporary catalog, Git repositories and an HTTP forge.
use aep::{
    project::secret::{Memory, Secrets},
    workspace::git,
};
use aep_studio_app::{
    api,
    plugins::{self, Ctx, GitToolsTarget},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const TOKEN: &str = "ghp_TESTTOKEN1234567890";
const ORIGIN: &str = "https://github.com/acme/demo.git";
const PULLS: &str =
    "/repos/acme/demo/pulls?state=all&sort=updated&direction=desc&per_page=100&page=1";

fn context(home: PathBuf, api_base: Option<String>) -> Ctx {
    Ctx {
        home,
        secrets: Arc::new(Memory::default()),
        api_base,
    }
}

fn plugin<'a>(items: &'a [Value], id: &str) -> &'a Value {
    items.iter().find(|item| item["id"] == id).unwrap()
}

#[test]
fn defaults_persist_with_branch_dependencies_and_corrupt_settings_are_backed_up() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = context(temp.path().join("catalog"), None);
    let settings = ctx.home.join("plugins/plugins.json");
    let list = plugins::list(&ctx)?;
    assert_eq!(
        list.iter().map(|item| &item["id"]).collect::<Vec<_>>(),
        ["branches", "github", "gitlab"]
    );
    assert_eq!(plugin(&list, "branches")["badge"], "enabled");
    for id in ["github", "gitlab"] {
        assert_eq!(plugin(&list, id)["badge"], "disabled");
    }
    assert!(!settings.exists());
    assert_eq!(
        plugins::remote_status(&ctx, "missing", "/nonexistent")?,
        json!({"active":false})
    );

    plugins::set_enabled(&ctx, "branches", false)?;
    let list = plugins::set_enabled(&ctx, "github", true)?;
    assert_eq!(plugin(&list, "github")["enabled"], true);
    assert_eq!(plugin(&list, "github")["badge"], "unavailable");
    assert_eq!(
        plugins::refresh(&ctx, "missing", "/nonexistent")?,
        json!({"active":false})
    );
    let reopened = context(ctx.home.clone(), None);
    assert_eq!(plugins::list(&reopened)?, list);
    let list = plugins::set_enabled(&reopened, "branches", true)?;
    assert_eq!(plugin(&list, "github")["badge"], "needs_config");
    assert!(plugins::set_enabled(&ctx, "unknown", true).is_err());
    assert!(plugins::save(&ctx, "branches", "", Some(TOKEN)).is_err());
    assert!(plugins::clear_token(&ctx, "branches").is_err());
    assert!(plugins::test(&ctx, "branches", "", None).is_err());

    let dir = settings.parent().unwrap();
    fs::write(dir.join("git-tools.json"), b"existing external tools")?;
    fs::write(&settings, b"{ invalid plugin settings")?;
    assert!(plugins::list(&ctx).is_err());
    assert_eq!(fs::read(&settings)?, b"{ invalid plugin settings");
    let recovered = plugins::set_enabled(&ctx, "gitlab", true)?;
    assert_eq!(plugin(&recovered, "branches")["badge"], "enabled");
    assert_eq!(plugin(&recovered, "github")["badge"], "disabled");
    assert_eq!(plugin(&recovered, "gitlab")["badge"], "needs_config");
    assert_eq!(
        fs::read(dir.join("plugins.json.bad"))?,
        b"{ invalid plugin settings"
    );
    assert_eq!(
        fs::read(dir.join("git-tools.json"))?,
        b"existing external tools"
    );
    Ok(())
}

#[test]
fn host_and_token_settings_validate_mask_retain_and_clear() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = context(temp.path().join("catalog"), None);
    plugins::set_enabled(&ctx, "gitlab", true)?;
    assert!(plugins::save(&ctx, "gitlab", "http://example.invalid", Some(TOKEN)).is_err());
    assert!(plugins::save(&ctx, "gitlab", "example.invalid", Some("bad token")).is_err());
    assert_eq!(ctx.secrets.get("gitlab")?, None);
    let list = plugins::save(&ctx, "gitlab", "GitLab.Example.com/", Some(TOKEN))?;
    let info = plugin(&list, "gitlab");
    assert_eq!(info["host"], "https://gitlab.example.com");
    assert_eq!(info["badge"], "enabled");
    assert_eq!(info["token"], json!({"set":true,"hint":"7890"}));
    assert_eq!(ctx.secrets.get("gitlab")?.as_deref(), Some(TOKEN));
    let stored: Value = serde_json::from_slice(&fs::read(ctx.home.join("plugins/plugins.json"))?)?;
    assert_eq!(stored["gitlab"]["token_hint"], "7890");
    assert!(!serde_json::to_string(&list)?.contains(TOKEN));
    assert_no_token(&ctx.home)?;

    let list = plugins::save(&ctx, "gitlab", "", None)?;
    assert_eq!(plugin(&list, "gitlab")["host"], "https://gitlab.com");
    assert_eq!(ctx.secrets.get("gitlab")?.as_deref(), Some(TOKEN));
    let list = plugins::clear_token(&ctx, "gitlab")?;
    assert_eq!(plugin(&list, "gitlab")["enabled"], true);
    assert_eq!(plugin(&list, "gitlab")["badge"], "needs_config");
    assert_eq!(
        plugin(&list, "gitlab")["token"],
        json!({"set":false,"hint":null})
    );
    assert_eq!(ctx.secrets.get("gitlab")?, None);
    Ok(())
}

struct RejectWrites;

impl Secrets for RejectWrites {
    fn get(&self, _: &str) -> Result<Option<String>> {
        bail!("unexpected secret read")
    }
    fn set(&self, _: &str, token: &str) -> Result<()> {
        bail!("write rejected: {token}")
    }
    fn delete(&self, _: &str) -> Result<()> {
        bail!("delete rejected")
    }
}

#[test]
fn rejected_secret_write_restores_settings_and_redacts_errors() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = Ctx {
        home: temp.path().join("catalog"),
        secrets: Arc::new(RejectWrites),
        api_base: None,
    };
    let file = ctx.home.join("plugins/plugins.json");
    let err = plugins::save(&ctx, "github", "other.example.com", Some(TOKEN)).unwrap_err();
    assert!(!err.to_string().contains(TOKEN));
    assert!(!file.exists());
    let list = plugins::set_enabled(&ctx, "github", true)?;
    assert!(plugins::save(&ctx, "github", "other.example.com", Some(TOKEN)).is_err());
    assert!(plugins::clear_token(&ctx, "github").is_err());
    assert_eq!(plugins::list(&ctx)?, list);
    assert_no_token(&ctx.home)?;
    Ok(())
}

#[test]
fn account_tests_classify_failures_without_saving_drafts() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let stub = Stub::new(Reply::json(
        200,
        json!({"login":"octocat","username":"tanuki"}),
    ))?;
    let ctx = context(temp.path().join("catalog"), Some(stub.base()));
    assert_eq!(
        plugins::test(&ctx, "github", "", Some(TOKEN))?,
        json!({"ok":true,"account":"octocat"})
    );
    assert_eq!(
        plugins::test(&ctx, "gitlab", "", Some(TOKEN))?,
        json!({"ok":true,"account":"tanuki"})
    );
    assert_eq!(ctx.secrets.get("github")?, None);
    assert_eq!(
        stub.requests(),
        vec![
            Request::new("/user", true, false),
            Request::new("/user", false, true)
        ]
    );
    assert!(!ctx.home.exists());

    plugins::set_enabled(&ctx, "github", true)?;
    plugins::save(&ctx, "github", "", Some(TOKEN))?;
    stub.reply(Reply::json(
        401,
        json!({"message": format!("bad credential {TOKEN}")}),
    ));
    let draft = plugins::test(&ctx, "github", "", Some(TOKEN))?;
    assert_eq!(draft["error"]["kind"], "auth");
    assert!(!serde_json::to_string(&draft)?.contains(TOKEN));
    assert_eq!(plugin(&plugins::list(&ctx)?, "github")["badge"], "enabled");
    let saved = plugins::test(&ctx, "github", "", None)?;
    assert_eq!(saved["error"]["status"], 401);
    assert_eq!(plugin(&plugins::list(&ctx)?, "github")["badge"], "error");

    let unreachable = Ctx {
        api_base: Some("http://127.0.0.1:0".into()),
        ..ctx.clone()
    };
    assert_eq!(
        plugins::test(&unreachable, "github", "", None)?["error"]["kind"],
        "unreachable"
    );
    assert_eq!(plugin(&plugins::list(&ctx)?, "github")["badge"], "error");
    stub.reply(Reply::json(200, json!({"login":"octocat"})));
    assert_eq!(
        plugins::test(&ctx, "github", "", None)?["account"],
        "octocat"
    );
    assert_eq!(plugin(&plugins::list(&ctx)?, "github")["badge"], "enabled");
    assert_no_token(&ctx.home)?;
    Ok(())
}

#[test]
fn status_uses_local_state_and_prefers_a_matching_remote() -> Result<()> {
    let repo = Repo::new()?;
    let ctx = context(repo.home.clone(), None);
    plugins::set_enabled(&ctx, "github", true)?;
    let empty = plugins::remote_status(&ctx, &repo.project, repo.path())?;
    assert!(empty["remote"].is_null());
    assert!(
        empty["branches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|branch| branch["name"] == "main")
    );
    assert_eq!(
        plugins::refresh(&ctx, &repo.project, repo.path())?["refreshing"],
        false
    );
    assert!(!repo.home.join("plugins/remote").exists());

    repo.add_remote("origin", "https://gitlab.example.invalid/acme/demo.git")?;
    let status = plugins::remote_status(&ctx, &repo.project, repo.path())?;
    assert_eq!(status["remote"]["name"], "origin");
    assert!(status["remote"]["plugin"].is_null());
    repo.add_remote("team/upstream", ORIGIN)?;
    let matched = plugins::remote_status(&ctx, &repo.project, repo.path())?;
    assert_eq!(matched["remote"]["name"], "team/upstream");
    assert_eq!(matched["remote"]["plugin"], "github");
    assert_eq!(matched["remote"]["badge"], "needs_config");
    git(&repo.root, &["remote", "set-url", "origin", ORIGIN])?;
    let preferred = plugins::remote_status(&ctx, &repo.project, repo.path())?;
    assert_eq!(preferred["remote"]["name"], "origin");
    assert!(!repo.home.join("plugins/remote").exists());
    Ok(())
}

#[test]
fn refresh_fetches_once_keeps_last_results_and_respects_rate_limits() -> Result<()> {
    let repo = Repo::new()?;
    let work = api::create_work(
        &repo.home,
        &repo.project,
        "插件远端",
        "Software",
        false,
        &[repo.path().into()],
    )?;
    let stage = format!("aep-stage/{work}");
    git(&repo.root, &["branch", &stage])?;
    repo.add_remote("origin", ORIGIN)?;
    let upstream = "https://upstream.example.invalid/acme/demo.git";
    repo.add_remote("team/upstream", upstream)?;
    git(
        &repo.root,
        &["config", "branch.main.remote", "team/upstream"],
    )?;
    git(
        &repo.root,
        &["config", "branch.main.merge", "refs/heads/main"],
    )?;
    let rewrite = format!("url.{}.insteadOf", repo.bare.display());
    for remote_url in [ORIGIN, upstream] {
        git(&repo.root, &["config", "--add", &rewrite, remote_url])?;
    }
    let tip = git(
        &repo.bare,
        &[
            "-c",
            "user.name=Studio",
            "-c",
            "user.email=studio@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit-tree",
            "HEAD^{tree}",
            "-p",
            "HEAD",
            "-m",
            "incoming",
        ],
    )?
    .trim()
    .to_owned();
    git(&repo.bare, &["update-ref", "refs/heads/main", &tip])?;
    git(&repo.bare, &["update-ref", "refs/heads/remote-only", &tip])?;
    let before = git(&repo.root, &["for-each-ref", "refs/heads"])?;
    fs::write(repo.root.join("README.md"), b"unsaved local content\n")?;
    let before_status = git(&repo.root, &["status", "--porcelain"])?;

    let rows = json!([
        pull(10, &stage, "open", false, None, "2026-05-10T12:00:00Z"),
        pull(12, &stage, "open", false, None, "2026-05-13T12:00:00Z"),
        pull(20, "draft", "open", true, None, "2026-05-11T12:00:00Z"),
        pull(
            30,
            "merged",
            "closed",
            false,
            Some("2026-05-14T00:00:00Z"),
            "2026-05-10T12:00:00Z"
        ),
        pull(40, "closed", "closed", false, None, "2026-05-10T12:00:00Z"),
    ]);
    let stub = Stub::new(Reply::json(200, rows.clone()))?;
    let ctx = context(repo.home.clone(), Some(stub.base()));
    plugins::save(&ctx, "github", "", Some(TOKEN))?;
    plugins::set_enabled(&ctx, "github", true)?;
    let local = plugins::remote_status(&ctx, &repo.project, repo.path())?;
    assert_eq!(local["remotes"][0]["url"], ORIGIN);
    assert_eq!(local["pulls_checked"], false);
    assert!(stub.requests().is_empty());

    stub.hold.store(true, Ordering::Release);
    let first = plugins::refresh(&ctx, &repo.project, repo.path())?;
    assert_eq!(first["refreshing"], true);
    wait_for(|| stub.requests().len() == 1)?;
    let again = plugins::refresh(&ctx, &repo.project, repo.path())?;
    assert_eq!(again["refreshing"], true);
    assert_eq!(stub.requests().len(), 1);
    stub.hold.store(false, Ordering::Release);
    let fetched = repo.wait_idle(&ctx)?;
    assert_eq!(stub.requests(), vec![Request::new(PULLS, true, false)]);
    assert!(fetched["refreshed_at"].as_u64().is_some());
    assert_eq!(fetched["pulls_checked"], true);
    assert_eq!(fetched["pulls"][&stage]["number"], 12);
    assert_eq!(fetched["pulls"][&stage]["others"], 1);
    assert_eq!(
        fetched["pulls"][&stage]["url"],
        "https://github.com/acme/demo/pull/12"
    );
    assert_eq!(fetched["pulls"][&stage]["state"], "open");
    for state in ["draft", "merged", "closed"] {
        assert_eq!(fetched["pulls"][state]["state"], state);
    }
    let graph = api::branch_graph(&repo.home, &repo.project, repo.path(), 50)?;
    let labeled = graph["branches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == stage)
        .unwrap();
    assert_eq!(labeled["item"], json!({"id":work,"name":"插件远端"}));
    let main = fetched["branches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "main")
        .unwrap();
    assert_eq!(main["upstream"], "team/upstream/main");
    assert_eq!(main["behind"], 1);
    for remote in ["origin", "team/upstream"] {
        assert_eq!(
            git(
                &repo.root,
                &["rev-parse", &format!("refs/remotes/{remote}/main")]
            )?
            .trim(),
            tip
        );
    }
    assert_eq!(git(&repo.root, &["for-each-ref", "refs/heads"])?, before);
    assert_eq!(git(&repo.root, &["status", "--porcelain"])?, before_status);
    assert_eq!(
        fs::read(repo.root.join("README.md"))?,
        b"unsaved local content\n"
    );

    stub.reply(Reply::json(
        401,
        json!({"message": format!("bad credential {TOKEN}")}),
    ));
    let refreshing = plugins::refresh(&ctx, &repo.project, repo.path())?;
    assert!(refreshing["api_error"].is_null());
    let auth = repo.wait_idle(&ctx)?;
    assert_eq!(auth["api_error"]["kind"], "auth");
    assert_eq!(auth["pulls"], fetched["pulls"]);
    assert_eq!(plugin(&plugins::list(&ctx)?, "github")["badge"], "error");

    let reset = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() + 3600;
    stub.reply(Reply::Json {
        status: 429,
        headers: format!("X-RateLimit-Reset: {reset}\r\n"),
        body: json!({"message":"Try later"}),
    });
    plugins::refresh(&ctx, &repo.project, repo.path())?;
    let limited = repo.wait_idle(&ctx)?;
    assert_eq!(limited["api_error"]["kind"], "rate_limited");
    assert_eq!(limited["api_error"]["reset_at"], reset);
    assert_eq!(limited["pulls"], fetched["pulls"]);
    assert_eq!(stub.requests().len(), 3);
    stub.reply(Reply::json(200, rows.clone()));
    plugins::refresh(&ctx, &repo.project, repo.path())?;
    assert_eq!(repo.wait_idle(&ctx)?["api_error"]["kind"], "rate_limited");
    assert_eq!(stub.requests().len(), 3);

    let file = cache_path(&repo.home)?;
    let mut cached: Value = serde_json::from_slice(&fs::read(&file)?)?;
    assert_eq!(cached["repository"], repo.path());
    assert_eq!(cached["remote"], "origin");
    assert_eq!(cached["plugin"], "github");
    cached["api_error"]["reset_at"] = json!(0);
    fs::write(&file, serde_json::to_vec(&cached)?)?;
    stub.reply(Reply::Stall);
    plugins::refresh(&ctx, &repo.project, repo.path())?;
    let timeout = repo.wait_idle(&ctx)?;
    assert_eq!(timeout["api_error"]["kind"], "timeout");
    assert_eq!(timeout["pulls"], fetched["pulls"]);
    assert_eq!(timeout["branches"], fetched["branches"]);
    assert_eq!(stub.requests().len(), 4);

    stub.reply(Reply::json(200, rows));
    plugins::refresh(&ctx, &repo.project, repo.path())?;
    let recovered = repo.wait_idle(&ctx)?;
    assert!(recovered["api_error"].is_null());
    assert_eq!(plugin(&plugins::list(&ctx)?, "github")["badge"], "enabled");
    assert!(!serde_json::to_string(&auth)?.contains(TOKEN));
    assert!(!serde_json::to_string(&recovered)?.contains(TOKEN));
    assert_no_token(&repo.home)?;
    plugins::clear_token(&ctx, "github")?;
    assert_eq!(
        plugins::remote_status(&ctx, &repo.project, repo.path())?["pulls"],
        json!({})
    );
    Ok(())
}

#[test]
fn open_url_validation_requires_https() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = context(temp.path().to_owned(), None);
    for url in [
        "http://github.com",
        "file:///tmp/demo",
        "javascript:alert(1)",
        "https://",
    ] {
        assert!(plugins::open_url(&ctx, url).is_err(), "{url}");
    }
    let url = "https://github.com/acme/demo/pull/12?source=studio";
    assert_eq!(plugins::check_url(url)?.as_str(), url);
    Ok(())
}

#[test]
fn reveal_git_tools_config_uses_the_fixed_file_or_directory() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("AEP Catalog");
    let dir = home.join("plugins");
    let file = dir.join("git-tools.json");

    assert_eq!(
        plugins::git_tools_target(&home),
        (GitToolsTarget::Missing, dir.clone())
    );
    let ctx = context(home.clone(), None);
    let result = plugins::reveal_git_tools_config(&ctx)?;
    assert_eq!(serde_json::to_value(result)?, "missing");
    assert!(!home.exists());

    fs::create_dir_all(&dir)?;
    let (target, path) = plugins::git_tools_target(&home);
    assert_eq!(path, dir);
    assert_eq!(serde_json::to_value(target)?, "dir");

    fs::write(&file, "[]")?;
    let (target, path) = plugins::git_tools_target(&home);
    assert_eq!(path, file);
    assert_eq!(serde_json::to_value(target)?, "file");
    Ok(())
}

struct Repo {
    _temp: tempfile::TempDir,
    home: PathBuf,
    root: PathBuf,
    bare: PathBuf,
    project: String,
}

impl Repo {
    fn new() -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let home = temp.path().join("catalog");
        let root = temp.path().join("repo");
        fs::create_dir(&root)?;
        let root = root.canonicalize()?;
        let bare = temp.path().join("remote.git");
        git(&root, &["init", "-q", "-b", "main"])?;
        fs::write(root.join("README.md"), b"demo\n")?;
        git(&root, &["add", "README.md"])?;
        git(
            &root,
            &[
                "-c",
                "user.name=Studio",
                "-c",
                "user.email=studio@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "initial",
            ],
        )?;
        git(
            &root,
            &["clone", "--quiet", "--bare", ".", bare.to_str().unwrap()],
        )?;
        let project = api::create_project(&home, "demo", &[root.to_str().unwrap().into()])?;
        Ok(Self {
            _temp: temp,
            home,
            root,
            bare,
            project,
        })
    }

    fn path(&self) -> &str {
        self.root.to_str().unwrap()
    }

    fn add_remote(&self, name: &str, url: &str) -> Result<()> {
        git(&self.root, &["remote", "add", name, url])?;
        Ok(())
    }

    fn wait_idle(&self, ctx: &Ctx) -> Result<Value> {
        let start = Instant::now();
        loop {
            let status = plugins::remote_status(ctx, &self.project, self.path())?;
            if status["refreshing"] == false {
                return Ok(status);
            }
            ensure!(
                start.elapsed() < Duration::from_secs(25),
                "remote refresh stayed busy"
            );
            thread::sleep(Duration::from_millis(75));
        }
    }
}

fn wait_for(condition: impl Fn() -> bool) -> Result<()> {
    let start = Instant::now();
    while !condition() {
        ensure!(
            start.elapsed() < Duration::from_secs(8),
            "HTTP request did not arrive"
        );
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

fn assert_no_token(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            assert_no_token(&path)?;
        } else if path.is_file() {
            let data = fs::read(&path)?;
            ensure!(
                !data
                    .windows(TOKEN.len())
                    .any(|window| window == TOKEN.as_bytes()),
                "credential contained in {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn cache_path(home: &Path) -> Result<PathBuf> {
    let mut files = fs::read_dir(home.join("plugins/remote"))?;
    let file = files.next().context("remote cache was not saved")??.path();
    assert!(files.next().is_none());
    assert_eq!(
        file.extension().and_then(|part| part.to_str()),
        Some("json")
    );
    let key = file.file_stem().unwrap().to_str().unwrap();
    assert!(key.len() == 16 && key.bytes().all(|byte| byte.is_ascii_hexdigit()));
    Ok(file)
}

fn pull(
    number: u64,
    branch: &str,
    state: &str,
    draft: bool,
    merged_at: Option<&str>,
    date: &str,
) -> Value {
    json!({
        "number": number, "title": format!("PR {number}"), "state": state, "draft": draft,
        "merged_at": merged_at, "html_url": format!("https://github.com/acme/demo/pull/{number}"),
        "created_at": date, "head": { "ref": branch, "repo": { "full_name": "acme/demo" } },
    })
}

#[derive(Clone)]
enum Reply {
    Json {
        status: u16,
        headers: String,
        body: Value,
    },
    Stall,
}

impl Reply {
    fn json(status: u16, body: Value) -> Self {
        Self::Json {
            status,
            headers: String::new(),
            body,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Request {
    path: String,
    github_token: bool,
    gitlab_token: bool,
}

impl Request {
    fn new(path: &str, github_token: bool, gitlab_token: bool) -> Self {
        Self {
            path: path.into(),
            github_token,
            gitlab_token,
        }
    }

    fn read(stream: &mut TcpStream) -> Result<Self> {
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        let mut lines = BufReader::new(stream).lines();
        let line = lines.next().context("missing HTTP request")??;
        let mut parts = line.split_whitespace();
        ensure!(parts.next() == Some("GET"), "expected a read-only request");
        let mut request = Self::new(
            parts.next().context("missing HTTP request path")?,
            false,
            false,
        );
        for line in lines {
            let line = line?;
            if line.is_empty() {
                break;
            }
            request.github_token |= line.strip_prefix("Authorization: Bearer ") == Some(TOKEN);
            request.gitlab_token |= line.strip_prefix("PRIVATE-TOKEN: ") == Some(TOKEN);
        }
        Ok(request)
    }
}

struct Stub {
    base: String,
    response: Arc<Mutex<Reply>>,
    requests: Arc<Mutex<Vec<Request>>>,
    hold: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    server: Option<thread::JoinHandle<Result<()>>>,
}

impl Stub {
    fn new(reply: Reply) -> Result<Self> {
        let listener =
            TcpListener::bind("127.0.0.1:0").context("cannot bind the local forge stub")?;
        let base = format!("http://{}", listener.local_addr()?);
        listener.set_nonblocking(true)?;
        let response = Arc::new(Mutex::new(reply));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let hold = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let server = {
            let (response, requests, hold, stop) = (
                response.clone(),
                requests.clone(),
                hold.clone(),
                stop.clone(),
            );
            thread::spawn(move || -> Result<()> {
                let mut stalled = Vec::new();
                while !stop.load(Ordering::Acquire) {
                    let mut stream = match listener.accept() {
                        Ok((stream, _)) => stream,
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(err) => return Err(err.into()),
                    };
                    stream.set_nonblocking(false)?;
                    let request = Request::read(&mut stream)?;
                    requests.lock().unwrap().push(request);
                    while hold.load(Ordering::Acquire) && !stop.load(Ordering::Acquire) {
                        thread::sleep(Duration::from_millis(5));
                    }
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    match response.lock().unwrap().clone() {
                        Reply::Json {
                            status,
                            headers,
                            body,
                        } => {
                            let body = body.to_string();
                            write!(
                                stream,
                                "HTTP/1.1 {status} Stub\r\n{headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )?;
                        }
                        Reply::Stall => stalled.push(stream),
                    }
                }
                Ok(())
            })
        };
        Ok(Self {
            base,
            response,
            requests,
            hold,
            stop,
            server: Some(server),
        })
    }

    fn base(&self) -> String {
        self.base.clone()
    }

    fn reply(&self, reply: Reply) {
        *self.response.lock().unwrap() = reply;
    }

    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let outcome = self.server.take().unwrap().join();
        if !thread::panicking() {
            outcome.unwrap().expect("HTTP stub failed");
        }
    }
}
