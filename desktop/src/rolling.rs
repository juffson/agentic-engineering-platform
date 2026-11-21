use crate::{
    Runner,
    https_pilot::{Connection, Progress, preview, read_json},
    https_queue::site_status,
    rolling_gate::Gate,
};
use aep::{delivery::slug, workspace::Workspace};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::{self, File},
    io::Write,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
};

fn command(runner: &Runner, program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .env("PATH", runner.path()?)
        .args(args)
        .output()
        .with_context(|| format!("无法启动 {program}"))?;
    ensure!(
        output.status.success(),
        "{program}：{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8(output.stdout)?)
}

fn valid_site(site: &str) -> bool {
    (2..=40).contains(&site.len())
        && site.as_bytes()[0].is_ascii_lowercase()
        && !site.ends_with('-')
        && site
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

pub(crate) fn sites(runner: &Runner) -> Result<Vec<String>> {
    let output = command(
        runner,
        "docker",
        &[
            "ps",
            "--filter",
            "label=org.aep.pilot",
            "--format",
            "{{.Names}}\t{{.Label \"org.aep.pilot\"}}",
        ],
    )?;
    let mut sites: Vec<_> = output
        .lines()
        .filter_map(|line| {
            let (container, name) = line.split_once('\t')?;
            (valid_site(name) && container == format!("{name}-control")).then(|| name.to_owned())
        })
        .collect();
    sites.sort();
    Ok(sites)
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Site {
    pub name: String,
    pub endpoint: String,
    pub access: PathBuf,
    pub repository: String,
    pub workers: Vec<String>,
}

pub(crate) fn probe(
    source: &Workspace,
    runner: &Runner,
    site: String,
    access: PathBuf,
) -> Result<Site> {
    ensure!(
        valid_site(&site) && sites(runner)?.contains(&site),
        "找不到已启动的站点"
    );
    let access = access.canonicalize().context("访问目录不存在")?;
    let ca = access.join("ca.pem");
    let operator = access.join("operator");
    ensure!(
        ca.is_file() && operator.is_file(),
        "访问目录需要 ca.pem 和 operator"
    );
    let port = command(
        runner,
        "docker",
        &["port", &format!("{site}-control"), "8443/tcp"],
    )?;
    let port = port
        .trim()
        .strip_prefix("127.0.0.1:")
        .and_then(|number| number.parse::<u16>().ok())
        .context("站点没有唯一的本机 HTTPS 端口")?;
    let endpoint = format!("https://127.0.0.1:{port}");
    let connection = Connection {
        endpoint: endpoint.clone(),
        ca: Some(ca),
        operator_file: operator,
    };
    let snapshot = site_status(source, &connection, runner)?;
    ensure!(
        snapshot.deploy_keys.len() == 2,
        "该演练需要站点配置两台独立使用 Deploy Key 的 Worker"
    );
    let repository = snapshot
        .deploy_keys
        .values()
        .next()
        .context("没有私有仓库")?
        .repository
        .clone();
    ensure!(
        slug(&format!("https://github.com/{repository}"))? == repository
            && snapshot.belongs_to(&repository)
            && snapshot.deploy_keys.iter().all(|(id, keys)| {
                snapshot.workers.get(id).is_some_and(|worker| worker.online)
                    && keys.inventory.active.is_some()
            }),
        "两台 Worker 需在线并为同一仓库配置生效密钥"
    );
    let visibility = command(
        runner,
        "gh",
        &[
            "repo",
            "view",
            &repository,
            "--json",
            "isPrivate",
            "--jq",
            ".isPrivate",
        ],
    )?;
    ensure!(visibility.trim() == "true", "演练需要私有 GitHub 仓库");
    Ok(Site {
        name: site,
        endpoint,
        access,
        repository,
        workers: snapshot.deploy_keys.into_keys().collect(),
    })
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub(crate) struct Status {
    pub stage: String,
    pub reason: String,
    pub exit_code: Option<i32>,
}

#[derive(Deserialize)]
struct Task {
    session: PathBuf,
    task: String,
}

pub(crate) struct Delivery {
    pub role: &'static str,
    pub progress: Progress,
    pub checks: Vec<Value>,
    evidence: PathBuf,
}

impl Delivery {
    fn load(root: &Path, role: &'static str) -> Option<Self> {
        let task: Task = read_json(root.join(format!("delivery-{role}.json")))?;
        aep::workspace::ident(&task.task, 63).ok()?;
        let parent = root
            .join("checkout/.git/aep/https-studio")
            .canonicalize()
            .ok()?;
        let path = task.session.canonicalize().ok()?;
        if path.parent()? != parent {
            return None;
        }
        let progress: Progress = read_json(path.join("state.json"))?;
        if progress.task != task.task {
            return None;
        }
        let evidence = path.join(task.task).join("evidence");
        let checks: Value = read_json(evidence.join("github-ci.json")).unwrap_or_default();
        Some(Self {
            role,
            progress,
            checks: checks["statusCheckRollup"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
            evidence,
        })
    }

    pub fn proofs(&self) -> Vec<(&'static str, PathBuf)> {
        let mut paths = vec![
            ("候选 Diff", self.evidence.join("candidate.diff")),
            ("CI 验收日志", self.evidence.join("github-acceptance.log")),
            ("合并回执", self.evidence.join("github-merged.json")),
            ("清理回执", self.evidence.join("cleanup.json")),
        ];
        let run = &self.progress.recovered_run;
        if aep::workspace::ident(run, 64).is_ok() {
            let run = self.evidence.join("worker/runs").join(run);
            paths.push(("验收日志", run.join("checks.log")));
            paths.push(("Claude 审查", run.join("review.txt")));
        }
        paths.retain(|(_, path)| path.is_file());
        paths
    }
}

pub(crate) struct Session {
    pub root: PathBuf,
    pub source: PathBuf,
    pub site: Site,
    pub status: Status,
    pub log: String,
    pub cutover_error: Option<String>,
    pub deliveries: Vec<Delivery>,
    pub shots: Vec<(String, PathBuf)>,
    pub selected: Option<PathBuf>,
    pub acknowledged: Option<Gate>,
    pending: Option<Gate>,
    submitted: Option<Gate>,
    child: Option<Child>,
    approval: Option<ChildStdin>,
}

impl Session {
    fn from_disk(root: PathBuf, source: &Workspace, site: Site) -> Self {
        Self {
            root,
            source: source.root.clone(),
            site,
            status: Status::default(),
            log: String::new(),
            cutover_error: None,
            deliveries: Vec::new(),
            shots: Vec::new(),
            selected: None,
            acknowledged: None,
            pending: None,
            submitted: None,
            child: None,
            approval: None,
        }
    }

    pub fn launch(source: &Workspace, runner: &Runner, site: Site) -> Result<Self> {
        let script = source.root.join("deploy/pilot/rolling-keys.sh");
        ensure!(
            script.is_file()
                && script
                    .canonicalize()?
                    .starts_with(source.root.canonicalize()?),
            "请打开包含 deploy/pilot/rolling-keys.sh 的 AEP 源码仓库"
        );
        let parent = source.data.join("https-rolling-studio");
        fs::create_dir_all(&parent)?;
        let root = tempfile::Builder::new()
            .prefix("studio-")
            .tempdir_in(parent.canonicalize()?)?
            .keep();
        serde_json::to_writer(File::create(root.join("site.json"))?, &site)?;
        let log = File::create(root.join("rolling.log"))?;
        let mut cmd = Command::new("bash");
        cmd.current_dir(&source.root)
            .env("PATH", runner.path()?)
            .env("GH_PROMPT_DISABLED", "1")
            .arg(script)
            .arg("--studio-session")
            .arg(&root)
            .args([
                "--site",
                &site.name,
                "--repo",
                &site.repository,
                "--access-dir",
            ])
            .arg(&site.access);
        for worker in &site.workers {
            cmd.args(["--worker", worker]);
        }
        let mut child = cmd
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .spawn()?;
        let mut session = Self::from_disk(root, source, site);
        session.approval = child.stdin.take();
        session.child = Some(child);
        Ok(session)
    }

    pub fn latest(source: &Workspace) -> Option<Self> {
        let mut roots: Vec<_> = fs::read_dir(source.data.join("https-rolling-studio"))
            .ok()?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|typ| typ.is_dir()))
            .collect();
        roots.sort_by_cached_key(|entry| {
            std::cmp::Reverse(entry.metadata().and_then(|m| m.modified()).ok())
        });
        roots.into_iter().find_map(|entry| {
            let root = entry.path();
            let site = read_json(root.join("site.json"))?;
            let mut session = Self::from_disk(root, source, site);
            session.refresh();
            Some(session)
        })
    }

    pub fn active(&self) -> bool {
        self.child.is_some()
    }

    pub fn gate(&self) -> Option<&Gate> {
        self.approval.as_ref()?;
        self.pending
            .as_ref()
            .filter(|pending| Some(*pending) != self.submitted.as_ref())
    }

    pub fn confirm(&mut self) -> Result<()> {
        let shown = self.gate().context("当前没有可确认的操作")?.clone();
        ensure!(
            self.acknowledged.as_ref() == Some(&shown)
                && read_json::<Gate>(self.root.join("gate.json")).as_ref() == Some(&shown),
            "确认内容已更新，请重新核对"
        );
        let pipe = self.approval.as_mut().context("确认通道已关闭")?;
        pipe.write_all(shown.command().as_bytes())
            .context("无法发送确认")?;
        self.acknowledged = None;
        self.submitted = Some(shown);
        Ok(())
    }

    pub fn stop_at_gate(&mut self) {
        if self.gate().is_some() {
            self.approval.take();
            self.acknowledged = None;
        }
    }

    pub fn archive(&self) -> Option<PathBuf> {
        let path = self.root.join("evidence.tar.gz");
        path.is_file().then_some(path)
    }

    pub fn refresh(&mut self) {
        if let Some(status) = read_json(self.root.join("rolling-state.json")) {
            self.status = status;
        }
        if let Some(result) = self
            .child
            .as_mut()
            .and_then(|child| child.try_wait().transpose())
        {
            self.child = None;
            self.approval = None;
            if self.status.exit_code.is_none() {
                self.status.exit_code =
                    Some(result.ok().and_then(|status| status.code()).unwrap_or(1));
            }
        }
        self.log = preview(&self.root.join("rolling.log"), true).unwrap_or_default();
        self.deliveries = ["peer", "switched"]
            .into_iter()
            .filter_map(|role| Delivery::load(&self.root, role))
            .collect();
        let gate = read_json::<Gate>(self.root.join("gate.json")).filter(|gate| {
            gate.valid()
                && gate.repository == self.site.repository
                && self.site.workers.contains(&gate.worker)
        });
        if self.pending != gate {
            self.acknowledged = None;
        }
        self.pending = gate;
        let proof = self.root.join("evidence");
        if self.cutover_error.is_none() {
            let failure: Value = read_json(proof.join("after-fault.json")).unwrap_or_default();
            if let Some(worker) = self.site.workers.first() {
                self.cutover_error = failure["data"]["deploy_keys"][worker]["last"]["error"]
                    .as_str()
                    .map(str::to_owned);
            }
        }
        self.shots = screenshots(&proof);
    }
}

fn screenshots(proof: &Path) -> Vec<(String, PathBuf)> {
    let mut files = Vec::new();
    for (step, name, label) in [
        (
            "simulate-failed-cutover",
            "02-roll-failed-key-preserved.png",
            "切换失败与旧密钥",
        ),
        (
            "retry-or-switch-0",
            "03-1-active-verified.png",
            "第一台重试验证",
        ),
        ("peer", "05-peer-merge.png", "故障期间的 CI 与合并"),
        ("peer", "06-peer-archive.png", "故障期间任务归档"),
        ("switched", "05-peer-merge.png", "切换后的 CI 与合并"),
        ("switched", "06-peer-archive.png", "切换后任务归档"),
        (
            "confirm-retirement-1",
            "03-retired-and-verified.png",
            "轮换完成",
        ),
    ] {
        if let Some(path) = fs::read_dir(proof.join(step))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|typ| typ.is_dir()))
            .map(|entry| entry.path().join(name))
            .find(|path| path.is_file())
        {
            files.push((label.into(), path));
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rolling_gate::Action;

    #[test]
    fn approval_requires_a_current_acknowledged_gate_and_is_submitted_once() -> Result<()> {
        let scratch = tempfile::tempdir()?;
        let source = Workspace {
            root: scratch.path().into(),
            data: scratch.path().into(),
        };
        let site = Site {
            name: "pilot".into(),
            endpoint: "https://127.0.0.1:18443".into(),
            access: scratch.path().into(),
            repository: "owner/private".into(),
            workers: vec!["pilot-worker".into(), "pilot-worker-two".into()],
        };
        let mut session = Session::from_disk(scratch.path().into(), &source, site);
        let mut child = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        session.approval = child.stdin.take();
        let gate = Gate {
            id: uuid::Uuid::new_v4().to_string(),
            worker: "pilot-worker".into(),
            repository: "owner/private".into(),
            action: Action::Publish {
                target: "test/https-rolling-run".into(),
                candidate: "a".repeat(40),
            },
        };
        let path = scratch.path().join("gate.json");
        serde_json::to_writer(File::create(&path)?, &gate)?;
        session.refresh();
        assert!(session.confirm().is_err());

        session.acknowledged = Some(gate.clone());
        let mut changed = gate.clone();
        changed.action = Action::Publish {
            target: "test/https-rolling-run".into(),
            candidate: "b".repeat(40),
        };
        serde_json::to_writer(File::create(&path)?, &changed)?;
        assert!(session.confirm().is_err());
        session.refresh();
        assert!(session.acknowledged.is_none());

        session.acknowledged = Some(changed.clone());
        session.confirm()?;
        assert!(session.confirm().is_err());
        session.approval.take();
        assert_eq!(
            child.wait_with_output()?.stdout,
            changed.command().as_bytes()
        );
        Ok(())
    }
}
