use aep::{
    project::remote::{
        Remote, RemoteBranch, RemoteUrl, Upstream, fetch, remote_only, remotes, upstreams,
    },
    workspace::git,
};
use anyhow::{Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

struct Repo {
    _temp: tempfile::TempDir,
    bare: PathBuf,
    local: PathBuf,
    base: String,
}

impl Repo {
    fn new() -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let seed = temp.path().join("seed");
        let bare = temp.path().join("server.git");
        let local = temp.path().join("local");
        fs::create_dir(&seed)?;
        git(&seed, &["init", "--quiet", "--initial-branch=main"])?;
        fs::write(seed.join("tracked"), "base\n")?;
        git(&seed, &["add", "tracked"])?;
        commit(&seed, "base")?;
        let base = git(&seed, &["rev-parse", "HEAD"])?.trim().to_owned();
        git(&seed, &["clone", "--quiet", "--bare", ".", as_arg(&bare)?])?;
        git(
            temp.path(),
            &["clone", "--quiet", as_arg(&bare)?, as_arg(&local)?],
        )?;
        Ok(Self {
            _temp: temp,
            bare,
            local,
            base,
        })
    }

    fn publish_branch(&self, branch: &str, sha: &str) -> Result<()> {
        git(
            &self.bare,
            &["update-ref", &format!("refs/heads/{branch}"), sha],
        )?;
        Ok(())
    }

    fn remote_commit(&self) -> Result<String> {
        let sha = git(
            &self.bare,
            &[
                "-c",
                "user.name=Studio test",
                "-c",
                "user.email=studio@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit-tree",
                "HEAD^{tree}",
                "-p",
                &self.base,
                "-m",
                "remote",
            ],
        )?;
        Ok(sha.trim().to_owned())
    }
}

fn as_arg(path: &Path) -> Result<&str> {
    path.to_str().context("non-UTF-8 fixture path")
}

fn commit(root: &Path, subject: &str) -> Result<()> {
    git(
        root,
        &[
            "-c",
            "user.name=Studio test",
            "-c",
            "user.email=studio@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            subject,
        ],
    )?;
    Ok(())
}

#[test]
fn remote_config_keeps_raw_fetch_urls_and_supports_no_remotes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    git(root, &["init", "--quiet", "--initial-branch=main"])?;
    commit(root, "local only")?;
    assert!(remotes(root)?.is_empty());
    let ups = upstreams(root)?;
    assert_eq!(ups.len(), 1);
    assert_eq!(ups[0].upstream, None);
    assert!(remote_only(root, &ups)?.is_empty());

    let original = "https://GitHub.com/acme/demo.git";
    let rewritten = "/path with spaces/demo.git";
    git(root, &["remote", "add", "origin", original])?;
    git(
        root,
        &[
            "config",
            "--add",
            "remote.origin.url",
            "https://mirror.example.invalid/g/r.git",
        ],
    )?;
    git(
        root,
        &["config", &format!("url.{rewritten}.insteadOf"), original],
    )?;
    git(root, &["remote", "add", "team/upstream", rewritten])?;
    assert_eq!(
        git(root, &["remote", "get-url", "origin"])?.trim_end(),
        rewritten
    );
    assert_eq!(
        remotes(root)?,
        vec![
            Remote {
                name: "origin".into(),
                url: original.into(),
                parsed: Some(RemoteUrl {
                    host: "github.com".into(),
                    path: "acme/demo".into()
                }),
            },
            Remote {
                name: "team/upstream".into(),
                url: rewritten.into(),
                parsed: None
            },
        ]
    );
    Ok(())
}

#[test]
fn remote_upstreams_use_tracking_config_and_report_missing_or_untracked_refs() -> Result<()> {
    let repo = Repo::new()?;
    let root = &repo.local;
    let remote_tip = repo.remote_commit()?;
    repo.publish_branch("service/trunk", &remote_tip)?;
    repo.publish_branch("feature/同名", &repo.base)?;
    repo.publish_branch("only-remote", &repo.base)?;
    git(
        root,
        &["remote", "add", "team/upstream", as_arg(&repo.bare)?],
    )?;
    fetch(root, "team/upstream", FETCH_TIMEOUT)?;
    let target = "team/upstream/service/trunk";
    git(root, &["branch", "--set-upstream-to", target, "main"])?;
    git(root, &["branch", "--no-track", "feature/同名"])?;
    git(root, &["branch", "--no-track", target])?;
    git(root, &["branch", "--track", "equal", "origin/main"])?;
    commit(root, "local one")?;
    commit(root, "local two")?;

    let ups = upstreams(root)?;
    let get = |name| ups.iter().find(|up| up.branch == name).unwrap();
    assert_eq!(
        get("main"),
        &Upstream {
            branch: "main".into(),
            upstream: Some(target.into()),
            present: true,
            ahead: Some(2),
            behind: Some(1),
            same_name: None,
        }
    );
    assert_eq!(
        (get("equal").ahead, get("equal").behind),
        (Some(0), Some(0))
    );
    let same = get("feature/同名");
    assert_eq!(
        (
            same.upstream.as_deref(),
            same.present,
            same.ahead,
            same.behind
        ),
        (None, false, None, None)
    );
    assert_eq!(
        same.same_name.as_deref(),
        Some("team/upstream/feature/同名")
    );
    assert_eq!(
        remote_only(root, &ups)?,
        vec![RemoteBranch {
            name: "team/upstream/only-remote".into(),
            sha: repo.base.clone()
        }]
    );

    git(
        root,
        &["update-ref", "-d", &format!("refs/remotes/{target}")],
    )?;
    let missing = upstreams(root)?
        .into_iter()
        .find(|up| up.branch == "main")
        .unwrap();
    assert_eq!(missing.upstream.as_deref(), Some(target));
    assert_eq!(
        (
            missing.present,
            missing.ahead,
            missing.behind,
            missing.same_name
        ),
        (false, None, None, None)
    );
    Ok(())
}

