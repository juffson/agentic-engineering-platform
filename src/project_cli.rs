//! The project workflow entry point used from shells and existing agent sessions.
use crate::workflow_stage;
use aep::project::directory::Binding;
use aep::{
    cancel::Cancellation,
    handoff,
    memory::{Key, Kind as MemoryKind, Memory},
    project::{self, HttpsSite, Kind, Library, Project, RecordKind, Work, save_json},
    workflow::{self, Client, ExternalSession, Request, Template},
};
use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

mod submission;
#[derive(Args)]
pub struct Options {
    /// Shared Studio catalog. Defaults to AEP_PROJECT_HOME or the Studio application directory.
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    /// Project name or ID. Use an ID in long-lived automation.
    #[arg(long, global = true)]
    project: Option<String>,
    /// Work item name or ID.
    #[arg(long, global = true)]
    work: Option<String>,
    #[command(subcommand)]
    action: Action,
}
#[derive(Args)]
struct Routing {
    #[arg(long, value_enum)]
    client: Option<Client>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    executable: Option<String>,
    #[arg(long)]
    repository: Option<PathBuf>,
    #[arg(long)]
    check_command: Option<String>,
}
#[derive(Subcommand)]
enum Action {
    /// List providers, models and reasoning levels available to the local ZCode app.
    ZcodeModels,
    /// Show or update this project's ZCode default.
    ZcodeDefault {
        #[arg(long, conflicts_with = "clear")]
        model: Option<String>,
        #[arg(long)]
        clear: bool,
    },
    /// Create a Studio project and optionally associate local Git repositories.
    Create {
        name: String,
        #[arg(long)]
        repository: Vec<PathBuf>,
    },
    /// List projects and their work items.
    List,
    /// Bind this local Git worktree to a Studio project and item.
    Bind,
    /// Select the project for Studio and subsequent interactive CLI calls.
    Open,
    /// Associate a repository with a project or explicitly selected item.
    AddRepo {
        path: PathBuf,
    },
    /// Exclude a repository from the selected item, or unlink an unused project repository.
    RemoveRepo {
        path: PathBuf,
    },
    /// Associate an existing local task ticket with this item's repository.
    LinkTicket {
        #[arg(long)]
        repository: PathBuf,
        ticket: String,
    },
    /// Create a feature or release using a built-in template.
    AddWork {
        name: String,
        #[arg(long, value_enum, default_value = "software")]
        template: Template,
        #[arg(long)]
        release: bool,
    },
    /// Send or update a short handoff to another item in this project.
    Handoff {
        #[arg(long)]
        to: String,
        #[arg(long)]
        note: String,
    },
    /// Link an existing repository relay or remote HTTPS session to this work item.
    LinkRecord {
        #[arg(long)]
        repository: PathBuf,
        #[arg(value_enum)]
        kind: RecordKind,
        id: String,
    },
    /// Remove a work item reference while keeping the repository archive.
    UnlinkRecord {
        #[arg(long)]
        repository: PathBuf,
        #[arg(value_enum)]
        kind: RecordKind,
        id: String,
    },
    /// Read or configure the project HTTPS connection for a repository.
    Site {
        #[arg(long)]
        repository: PathBuf,
        #[arg(long, requires = "operator_file", conflicts_with = "clear")]
        endpoint: Option<String>,
        #[arg(long, requires = "endpoint")]
        operator_file: Option<PathBuf>,
        #[arg(long, requires = "endpoint")]
        ca: Option<PathBuf>,
        #[arg(long)]
        clear: bool,
    },
    Status,
    /// Configure a stage without starting a model. Stage numbers start at 1.
    Configure {
        #[arg(long)]
        stage: usize,
        #[command(flatten)]
        routing: Routing,
        #[arg(long)]
        note: Option<String>,
    },
    /// Run the configured local model and wait for its receipt.
    Run {
        #[command(flatten)]
        routing: Routing,
        #[arg(long)]
        brief: Option<String>,
    },
    /// Register an existing client session and return its compact task packet (no model call).
    Claim {
        /// Expected current stage, starting at 1.
        #[arg(long)]
        stage: usize,
        #[arg(long)]
        session: String,
        #[arg(long, default_value = ".")]
        workspace: PathBuf,
        #[command(flatten)]
        routing: Routing,
        #[arg(long)]
        brief: Option<String>,
        #[arg(long)]
        retry: bool,
    },
    /// Preview a stage without claiming it, or refresh a claim by attempt. Since returns project-memory changes.
    Context {
        #[arg(long)]
        attempt: Option<String>,
        #[arg(long)]
        since: Option<i64>,
    },
    /// Publish a result and optional evidence files from the claimed workspace. Read stdin with --file -.
    Submit {
        #[arg(long)]
        attempt: String,
        #[arg(long)]
        session: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        summary: String,
        #[arg(long)]
        artifact: Vec<PathBuf>,
        #[arg(long)]
        blocked: bool,
    },
    Cancel {
        #[arg(long)]
        attempt: String,
    },
    /// Confirm the expected stage after reviewing its result or manual note.
    Advance {
        #[arg(long)]
        stage: usize,
        #[arg(long)]
        note: Option<String>,
    },
    Reopen {
        #[arg(long)]
        stage: usize,
    },
    /// Read an attempt's receipt or a full artifact on demand.
    Show {
        /// Stream a submitted evidence file by its workspace-relative name.
        #[arg(long)]
        artifact: Option<PathBuf>,
        #[arg(long)]
        attempt: String,
        #[arg(long,default_value="receipt",value_parser=["receipt","result","request","prompt","stdout","stderr","files"])]
        part: String,
    },
    /// Report AEP input bytes, elapsed time and available provider counters per attempt.
    Metrics,
    Memory {
        #[command(subcommand)]
        operation: Notes,
    },
}
#[derive(Subcommand)]
enum Notes {
    List {
        #[arg(long)]
        since: Option<i64>,
    },
    Get {
        id: i64,
    },
    Add {
        #[arg(long, value_enum, default_value = "decision")]
        kind: MemoryKind,
        text: String,
    },
    Revise {
        id: i64,
        #[arg(long, value_enum, default_value = "decision")]
        kind: MemoryKind,
        text: String,
    },
}
fn print(v: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}
fn root(path: &Path) -> Result<PathBuf> {
    Ok(aep::workspace::Workspace::open(path)?.root.canonicalize()?)
}
fn find<'a, T>(items: &'a [T], key: &str, identity: impl Fn(&T) -> (&str, &str)) -> Result<&'a T> {
    if let Some(t) = items.iter().find(|x| identity(x).0 == key) {
        return Ok(t);
    }
    let mut hits = items.iter().filter(|x| identity(x).1 == key);
    let t = hits.next().with_context(|| format!("找不到 {key}"))?;
    ensure!(hits.next().is_none(), "名称不唯一，请使用 ID");
    Ok(t)
}
fn resolve<'a>(lib: &'a Library, p: Option<&str>) -> Result<&'a Project> {
    find(
        &lib.projects,
        p.or(lib.active.as_deref()).context("需要选择项目")?,
        |p| (&p.id, &p.name),
    )
}
fn item<'a>(p: &'a Project, w: Option<&str>) -> Result<&'a Work> {
    find(
        &p.work,
        w.or(p.selected.as_deref()).context("需要选择事项")?,
        |w| (&w.id, &w.name),
    )
}
fn project_mut<'a>(lib: &'a mut Library, id: &str) -> Result<&'a mut Project> {
    lib.projects
        .iter_mut()
        .find(|p| p.id == id)
        .context("项目已改变")
}
fn index(stage: usize, w: &Work) -> Result<usize> {
    ensure!((1..=w.template.stages().len()).contains(&stage), "无效阶段");
    Ok(stage - 1)
}
fn profile(w: &Work, i: usize, r: &Routing) -> workflow::Profile {
    let mut p = w.profile(i);
    if let Some(client) = r.client
        && client != p.client
    {
        p.client = client;
        p.model.clear();
        p.executable.clear();
        p.check_command.clear();
    }
    if let Some(v) = &r.model {
        p.model = v.clone();
    }
    if let Some(v) = &r.executable {
        p.executable = v.clone();
    }
    if let Some(v) = &r.check_command {
        p.check_command = v.clone();
    }
    p
}
fn target(
    w: &Work,
    i: usize,
    r: &Routing,
    external: Option<&ExternalSession>,
) -> Result<Option<PathBuf>> {
    if let Some(path) = &r.repository {
        let repo = root(path)?;
        ensure!(w.repositories.contains(&repo), "仓库未关联到此事项");
        return Ok(Some(repo));
    }
    if let Some(session) = external {
        if let Ok(current) = aep::workspace::Workspace::open(&session.workspace) {
            for path in &w.repositories {
                if aep::workspace::Workspace::open(path).is_ok_and(|repo| repo.data == current.data)
                {
                    return Ok(Some(path.clone()));
                }
            }
        }
        return Ok(None);
    }
    Ok(w.target(i))
}
fn resolve_attempt(
    dir: &Path,
    lib: &Library,
    p: Option<&str>,
    w: Option<&str>,
    id: &str,
) -> Result<PathBuf> {
    aep::workspace::ident(id, 64)?;
    for project in &lib.projects {
        if p.is_some_and(|s| s != project.id && s != project.name) {
            continue;
        }
        for work in &project.work {
            if w.is_some_and(|s| s != work.id && s != work.name) {
                continue;
            }
            if work.attempts.iter().any(|a| a.id == id) {
                return Ok(workflow::attempt_dir(dir, &project.id, &work.id, id));
            }
        }
    }
    bail!("未找到该范围内的执行 {id}")
}
fn request(dir: &Path) -> Result<Request> {
    Ok(serde_json::from_reader(fs::File::open(
        dir.join("request.json"),
    )?)?)
}
fn external_context(dir: &Path, since: Option<i64>) -> Result<Value> {
    let work = dir
        .parent()
        .and_then(Path::parent)
        .context("missing work directory")?;
    let id = dir.file_name().unwrap().to_str().unwrap();
    let cancel = Cancellation::new(work, id)?;
    let _lock = cancel.lock()?;
    let req = request(dir)?;
    let session = req.external.as_ref().context("此执行由 Studio 托管")?;
    let mut ev = workflow::evidence(dir)?;
    ensure!(
        ev["status"] == "claimed",
        "领取已结束，使用 project show 查看结果"
    );
    let packet = workflow_stage::context(&req, dir, &session.workspace, since)?;
    if !dir.join("prompt.txt").exists() {
        fs::write(dir.join("prompt.txt"), &packet.text)?;
    }
    for (key, ids) in [
        ("read_memory", &packet.project_ids),
        ("read_repository_memory", &packet.repo_ids),
    ] {
        let mut all = ev[key].as_array().cloned().unwrap_or_default();
        for id in ids {
            if !all.contains(&json!(id)) {
                all.push(json!(id));
            }
        }
        ev[key] = json!(all);
    }
    ev["prompt_bytes"] = json!(dir.join("prompt.txt").metadata()?.len());
    save_json(dir, "evidence.json", &ev)?;
    Ok(
        json!({"attempt":id,"status":"claimed","session":session.session,"workspace":session.workspace,"stage":ev["stage"],"context":packet.text,"next_since":packet.project_ids.iter().max().copied().or(since),"has_more":packet.more,"read_memory":packet.project_ids,"read_repository_memory":packet.repo_ids,"request_file":dir.join("request.json")}),
    )
}
fn view(work: &Work, project: &Project, dir: &Path) -> Result<Value> {
    let https: Vec<_> = work
        .records
        .iter()
        .filter(|r| r.kind == RecordKind::Https)
        .map(|r| r.kind.summary(&r.repository, &r.id))
        .collect();
    let mut attempts = vec![];
    for a in &work.attempts {
        let d = workflow::attempt_dir(dir, &project.id, &work.id, &a.id);
        let e = workflow::evidence(&d)?;
        attempts.push(json!({"id":a.id,"stage":a.stage+1,"status":e["status"],"session":e["session"],"client":a.profile.client,"model":a.profile.model,"error":e["error"]}));
    }
    Ok(
        json!({"project":project.id,"work":work.id,"name":work.name,"template":work.template,"stage_name":work.stage_name(),"stage":(work.stage<work.notes.len()).then_some(work.stage+1),"repositories":work.repositories,"records":work.records,"https":https,"handoffs":work.handoffs,"stages":work.template.stages().iter().enumerate().map(|(i,s)|json!({"stage":i+1,"name":s.name,"profile":work.profile(i),"attempt":work.current_attempt.get(&i)})).collect::<Vec<_>>(),"attempts":attempts}),
    )
}
struct Catalog {
    dir: PathBuf,
    library: Library,
    project: Option<String>,
    work: Option<String>,
}
impl Catalog {
    fn project(&self) -> Result<&Project> {
        resolve(&self.library, self.project.as_deref())
    }
    fn work(&self) -> Result<&Work> {
        item(self.project()?, self.work.as_deref())
    }
    fn attempt(&self, id: &str) -> Result<PathBuf> {
        resolve_attempt(
            &self.dir,
            &self.library,
            self.project.as_deref(),
            self.work.as_deref(),
            id,
        )
    }
    fn edit_project<T>(&mut self, edit: impl FnOnce(&mut Project) -> Result<T>) -> Result<T> {
        let id = self.project()?.id.clone();
        self.library
            .change(&self.dir, |lib| edit(project_mut(lib, &id)?))
    }
    fn edit_work<T>(&mut self, edit: impl FnOnce(&mut Work) -> Result<T>) -> Result<T> {
        let wid = self.work()?.id.clone();
        self.edit_project(|p| edit(p.work_by_id_mut(&wid)?))
    }
    fn reserve(
        &mut self,
        routing: &Routing,
        brief: Option<String>,
        external: Option<ExternalSession>,
        retry: bool,
    ) -> Result<PathBuf> {
        let work = self.work()?;
        let wid = work.id.clone();
        let i = work.stage;
        ensure!(i < work.template.stages().len(), "事项已完成");
        if !retry
            && let Some(ext) = &external
            && let Some(id) = work.current_attempt.get(&i)
        {
            let dir = self.attempt(id)?;
            let req = request(&dir)?;
            if req.external.as_ref() == Some(ext) {
                ensure!(
                    routing.client.is_none_or(|c| c == req.profile.client)
                        && routing
                            .model
                            .as_ref()
                            .is_none_or(|m| m == &req.profile.model),
                    "会话已绑定另一配置"
                );
                return Ok(dir);
            }
        }
        let dir = self.dir.clone();
        self.edit_project(|p| {
            let w = p.work_by_id_mut(&wid)?;
            let profile = profile(w, i, routing);
            let repository = target(w, i, routing, external.as_ref())?;
            let text = brief.unwrap_or_else(|| w.notes[i].clone());
            p.reserve(
                &dir,
                &wid,
                i,
                handoff::Assignment {
                    profile,
                    repository,
                    brief: text,
                    external,
                },
            )
        })
    }
    fn advance(&mut self, stage: usize, note: Option<String>) -> Result<()> {
        let w = self.work()?;
        let i = index(stage, w)?;
        ensure!(i == w.stage, "阶段已推进或尚未到达");
        let mut text = note.unwrap_or_else(|| w.notes[i].clone());
        if let Some(id) = w.current_attempt.get(&i) {
            let dir = self.attempt(id)?;
            ensure!(
                workflow::evidence(&dir)?["status"] == "succeeded",
                "执行尚未通过"
            );
            if text.trim().is_empty() {
                text = fs::read_to_string(dir.join("result.txt"))?;
            }
        }
        ensure!(!text.trim().is_empty(), "请提供人工阶段结论");
        self.edit_work(|w| {
            w.notes[i] = text;
            w.stage += 1;
            Ok(())
        })?;
        print(&json!({"confirmed_stage":stage}))
    }
    fn show(&self, id: &str, part: &str, artifact: Option<PathBuf>) -> Result<()> {
        let dir = self.attempt(id)?;
        let ev = workflow::evidence(&dir)?;
        let path = if let Some(file) = artifact {
            ensure!(
                ev["artifacts"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|x| x["path"].as_str() == file.to_str())),
                "回执中没有此文件"
            );
            dir.join("submission/files").join(file)
        } else {
            match part {
                "receipt" => return print(&ev),
                "files" => return print(&json!({"directory":dir,"submitted":ev["artifacts"]})),
                "result" => dir.join("result.txt"),
                "request" => dir.join("request.json"),
                "prompt" => dir.join("prompt.txt"),
                "stdout" => dir.join("stdout.log"),
                _ => dir.join("stderr.log"),
            }
        };
        io::copy(
            &mut fs::File::open(&path).with_context(|| format!("读取 {}", path.display()))?,
            &mut io::stdout(),
        )?;
        Ok(())
    }
}
pub fn execute(opt: Options, cwd: &Path) -> Result<()> {
    let file = Binding::file(cwd);
    let binding = file.as_deref().map(Binding::read).transpose()?.flatten();
    let explicit_home = opt
        .home
        .or_else(|| std::env::var_os("AEP_PROJECT_HOME").map(PathBuf::from));
    let dir = std::path::absolute(
        explicit_home
            .or_else(|| binding.as_ref().map(|b| b.home.clone()))
            .or_else(project::catalog_dir)
            .context("找不到项目库")?,
    )?;
    let binding = binding.filter(|b| b.home == dir);
    let project = opt.project.or_else(|| std::env::var("AEP_PROJECT").ok());
    let bound_work = binding
        .as_ref()
        .filter(|b| project.as_deref().is_none_or(|s| s == b.project))
        .map(|b| b.work.clone());
    let project = project.or_else(|| binding.as_ref().map(|b| b.project.clone()));
    let mut c = Catalog {
        library: Library::open(&dir)?,
        dir,
        project,
        work: opt
            .work
            .or_else(|| std::env::var("AEP_WORK").ok())
            .or(bound_work),
    };
    match opt.action {
        Action::ZcodeModels => print(&aep::zcode_catalog::discover(
            &std::env::var_os("PATH").unwrap_or_default(),
        )?),
        Action::ZcodeDefault { model, clear } => {
            if let Some(model) = model {
                let catalog =
                    aep::zcode_catalog::discover(&std::env::var_os("PATH").unwrap_or_default())?;
                ensure!(
                    catalog.iter().any(|m| m.accepts(&model)),
                    "本机 ZCode 未提供此模型或档位"
                );
                c.edit_project(|p| {
                    p.set_zcode_default(model);
                    Ok(())
                })?;
            } else if clear {
                c.edit_project(|p| {
                    p.set_zcode_default(String::new());
                    Ok(())
                })?;
            }
            print(&json!({"project":c.project()?.id,"model":c.project()?.zcode_default()}))
        }
        Action::List => {
            let mut projects = vec![];
            for p in &c.library.projects {
                let mut items = vec![];
                for w in &p.work {
                    let current=w.current_attempt.get(&w.stage).map(|id|{
                        let e=workflow::evidence(&workflow::attempt_dir(&c.dir,&p.id,&w.id,id))?;
                        Ok::<_,anyhow::Error>(json!({"attempt":id,"status":e["status"],"client":e["profile"]["client"],"session":e["session"]}))
                    }).transpose()?;
                    items.push(
                        json!({"id":w.id,"name":w.name,"stage":w.stage_name(),"current":current}),
                    );
                }
                projects.push(
                    json!({"id":p.id,"name":p.name,"repositories":p.repositories,"items":items}),
                );
            }
            print(&projects)
        }
        Action::Create { name, repository } => {
            let mut repos = vec![];
            for path in repository {
                let repo = root(&path)?;
                if !repos.contains(&repo) {
                    repos.push(repo);
                }
            }
            c.library.change(&c.dir, |l| l.create(&name, repos))?;
            print(&json!({"project":c.library.active}))
        }
        Action::Bind => {
            let file = file.context("请在 Git 工作区中绑定")?;
            let b = Binding {
                home: c.dir.clone(),
                project: c.project()?.id.clone(),
                work: c.work()?.id.clone(),
            };
            let dir = file.parent().unwrap();
            fs::create_dir_all(dir)?;
            save_json(dir, "project-binding.json", &b)?;
            print(&b)
        }
        Action::Open => {
            let pid = c.project()?.id.clone();
            let wid = c
                .work
                .as_deref()
                .map(|_| c.work().map(|w| w.id.clone()))
                .transpose()?;
            c.library.change(&c.dir, |lib| {
                lib.active = Some(pid.clone());
                if let Some(wid) = wid {
                    project_mut(lib, &pid)?.selected = Some(wid);
                }
                Ok(())
            })?;
            print(&json!({"project":pid}))
        }
        Action::AddRepo { path } => {
            let repo = root(&path)?;
            let wid = c
                .work
                .as_deref()
                .map(|_| c.work().map(|w| w.id.clone()))
                .transpose()?;
            c.edit_project(|p| {
                if !p.repositories.contains(&repo) {
                    p.repositories.push(repo.clone());
                }
                if let Some(wid) = wid {
                    p.work_by_id_mut(&wid)?.include(&repo);
                }
                Ok(())
            })?;
            print(&repo)
        }
        Action::RemoveRepo { path } => {
            let repo = root(&path)?;
            let wid = c
                .work
                .as_deref()
                .map(|_| c.work().map(|w| w.id.clone()))
                .transpose()?;
            c.edit_project(|p| {
                if let Some(wid) = wid {
                    p.work_by_id_mut(&wid)?.repositories.retain(|r| r != &repo);
                } else {
                    ensure!(
                        !p.work.iter().any(|w| w.repositories.contains(&repo)),
                        "请先从事项中移出仓库"
                    );
                    p.repositories.retain(|r| r != &repo);
                    p.https_sites.remove(&repo);
                }
                Ok(())
            })?;
            print(&repo)
        }
        Action::LinkTicket { repository, ticket } => {
            let repo = aep::workspace::Workspace::open(&repository)?;
            aep::workspace::ident(&ticket, 63)?;
            ensure!(
                repo.data
                    .join("local-tasks")
                    .join(format!("{ticket}.json"))
                    .is_file(),
                "仓库中找不到票据"
            );
            let root = repo.root.canonicalize()?;
            c.edit_work(|w| {
                ensure!(w.repositories.contains(&root), "仓库未关联到此事项");
                w.tickets.insert(root, ticket.clone());
                Ok(())
            })?;
            print(&json!({"ticket":ticket}))
        }
        Action::AddWork {
            name,
            template,
            release,
        } => {
            let kind = if release {
                Kind::Release
            } else {
                Kind::Feature
            };
            c.edit_project(|p| p.add_work(&name, kind, template))?;
            print(&json!({"project":c.project()?.id,"work":c.project()?.selected}))
        }
        Action::Handoff { to, note } => {
            let source = c.work()?.id.clone();
            let to = item(c.project()?, Some(&to))?.id.clone();
            c.edit_project(|p| p.handoff(&source, &to, &note))?;
            print(&json!({"from": source, "to": to}))
        }
        Action::LinkRecord {
            repository,
            kind,
            id,
        } => {
            let ws = aep::workspace::Workspace::open(&repository)?;
            kind.verify(&ws, &id)?;
            let work = c.work()?.id.clone();
            c.edit_project(|p| p.link_record(&work, &ws.root, kind, &id))?;
            print(&json!({"kind":kind, "id": id}))
        }
        Action::UnlinkRecord {
            repository,
            kind,
            id,
        } => {
            let repo = root(&repository)?;
            c.edit_work(|w| w.unlink_record(&repo, kind, &id))?;
            print(&json!({"unlinked":id}))
        }
        Action::Site {
            repository,
            endpoint,
            operator_file,
            ca,
            clear,
        } => {
            let root = root(&repository)?;
            ensure!(
                c.project()?.repositories.contains(&root),
                "仓库未关联到此项目"
            );
            if clear || endpoint.is_some() {
                let site = endpoint
                    .zip(operator_file)
                    .map(|(endpoint, operator_file)| HttpsSite {
                        endpoint,
                        ca,
                        operator_file,
                    });
                c.edit_project(|p| p.configure_site(&root, site))?;
            }
            print(&c.project()?.https_sites.get(&root))
        }
        Action::Status => print(&view(c.work()?, c.project()?, &c.dir)?),
        Action::Metrics => {
            let rows = c
                .work()?
                .attempts
                .iter()
                .map(|a| workflow_stage::metrics(&c.attempt(&a.id)?))
                .collect::<Result<Vec<_>>>()?;
            print(&rows)
        }
        Action::Configure {
            stage,
            routing,
            note,
        } => {
            c.edit_work(|w| {
                let i = index(stage, w)?;
                let profile = profile(w, i, &routing);
                profile.validate()?;
                if let Some(repo) = target(w, i, &routing, None)? {
                    w.targets.insert(i, repo);
                }
                w.profiles.insert(i, profile);
                if let Some(note) = note {
                    w.notes[i] = note;
                }
                Ok(())
            })?;
            print(&json!({"saved_stage":stage}))
        }
        Action::Advance { stage, note } => c.advance(stage, note),
        Action::Reopen { stage } => {
            let w = c.work()?;
            let i = index(stage, w)?;
            ensure!(i < w.stage, "只能回到已确认的阶段");
            if let Some(a) = w.attempts.last() {
                ensure!(
                    !handoff::busy(&workflow::evidence(&c.attempt(&a.id)?)?),
                    "当前执行仍在进行"
                );
            }
            c.edit_work(|w| {
                w.stage = i;
                w.current_attempt.retain(|j, _| *j < i);
                Ok(())
            })?;
            print(&json!({"stage":stage}))
        }
        Action::Run { routing, brief } => {
            let dir = c.reserve(&routing, brief, None, true)?;
            let result = workflow_stage::execute(&dir.join("request.json"));
            print(&workflow::evidence(&dir)?)?;
            result
        }
        Action::Claim {
            stage,
            session,
            workspace,
            routing,
            brief,
            retry,
        } => {
            let w = c.work()?;
            ensure!(index(stage, w)? == w.stage, "请求的阶段不是当前阶段");
            ensure!(
                !session.trim().is_empty() && session.len() <= 256,
                "无效会话 ID"
            );
            let workspace = workspace.canonicalize()?;
            ensure!(workspace.is_dir(), "工作目录不存在");
            let dir = c.reserve(
                &routing,
                brief,
                Some(ExternalSession { session, workspace }),
                retry,
            )?;
            let ev = workflow::evidence(&dir)?;
            if ev["status"] == "claimed" {
                print(&external_context(&dir, None)?)
            } else {
                print(&ev)
            }
        }
        Action::Context {
            attempt: Some(id),
            since,
        } => print(&external_context(&c.attempt(&id)?, since)?),
        Action::Context {
            attempt: None,
            since,
        } => {
            let w = c.work()?;
            let i = w.stage;
            ensure!(i < w.notes.len(), "事项已完成，可按执行 ID 查看归档");
            let p = c.project()?;
            let wd = c.dir.join("workflows").join(&p.id).join(&w.id);
            let req = w.request(
                &p.name,
                i,
                handoff::Assignment {
                    profile: w.profile(i),
                    brief: w.notes[i].clone(),
                    repository: w.target(i),
                    external: None,
                },
            );
            let packet = workflow_stage::preview(&req, &wd, cwd, since)?;
            print(
                &json!({"project":p.id,"work":w.id,"stage":i+1,"context":packet.text,"read_memory":packet.project_ids,"read_repository_memory":packet.repo_ids,"has_more":packet.more,"next_since":packet.project_ids.iter().max().copied().or(since)}),
            )
        }
        Action::Submit {
            attempt,
            session,
            file,
            summary,
            artifact,
            blocked,
        } => print(&submission::finish(
            &c.attempt(&attempt)?,
            &session,
            &file,
            &summary,
            &artifact,
            blocked,
        )?),
        Action::Cancel { attempt } => {
            let dir = c.attempt(&attempt)?;
            handoff::cancel(&dir)?;
            print(&workflow::evidence(&dir)?)
        }
        Action::Show {
            attempt,
            part,
            artifact,
        } => c.show(&attempt, &part, artifact),
        Action::Memory { operation } => {
            let dir = c.dir.join("workflows").join(&c.project()?.id);
            let mut db = Memory::open(&dir)?;
            match operation {
                Notes::Get { id } => print(&db.get(id)?),
                Notes::List { since } => print(
                    &Memory::browse(&dir)?
                        .into_iter()
                        .filter(|v| since.is_none_or(|n| v["id"].as_i64().unwrap_or_default() > n))
                        .collect::<Vec<_>>(),
                ),
                Notes::Add { kind, text } => {
                    print(&json!({"id":db.add(&Key::new("","",""),kind.as_str(),&text,None)?}))
                }
                Notes::Revise { id, kind, text } => print(&json!({"id":db.revise(id,kind,&text)?})),
            }
        }
    }
}
