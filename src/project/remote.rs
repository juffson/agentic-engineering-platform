//! Local remote-tracking state and an explicit, bounded refresh for Studio.
use anyhow::{Context, Result, bail};
use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    io::{BufRead, BufReader, Seek},
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteUrl {
    pub host: String,
    pub path: String,
}

pub fn parse_url(url: &str) -> Option<RemoteUrl> {
    let (host, path) = if url.contains("://") {
        let url = Url::parse(url).ok()?;
        if !matches!(url.scheme(), "https" | "http" | "ssh" | "git") {
            return None;
        }
        (url.host_str()?.to_ascii_lowercase(), url.path().to_owned())
    } else {
        // In Git's scp syntax the colon must precede any slash in the authority.
        let (prefix, _) = url.split_once(':')?;
        let separator = if prefix.rsplit('@').next()?.starts_with('[') {
            url.find("]:")? + 1
        } else {
            prefix.len()
        };
        let (authority, path) = url.split_at(separator);
        let path = path.strip_prefix(':')?;
        if authority.contains(['/', '\\', '?', '#'])
            || authority.contains(char::is_whitespace)
            || (authority.len() == 1 && path.starts_with(['/', '\\']))
        {
            return None;
        }
        let host = Url::parse(&format!("ssh://{authority}")).ok()?;
        (host.host_str()?.to_ascii_lowercase(), path.to_owned())
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    (!path.is_empty()).then(|| RemoteUrl {
        host,
        path: path.to_owned(),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub url: String,
    pub parsed: Option<RemoteUrl>,
}

pub fn remotes(root: &Path) -> Result<Vec<Remote>> {
    let output = git_command(root)
        .args(["config", "--null", "--get-regexp", r"^remote\..*\.url$"])
        .output()
        .context("read git remote configuration")?;
    if output.status.code() == Some(1) && output.stderr.is_empty() {
        return Ok(Vec::new());
    }
    if !output.status.success() {
        bail!(
            "git config: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let mut seen = HashSet::new();
    let config = String::from_utf8(output.stdout)?;
    let mut remotes = Vec::new();
    for record in config.split_terminator('\0') {
        let (key, url) = record
            .split_once('\n')
            .context("invalid git config record")?;
        let name = key
            .strip_prefix("remote.")
            .and_then(|key| key.strip_suffix(".url"))
            .context("invalid git remote key")?;
        // Git fetch uses the first URL when a remote has multiple URLs.
        if seen.insert(name) {
            remotes.push(Remote {
                name: name.to_owned(),
                url: url.to_owned(),
                parsed: parse_url(url),
            });
        }
    }
    Ok(remotes)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upstream {
    pub branch: String,
    pub upstream: Option<String>,
    pub present: bool,
    pub ahead: Option<u64>,
    pub behind: Option<u64>,
    pub same_name: Option<String>,
}

pub fn upstreams(root: &Path) -> Result<Vec<Upstream>> {
    let refs = git(
        root,
        &[
            "for-each-ref",
            "--format=%(refname:strip=2)%1f%(upstream)%1f%(upstream:short)%1f%(upstream:track,nobracket)",
            "refs/heads/",
        ],
    )?;
    let mut upstreams = Vec::new();
    for line in refs.lines() {
        let fields: Vec<_> = line.split('\x1f').collect();
        let [branch, full, short, track] = fields.as_slice() else {
            bail!("invalid git upstream record");
        };
        let counts = match (*full, *track) {
            ("", _) | (_, "gone") => None,
            _ => Some(match track_counts(track) {
                Some(counts) => counts,
                None => {
                    let range = format!("refs/heads/{branch}...{full}");
                    let counts = git(root, &["rev-list", "--left-right", "--count", &range, "--"])?;
                    let (ahead, behind) = counts
                        .trim()
                        .split_once('\t')
                        .context("invalid git counts")?;
                    (ahead.parse()?, behind.parse()?)
                }
            }),
        };
        upstreams.push(Upstream {
            branch: (*branch).to_owned(),
            upstream: (!full.is_empty()).then(|| {
                full.strip_prefix("refs/remotes/")
                    .unwrap_or(short)
                    .to_owned()
            }),
            present: counts.is_some(),
            ahead: counts.map(|(ahead, _)| ahead),
            behind: counts.map(|(_, behind)| behind),
            same_name: None,
        });
    }

    if upstreams.iter().any(|up| up.upstream.is_none()) {
        let tracking = tracking_branches(root)?;
        let mut by_branch = HashMap::new();
        for (remote, branch) in &tracking {
            by_branch.entry(branch.as_str()).or_insert(&remote.name);
        }
        for up in upstreams.iter_mut().filter(|up| up.upstream.is_none()) {
            up.same_name = by_branch
                .get(up.branch.as_str())
                .map(|name| (*name).clone());
        }
    }
    Ok(upstreams)
}

fn track_counts(track: &str) -> Option<(u64, u64)> {
    if track.is_empty() {
        return Some((0, 0));
    }
    if let Some((ahead, behind)) = track.split_once(", ") {
        return Some((
            ahead.strip_prefix("ahead ")?.parse().ok()?,
            behind.strip_prefix("behind ")?.parse().ok()?,
        ));
    }
    if let Some(ahead) = track.strip_prefix("ahead ") {
        return Some((ahead.parse().ok()?, 0));
    }
    Some((0, track.strip_prefix("behind ")?.parse().ok()?))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteBranch {
    pub name: String,
    pub sha: String,
}

fn tracking_branches(root: &Path) -> Result<Vec<(RemoteBranch, String)>> {
    let refs = git(
        root,
        &[
            "for-each-ref",
            "--sort=refname",
            "--format=%(refname:strip=2)%1f%(objectname)%1f%(symref)",
            "refs/remotes/",
        ],
    )?;
    if refs.is_empty() {
        return Ok(Vec::new());
    }
    let remotes = git(root, &["remote"])?;
    let mut branches = Vec::new();
    for line in refs.lines() {
        let fields: Vec<_> = line.split('\x1f').collect();
        let [name, sha, symref] = fields.as_slice() else {
            bail!("invalid git remote branch record");
        };
        if !symref.is_empty() {
            continue;
        }
        let branch = remotes
            .lines()
            .filter(|remote| {
                name.strip_prefix(remote)
                    .is_some_and(|rest| rest.starts_with('/'))
            })
            .max_by_key(|remote| remote.len())
            .and_then(|remote| name[remote.len()..].strip_prefix('/'))
            .or_else(|| name.split_once('/').map(|(_, branch)| branch));
        if let Some(branch) = branch {
            branches.push((
                RemoteBranch {
                    name: (*name).to_owned(),
                    sha: (*sha).to_owned(),
                },
                branch.to_owned(),
            ));
        }
    }
    Ok(branches)
}

pub fn remote_only(root: &Path, ups: &[Upstream]) -> Result<Vec<RemoteBranch>> {
    let local: HashSet<_> = ups.iter().map(|up| up.branch.as_str()).collect();
    let tracked: HashSet<_> = ups.iter().filter_map(|up| up.upstream.as_deref()).collect();
    Ok(tracking_branches(root)?
        .into_iter()
        .filter(|(remote, branch)| {
            !local.contains(branch.as_str()) && !tracked.contains(remote.name.as_str())
        })
        .map(|(remote, _)| remote)
        .collect())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchError {
    pub message: String,
    pub auth: bool,
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FetchError {}

fn git_command(root: &Path) -> Command {
    let mut git = Command::new("git");
    git.arg("-C")
        .arg(root)
        .env("LC_ALL", "C")
        .env_remove(crate::CONTROL_OPERATOR_TOKEN)
        .env_remove(crate::CONTROL_DEVICE_TOKEN);
    git
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = git_command(root).args(args).output().context("start git")?;
    if !output.status.success() {
        bail!(
            "git {}: {}",
            args[0],
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)?)
}

pub fn fetch(root: &Path, remote: &str, timeout: Duration) -> Result<(), FetchError> {
    let error = |message: String| FetchError {
        message,
        auth: false,
    };
    if !remotes(root)
        .map_err(|err| error(err.to_string()))?
        .iter()
        .any(|configured| configured.name == remote)
    {
        return Err(error(format!("unknown git remote: {remote}")));
    }

    // An anonymous file lets Git report stderr without filling a pipe while we wait.
    let mut stderr =
        tempfile::tempfile().map_err(|err| error(format!("capture git fetch stderr: {err}")))?;
    let capture = stderr
        .try_clone()
        .map_err(|err| error(format!("capture git fetch stderr: {err}")))?;
    let refspec = format!("+refs/heads/*:refs/remotes/{remote}/*");
    let mut child = git_command(root)
        .args([
            "fetch",
            "--atomic",
            "--prune",
            "--no-tags",
            "--no-prune-tags",
            "--no-write-fetch-head",
            "--no-recurse-submodules",
            "--no-auto-gc",
            "--no-write-commit-graph",
            "--refmap=",
            "--quiet",
            "--",
            remote,
            &refspec,
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(capture)
        .process_group(0)
        .spawn()
        .map_err(|err| error(format!("start git fetch: {err}")))?;

    let timeout = timeout.min(Duration::from_secs(60));
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(err) => {
                stop_fetch(&mut child);
                return Err(error(format!("wait for git fetch: {err}")));
            }
        }
        let remaining = timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            stop_fetch(&mut child);
            return Err(error(format!("git fetch timed out after {timeout:?}")));
        }
        thread::sleep(remaining.min(Duration::from_millis(20)));
    };
    if status.success() {
        return Ok(());
    }

    let read_error = |err| error(format!("read git fetch stderr: {err}"));
    stderr.rewind().map_err(read_error)?;
    let mut message = None;
    let mut auth = false;
    for line in BufReader::new(stderr).split(b'\n') {
        let line = line.map_err(read_error)?;
        let line = String::from_utf8_lossy(&line);
        if message.is_none() && !line.trim().is_empty() {
            message = Some(line.trim().to_owned());
        }
        let lower = line.to_ascii_lowercase();
        auth |= [
            "authentication failed",
            "permission denied",
            "could not read username",
            "could not read password",
        ]
        .iter()
        .any(|marker| lower.contains(marker));
    }
    Err(FetchError {
        message: message.unwrap_or_else(|| format!("git fetch failed: {status}")),
        auth,
    })
}

fn stop_fetch(child: &mut Child) {
    if killpg(Pid::from_raw(child.id() as i32), Signal::SIGKILL).is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_url_parses_git_transports_and_nested_groups() {
        for (url, host, path) in [
            ("https://github.com/o/r", "github.com", "o/r"),
            ("https://GitHub.com:8443/o/r.git", "github.com", "o/r"),
            ("git@github.com:o/r.git", "github.com", "o/r"),
            (
                "ssh://git@gitlab.example.com:2222/g/sub/r.git",
                "gitlab.example.com",
                "g/sub/r",
            ),
            (
                "https://gitlab.example.com/g/sub/r/",
                "gitlab.example.com",
                "g/sub/r",
            ),
            ("git://host/g/r.git/", "host", "g/r"),
            ("http://host/g/r.git", "host", "g/r"),
            ("git@[::1]:g/r.git", "[::1]", "g/r"),
        ] {
            assert_eq!(
                parse_url(url),
                Some(RemoteUrl {
                    host: host.into(),
                    path: path.into()
                }),
                "{url}"
            );
        }
        for url in [
            "/tmp/g/r.git",
            "../g/r.git",
            "./g:x/r",
            "file:///tmp/g/r.git",
            "https://",
            "C:\\repos\\r.git",
        ] {
            assert_eq!(parse_url(url), None, "{url}");
        }
    }

    #[test]
    fn remote_track_counts_cover_git_output() {
        for (track, counts) in [
            ("", Some((0, 0))),
            ("ahead 2", Some((2, 0))),
            ("behind 1", Some((0, 1))),
            ("ahead 2, behind 1", Some((2, 1))),
            ("gone", None),
            ("unexpected", None),
        ] {
            assert_eq!(track_counts(track), counts);
        }
    }
}