#[test]
fn remote_fetch_refreshes_and_prunes_only_the_selected_tracking_refs() -> Result<()> {
    let repo = Repo::new()?;
    let root = &repo.local;
    let remote_tip = repo.remote_commit()?;
    repo.publish_branch("main", &remote_tip)?;
    repo.publish_branch("new", &remote_tip)?;
    git(&repo.bare, &["tag", "incoming", &remote_tip])?;

    git(
        root,
        &["update-ref", "refs/remotes/origin/stale", &repo.base],
    )?;
    git(
        root,
        &["update-ref", "refs/remotes/other/stale", &repo.base],
    )?;
    git(root, &["tag", "local-only"])?;
    git(
        root,
        &[
            "config",
            "--add",
            "remote.origin.fetch",
            "+refs/heads/*:refs/heads/mapped/*",
        ],
    )?;
    git(root, &["config", "remote.origin.pruneTags", "true"])?;
    fs::write(root.join("tracked"), "staged change\n")?;
    git(root, &["add", "tracked"])?;
    fs::write(root.join("tracked"), "unstaged change\n")?;
    fs::write(root.join("untracked"), "untracked change\n")?;
    let fetch_head = root.join(".git/FETCH_HEAD");
    fs::write(&fetch_head, "existing fetch record\n")?;

    let other_refs = || -> Result<Vec<String>> {
        Ok(
            git(root, &["for-each-ref", "--format=%(refname) %(objectname)"])?
                .lines()
                .filter(|line| !line.starts_with("refs/remotes/origin/"))
                .map(str::to_owned)
                .collect(),
        )
    };
    let refs = other_refs()?;
    let status = git(root, &["status", "--porcelain"])?;
    let config = git(root, &["config", "--local", "--null", "--list"])?;
    fetch(root, "origin", FETCH_TIMEOUT)?;

    assert_eq!(other_refs()?, refs);
    assert_eq!(git(root, &["status", "--porcelain"])?, status);
    assert_eq!(
        git(root, &["config", "--local", "--null", "--list"])?,
        config
    );
    assert_eq!(
        fs::read_to_string(root.join("tracked"))?,
        "unstaged change\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("untracked"))?,
        "untracked change\n"
    );
    assert_eq!(fs::read_to_string(fetch_head)?, "existing fetch record\n");
    assert_eq!(
        git(root, &["rev-parse", "refs/remotes/origin/main"])?.trim(),
        remote_tip
    );
    assert_eq!(
        git(root, &["rev-parse", "refs/remotes/origin/new"])?.trim(),
        remote_tip
    );
    let tracking = git(
        root,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/remotes/origin/",
        ],
    )?;
    assert!(
        !tracking
            .lines()
            .any(|name| name == "refs/remotes/origin/stale")
    );
    Ok(())
}

#[test]
fn remote_fetch_reports_authentication_failure_without_prompting_or_config_changes() -> Result<()> {
    let repo = Repo::new()?;
    let root = &repo.local;
    configure_test_transport(root)?;
    fs::write(
        root.join("test-transport"),
        "test \"$GIT_TERMINAL_PROMPT\" = 0 || exit 2\n\
         test -z \"${AEP_CONTROL_OPERATOR_TOKEN:-}${AEP_CONTROL_DEVICE_TOKEN:-}\" || exit 3\n\
         printf '%s\\n' 'transport diagnostic' 'fatal: could not read Username: terminal prompts disabled' >&2\n\
         exit 1\n",
    )?;
    let config = git(root, &["config", "--local", "--null", "--list"])?;
    let error = fetch(root, "origin", FETCH_TIMEOUT).unwrap_err();
    assert!(error.auth, "{error}");
    assert_eq!(error.message, "transport diagnostic");
    assert_eq!(
        git(root, &["config", "--local", "--null", "--list"])?,
        config
    );
    Ok(())
}

#[test]
fn remote_fetch_times_out_after_a_short_limit() -> Result<()> {
    let repo = Repo::new()?;
    let root = &repo.local;
    configure_test_transport(root)?;
    fs::write(root.join("test-transport"), "exec sleep 30\n")?;
    let start = Instant::now();
    let error = fetch(root, "origin", Duration::from_millis(150)).unwrap_err();
    assert!(!error.auth);
    assert!(error.message.contains("timed out"), "{error}");
    assert!(start.elapsed() < Duration::from_secs(5));
    assert_eq!(
        git(root, &["rev-parse", "refs/remotes/origin/main"])?.trim(),
        repo.base
    );
    Ok(())
}

fn configure_test_transport(root: &Path) -> Result<()> {
    let original = "https://example.invalid/group/repo.git";
    git(root, &["remote", "set-url", "origin", original])?;
    git(
        root,
        &[
            "config",
            "url.ssh://git@example.invalid/group/repo.git.insteadOf",
            original,
        ],
    )?;
    git(root, &["config", "core.sshCommand", "sh ./test-transport"])?;
    git(root, &["config", "ssh.variant", "simple"])?;
    Ok(())
}
