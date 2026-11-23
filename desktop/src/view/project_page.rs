use super::*;
mod agents;
mod collaboration;
mod console;
mod delivery;
mod merge;
mod timeline;
mod zcode;
mod zcode_history;
use aep::project::{Kind, Library, Project, Work};
use aep::workflow::{self, Client, Profile, Role as StageRole, Template};
use anyhow::{Context as _, ensure};
pub(in crate::view) use collaboration::SiteFields;
use collaboration::{InfraView, SiteEditor, WorkDesk};
use gpui_kit::component::input::{Textarea, TextareaState};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Library,
    Overview,
    Work,
    Repositories,
    Settings,
    Memory,
}
pub(crate) struct Board {
    pub(super) library: Library,
    zcode_catalog: Option<Result<Vec<aep::zcode_catalog::Model>, String>>,
    zcode_loading: bool,
    zcode_draft: Option<(String, String)>,
    zcode_history: Option<zcode_history::History>,
    timeline: Option<timeline::State>,
    site_editor: Option<SiteEditor>,
    infra_view: Option<InfraView>,
    desk: Option<WorkDesk>,
    delivery: Option<delivery::State>,
    pick_record: bool,
    dir: Option<PathBuf>,
    section: Section,
    new_project: Entity<InputState>,
    new_work: Entity<InputState>,
    kind: Kind,
    creating_work: bool,
    repo_path: Entity<InputState>,
    note: Entity<TextareaState>,
    note_key: Option<(String, usize)>,
    viewing: usize,
    filter_stage: Option<(Template, usize)>,
    template: Template,
    client: Client,
    model: Entity<InputState>,
    executable: Entity<InputState>,
    check_command: Entity<InputState>,
    polling: bool,
    catalog_polling: bool,
    log_view: Option<(String, &'static str, u64)>,
    memories: Vec<Value>,
    editing_memory: Option<i64>,
    memory_note: Entity<TextareaState>,
    pub(crate) error: Option<String>,
    sync_message: Option<String>,
    statuses: BTreeMap<PathBuf, String>,
    generation: u64,
    chat: Entity<TextareaState>,
    /// Manual open/closed state of the stage CLI settings, per (work, stage).
    config_open: Option<(String, usize, bool)>,
    new_work_repos: Vec<PathBuf>,
    switcher: bool,
    transcripts: console::Cache,
    scrolls: console::Scrolls,
    /// Folded conversation blocks the user opened.
    expanded: std::collections::BTreeSet<String>,
    waive: Entity<InputState>,
}
impl Board {
    pub(crate) fn library_project_id(&self) -> Option<&String> {
        self.library.project().map(|p| &p.id)
    }
    pub(crate) fn load(
        ws: Option<&Workspace>,
        remember: bool,
        window: &mut Window,
        cx: &mut Context<Studio>,
    ) -> Self {
        let dir = if remember {
            crate::recent::storage()
        } else {
            ws.map(|w| w.data.join("studio-projects"))
        };
        let mut error = None;
        let library = dir
            .as_ref()
            .map_or_else(|| Ok(Library::default()), |d| Library::open(d))
            .unwrap_or_else(|e| {
                error = Some(format!("{e:#}"));
                Library::default()
            });
        let section = if library.project().is_some() {
            Section::Overview
        } else {
            Section::Library
        };
        Self {
            library,
            zcode_catalog: None,
            zcode_loading: false,
            zcode_draft: None,
            zcode_history: None,
            timeline: None,
            site_editor: None,
            infra_view: None,
            desk: None,
            delivery: None,
            pick_record: false,
            dir,
            section,
            new_project: cx.new(|cx| InputState::new(window, cx).placeholder("项目名称")),
            new_work: cx.new(|cx| InputState::new(window, cx).placeholder("功能名称或版本号")),
            kind: Kind::Feature,
            creating_work: false,
            repo_path: cx.new(|cx| InputState::new(window, cx).placeholder("本地 Git 仓库路径")),
            note: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .placeholder("给阶段 Agent 的任务说明，或人工确认的结论…")
            }),
            note_key: None,
            viewing: 0,
            filter_stage: None,
            template: Template::Software,
            client: Client::Claude,
            model: cx
                .new(|cx| InputState::new(window, cx).placeholder("模型 ID · 留空使用 CLI 默认")),
            check_command: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("仅本阶段允许 Claude 执行的验收命令（可选）")
            }),
            executable: cx
                .new(|cx| InputState::new(window, cx).placeholder("可执行文件路径 · 默认自动查找")),
            polling: false,
            catalog_polling: false,
            log_view: None,
            memories: vec![],
            editing_memory: None,
            memory_note: cx.new(|cx| {
                TextareaState::new(window, cx).placeholder("写下对项目内所有阶段有效的决定…")
            }),
            error,
            sync_message: None,
            statuses: BTreeMap::new(),
            generation: 0,
            chat: cx.new(|cx| {
                TextareaState::new(window, cx).placeholder("继续追问、补充要求或纠正方向…")
            }),
            config_open: None,
            new_work_repos: vec![],
            switcher: false,
            transcripts: Default::default(),
            scrolls: Default::default(),
            expanded: Default::default(),
            waive: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("人工放行理由，如：沙箱禁止监听端口，已在本机手动跑通全量测试")
            }),
        }
    }
}
fn basename(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}
fn primary(
    id: impl Into<ElementId>,
    title: impl Into<SharedString>,
) -> ObservedElement<Stateful<Div>> {
    let title = title.into();
    div()
        .id(id)
        .test_support()
        .role(Role::Button)
        .aria_label(title.clone())
        .tab_stop(true)
        .cursor_pointer()
        .rounded_md()
        .text_sm()
        .child(title)
        .px_4()
        .py_2()
        .bg(rgb(MINT))
        .text_color(rgb(0x102920))
        .font_semibold()
        .hover(|s| s.bg(rgb(0xb3ebd8)))
}
fn input_field(id: &'static str, field: &Entity<InputState>) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .child(div().id(id).test_support().child(Input::new(field)))
}

impl Studio {
    pub(crate) fn open_project_repository(
        &mut self,
        ws: Workspace,
        action: aep::project::directory::DirectoryAction,
        w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let default_home = if self.remember_repository {
            crate::recent::storage()
        } else {
            self.board.dir.clone()
        };
        let explicit_home = std::env::var_os("AEP_PROJECT_HOME").map(PathBuf::from);
        let result = default_home.context("找不到项目库目录").and_then(|dir| {
            aep::project::directory::DirectoryProject::resolve(
                &ws,
                &dir,
                explicit_home.as_deref(),
                action,
            )
        });
        match result {
            Ok(target) => {
                self.board.dir = Some(target.home);
                self.board.library = target.library;
                self.board.note_key = None;
                self.board.desk = None;
                self.board.log_view = None;
                self.set_workspace(ws, Page::Project, w, cx);
                self.section(
                    if target.bound_work {
                        Section::Work
                    } else {
                        Section::Overview
                    },
                    w,
                    cx,
                );
                self.sync_feedback();
            }
            Err(e) => {
                self.board.sync_message = None;
                self.board.error = Some(format!("{e:#}"));
            }
        }
        cx.notify();
    }
    fn sync_feedback(&mut self) {
        self.board.sync_message = Some(match self.board.library.project() {
            Some(p) => format!("已同步 · {} · {} 个开发事项", p.name, p.work.len()),
            None => "已同步项目库".into(),
        });
    }
    fn sync_project(&mut self, w: &mut Window, cx: &mut Context<Self>) {
        let result = self
            .board
            .dir
            .as_deref()
            .context("找不到项目库目录")
            .and_then(Library::open);
        self.board.error = None;
        match result {
            Ok(latest) => {
                if latest.revision != self.board.library.revision {
                    self.accept_catalog(latest, w, cx);
                }
                if self.board.section == Section::Work {
                    self.refresh_work(cx);
                } else if self.board.section == Section::Memory {
                    self.refresh_project_memory(cx);
                }
                self.watch_agents(cx);
                self.sync_feedback();
            }
            Err(e) => {
                self.board.sync_message = None;
                self.board.error = Some(format!("{e:#}"));
            }
        }
        cx.notify();
    }
    fn change_board(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut Library) -> Result<()>,
    ) -> bool {
        let revision = self.board.library.revision;
        let result = self
            .board
            .dir
            .as_ref()
            .context("找不到项目库目录")
            .and_then(|dir| self.board.library.change(dir, change));
        if result.is_err() && revision != self.board.library.revision {
            self.board.section = Section::Library;
            self.board.note_key = None;
            self.page = Page::Project;
        }
        self.board.error = result.err().map(|e| format!("{e:#}"));
        cx.notify();
        self.board.error.is_none()
    }
    fn section(&mut self, section: Section, window: &mut Window, cx: &mut Context<Self>) {
        self.page = Page::Project;
        self.stage_setup = None;
        self.board.section = section;
        self.board.delivery = None;
        self.board.timeline = None;
        self.board.zcode_history = None;
        self.board.sync_message = None;
        self.board.site_editor = None;
        self.board.infra_view = None;
        self.board.pick_record = false;
        self.board.filter_stage = None;
        self.board.error = None;
        self.board.creating_work = false;
        if let Some(dir) = self.board.dir.as_deref() {
            match Library::open(dir) {
                Ok(latest) if latest.revision != self.board.library.revision => {
                    self.board.library = latest;
                    self.board.note_key = None;
                }
                Err(e) => self.board.error = Some(e.to_string()),
                _ => (),
            }
        }
        self.board.zcode_draft = None;
        if section == Section::Settings {
            self.ensure_zcode_catalog(cx);
        }
        if section == Section::Memory {
            self.board.editing_memory = None;
            self.board
                .memory_note
                .update(cx, |s, cx| s.set_value("", window, cx));
            self.refresh_project_memory(cx);
        }
        if section == Section::Work {
            self.load_stage(window, cx);
            self.refresh_work(cx);
        }
        cx.notify();
    }
    pub(super) fn return_to_project(&mut self, w: &mut Window, cx: &mut Context<Self>) {
        let section = if self.board.library.project().is_some() {
            self.board.section
        } else {
            Section::Library
        };
        let stage = self
            .stage_setup
            .as_ref()
            .filter(|s| {
                self.context_work().is_some_and(|w| {
                    w.id == s.plan.work && s.plan.stage < w.template.stages().len()
                })
            })
            .map(|s| s.plan.stage);
        self.section(section, w, cx);
        if let Some(stage) = stage
            && section == Section::Work
        {
            self.show_stage(stage, w, cx);
        }
    }
    fn load_stage(&mut self, w: &mut Window, cx: &mut Context<Self>) {
        if let Some(work) = self.board.library.project().and_then(Project::selected) {
            self.show_stage(work.stage.min(work.template.stages().len() - 1), w, cx);
        }
    }
    fn show_stage(&mut self, stage: usize, w: &mut Window, cx: &mut Context<Self>) {
        let Some(work) = self.board.library.project().and_then(Project::selected) else {
            return;
        };
        let key = (work.id.clone(), stage);
        if self.board.note_key.as_ref() != Some(&key) {
            self.board
                .note
                .update(cx, |input, cx| input.set_value(&work.notes[stage], w, cx));
            let profile = work.profile(stage);
            self.fill_profile(profile, w, cx);
            self.board.note_key = Some(key);
            self.watch_agents(cx);
        }
        self.board.viewing = stage;
        cx.notify();
    }
    fn save_stage(&mut self, advance: bool, w: &mut Window, cx: &mut Context<Self>) {
        let Some((id, stage)) = self.board.note_key.clone() else {
            return;
        };
        let value = self.board.note.read(cx).value().to_string();
        let Some(home) = self.board.dir.clone() else {
            return;
        };
        if self.change_board(cx, |lib| {
            let p = lib.project_mut()?;
            ensure!(p.selected.as_ref() == Some(&id), "事项已更新，请重新打开");
            if advance {
                p.finish_stage(&home, &id, stage, value)
            } else {
                p.selected_mut()?.notes[stage] = value;
                Ok(())
            }
        }) && advance
        {
            self.load_stage(w, cx);
        }
    }
    fn create_work(&mut self, w: &mut Window, cx: &mut Context<Self>) {
        let name = self.board.new_work.read(cx).value().to_string();
        let kind = self.board.kind;
        let template = self.board.template;
        let repos = self.board.new_work_repos.clone();
        if self.change_board(cx, |lib| {
            let p = lib.project_mut()?;
            let repos: Vec<_> = repos
                .into_iter()
                .filter(|r| p.repositories.contains(r))
                .collect();
            p.add_work(&name, kind, template)?;
            let work = p.selected_mut()?;
            for repo in &repos {
                work.include(repo);
            }
            Ok(())
        }) {
            self.board.new_work_repos.clear();
            self.board
                .new_work
                .update(cx, |field, cx| field.set_value("", w, cx));
            self.section(Section::Work, w, cx);
        }
    }
    fn add_repositories(&mut self, paths: Vec<PathBuf>, w: &mut Window, cx: &mut Context<Self>) {
        let roots = paths
            .iter()
            .map(|p| Workspace::open(p)?.root.canonicalize().map_err(Into::into))
            .collect::<Result<Vec<_>>>();
        if self.change_board(cx, |lib| {
            let project = lib.project_mut()?;
            for root in roots? {
                if !project.repositories.contains(&root) {
                    project.repositories.push(root);
                }
            }
            Ok(())
        }) {
            self.board
                .repo_path
                .update(cx, |field, cx| field.set_value("", w, cx));
        }
    }
    fn pick_repositories(&mut self, w: &mut Window, cx: &mut Context<Self>) {
        let active = self.board.library.active.clone();
        let dialog = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("关联 Git 仓库".into()),
        });
        cx.spawn_in(w, async move |this, cx| {
            let result = dialog.await;
            let _ = this.update_in(cx, |this, w, cx| {
                if this.board.library.active != active {
                    return;
                }
                match result {
                    Ok(Ok(Some(paths))) => this.add_repositories(paths, w, cx),
                    Ok(Ok(None)) => (),
                    other => {
                        this.board.error = Some(format!("选择仓库失败 {other:?}"));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }
    fn refresh_work(&mut self, cx: &mut Context<Self>) {
        self.board.generation += 1;
        let generation = self.board.generation;
        self.board.statuses.clear();
        let Some(work) = self
            .board
            .library
            .project()
            .and_then(Project::selected)
            .cloned()
        else {
            return;
        };
        let runner = self.runner.clone();
        let job = cx.background_executor().spawn(async move {
            let mut status = BTreeMap::new();
            for path in &work.repositories {
                let ticket = work.ticket(path);
                let result = (|| -> Result<String> {
                    let ws = Workspace::open(path)?;
                    if !ws
                        .data
                        .join("local-tasks")
                        .join(format!("{ticket}.json"))
                        .try_exists()?
                    {
                        return Ok("待创建任务".into());
                    }
                    let bytes = local_page::call(
                        &ws,
                        &runner,
                        &["status".into(), "--ticket".into(), ticket],
                        None,
                    )?;
                    let state: Value = serde_json::from_slice(&bytes)?;
                    Ok(local_page::label(string(&state, "phase")).0.into())
                })();
                status.insert(
                    path.clone(),
                    result.unwrap_or_else(|e| format!("读取失败 · {e:#}")),
                );
            }
            status
        });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |this, cx| {
                if this.board.generation == generation {
                    this.board.statuses = result;
                    cx.notify();
                }
            });
        })
        .detach();
    }
    fn open_stage_relay(
        &mut self,
        repo: &Path,
        stage: usize,
        w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.board.library.project() else {
            return;
        };
        let Some(work) = project.selected() else {
            return;
        };
        let stage_model = |client| {
            work.profiles
                .get(&stage)
                .filter(|profile| profile.client == client && !profile.model.is_empty())
                .or_else(|| project.preset(client))
                .map(|profile| profile.model.clone())
                .unwrap_or_default()
        };
        let defaults = crate::relay::RelayAgents {
            model: stage_model(Client::Codex),
            reviewer_model: Some(stage_model(Client::Claude)),
            zcode_model: stage_model(Client::ZCode),
            ..Default::default()
        };
        let plan = crate::relay::StagePlan {
            incoming: work.incoming(stage).cloned(),
            project: project.id.clone(),
            work: work.id.clone(),
            stage,
            first: defaults.clone(),
            second: defaults.clone(),
        };
        let title = format!(
            "{} / {} / 两轮接力",
            work.name,
            work.template.stages()[stage].name
        );
        let result = Workspace::open(repo).and_then(|ws| crate::relay::stage_tasks(&ws));
        match result {
            Ok(tasks) => {
                self.stage_setup = Some(crate::relay::StageSetup {
                    plan,
                    repo: repo.into(),
                    title,
                    tasks,
                    editing_second: false,
                });
                self.project_tool(repo, Page::Relay, None, w, cx);
                self.set_relay_agents(&defaults, w, cx);
            }
            Err(e) => self.board.error = Some(format!("读取本地任务失败 · {e:#}")),
        }
        cx.notify();
    }
    pub(crate) fn attach_stage_record(
        &mut self,
        plan: &crate::relay::StagePlan,
        id: &str,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let repo = self
            .workspace
            .as_ref()
            .context("没有选择仓库")?
            .root
            .clone();
        ensure!(
            self.change_board(cx, |lib| {
                let p = lib.project_mut()?;
                ensure!(
                    p.id == plan.project && p.selected.as_ref() == Some(&plan.work),
                    "事项已切换"
                );
                ensure!(
                    p.selected().and_then(|w| w.incoming(plan.stage)) == plan.incoming.as_ref(),
                    "阶段交接已变化，请重新打开"
                );
                p.link_at_stage(
                    &plan.work,
                    &repo,
                    aep::project::RecordKind::Relay,
                    id,
                    Some(plan.stage),
                )
            }),
            "{}",
            self.board.error.as_deref().unwrap_or("关联失败")
        );
        Ok(())
    }
    fn project_tool(
        &mut self,
        path: &Path,
        page: Page,
        ticket: Option<&str>,
        w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match Workspace::open(path) {
            Ok(ws) => {
                self.board.pick_record = false;
                self.board.infra_view = None;
                self.https_remote_form = None;
                if page == Page::Relay {
                    self.relay_id = None;
                    self.relay_task = None;
                }
                if self
                    .workspace
                    .as_ref()
                    .is_none_or(|active| active.root != ws.root)
                {
                    self.set_workspace(ws, page, w, cx);
                } else {
                    self.navigate(page, w, cx);
                }
                if let Some(ticket) = ticket {
                    self.focus_project_ticket(
                        ticket,
                        self.board
                            .library
                            .project()
                            .and_then(Project::selected)
                            .and_then(|item| item.template.stages().get(self.board.viewing))
                            .map_or("spec", |s| match s.role {
                                StageRole::Build => "implementation",
                                StageRole::Test => "checks",
                                StageRole::Accept | StageRole::Deliver => "review",
                                StageRole::Plan => "spec",
                            }),
                        w,
                        cx,
                    );
                }
            }
            Err(e) => {
                self.board.error = Some(format!("无法打开仓库 {e:#}"));
                cx.notify();
            }
        }
    }

    pub(crate) fn context_work(&self) -> Option<&Work> {
        if self.board.section != Section::Work {
            return None;
        }
        let work = self.board.library.project()?.selected()?;
        let repo = &self.workspace.as_ref()?.root;
        work.repositories.contains(repo).then_some(work)
    }
    pub(super) fn ticket_in_context(&self, ticket: &str) -> bool {
        self.context_work()
            .zip(self.workspace.as_ref())
            .is_none_or(|(work, ws)| work.ticket(&ws.root) == ticket)
    }
    pub(super) fn link_project_ticket(&mut self, ticket: String, cx: &mut Context<Self>) {
        let Some(root) = self.workspace.as_ref().map(|w| w.root.clone()) else {
            return;
        };
        if self.change_board(cx, |lib| {
            aep::workspace::ident(&ticket, 63)?;
            let work = lib.project_mut()?.selected_mut()?;
            ensure!(work.repositories.contains(&root), "该仓库未纳入当前事项");
            work.tickets.insert(root, ticket.clone());
            Ok(())
        }) && let Some(state) = &mut self.local_tasks
        {
            state.show_catalog = false;
        }
    }

    fn section_button(
        &self,
        id: &'static str,
        title: &'static str,
        section: Section,
        icon_name: IconName,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let on = self.board.section == section;
        h_flex()
            .id(id)
            .test_support()
            .role(Role::Button)
            .aria_label(title)
            .tab_stop(true)
            .cursor_pointer()
            .rounded_md()
            .text_color(rgb(if on { MINT } else { MUTED }))
            .hover(|s| s.bg(rgb(SELECTED)))
            .flex()
            .items_center()
            .gap_2()
            .py_2()
            .px_3()
            .bg(rgb(if on { SELECTED } else { RAIL }))
            .text_sm()
            .child(icon(icon_name))
            .child(title)
            .on_click(cx.listener(move |this, _, w, cx| this.section(section, w, cx)))
    }
    pub(super) fn project_rail(&self, cx: &Context<Self>) -> Div {
        let project = self.board.library.project();
        let mut rail = v_flex()
            .w(px(194.))
            .h_full()
            .flex_none()
            .p_3()
            .gap_2()
            .bg(rgb(RAIL))
            .border_r_1()
            .border_color(rgb(LINE))
            .child(
                h_flex()
                    .gap_2()
                    .py_4()
                    .px_2()
                    .child(
                        div()
                            .rounded_md()
                            .px_2()
                            .bg(rgb(MINT))
                            .text_color(rgb(RAIL))
                            .font_bold()
                            .child("A"),
                    )
                    .child(heading("AEP Studio")),
            )
            .child(self.section_button(
                "project-library",
                "项目库",
                Section::Library,
                IconName::BookOpen,
                cx,
            ));
        if self.page == Page::Project {
            rail = rail
                .child(
                    button("project-open-directory", "打开项目目录…", false)
                        .on_click(cx.listener(|this, _, w, cx| this.choose_repo(w, cx))),
                )
                .child(
                    button("project-sync", "立即同步", false)
                        .on_click(cx.listener(|this, _, w, cx| this.sync_project(w, cx))),
                );
        }
        if let Some(p) = project {
            rail = rail
                .child(caption(p.name.clone()).truncate().mt_4().px_2())
                .child(self.section_button(
                    "project-overview",
                    "项目概览",
                    Section::Overview,
                    IconName::LayoutDashboard,
                    cx,
                ))
                .child(self.section_button(
                    "project-work",
                    "开发事项",
                    Section::Work,
                    IconName::Play,
                    cx,
                ))
                .child(self.section_button(
                    "project-memory",
                    "项目记忆",
                    Section::Memory,
                    IconName::BookOpen,
                    cx,
                ))
                .child(self.section_button(
                    "project-repos",
                    "关联仓库",
                    Section::Repositories,
                    IconName::Folder,
                    cx,
                ))
                .child(self.section_button(
                    "project-settings",
                    "项目设置",
                    Section::Settings,
                    IconName::Settings,
                    cx,
                ));
        }
        if self.page != Page::Project {
            rail = rail.child(div().flex_1()).child(
                button(
                    "back-to-project",
                    if self.in_settings() {
                        "返回项目设置"
                    } else {
                        "返回开发事项"
                    },
                    true,
                )
                .on_click(cx.listener(|this, _, w, cx| this.return_to_project(w, cx))),
            );
        }
        rail
    }
    pub(super) fn project_page(&self, cx: &mut Context<Self>) -> Div {
        let project = self.board.library.project();
        let rail = self.project_rail(cx);
        let mut page = v_flex()
            .id("project-page")
            .test_support()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .gap_4()
            .p_6();
        if let Some(error) = &self.board.error {
            page = page.child(detail_text("project-error", error).text_color(rgb(AMBER)));
        } else if let Some(message) = &self.board.sync_message {
            page = page.child(detail_text("project-sync-result", message));
        }
        if self.board.section == Section::Library || project.is_none() {
            page = page.child(self.library_view(cx));
        } else if let Some(p) = project {
            let mut head = v_flex().gap_1().child(self.breadcrumb(p, cx));
            let title = match self.board.section {
                Section::Overview => p.name.clone(),
                Section::Work if self.board.creating_work => "新建开发事项".into(),
                Section::Work => p.selected().map_or("开发事项".into(), |w| w.name.clone()),
                Section::Memory => "项目记忆".into(),
                Section::Settings => "项目设置".into(),
                _ => "关联仓库".into(),
            };
            head = head.child(heading(title).text_2xl());
            let mut bar = h_flex().justify_between().gap_3().child(head);
            if matches!(self.board.section, Section::Work | Section::Overview)
                && !self.board.creating_work
            {
                bar = bar.child(
                    primary("project-new-work", "＋ 新建事项").on_click(cx.listener(
                        |this, _, w, cx| {
                            this.section(Section::Work, w, cx);
                            this.board.creating_work = true;
                            this.board.new_work.focus_handle(cx).focus(w, cx);
                        },
                    )),
                );
            }
            page = page.child(bar);
            if self.board.switcher {
                page = page.child(self.project_switcher(cx));
            }
            match self.board.section {
                Section::Overview => {
                    let finished = p
                        .work
                        .iter()
                        .filter(|work| work.stage >= work.template.stages().len())
                        .count();
                    page = page
                        .child(
                            h_flex()
                                .bg(rgb(PANEL))
                                .py_3()
                                .rounded_lg()
                                .child(metric("进行中", p.work.len() - finished, MINT))
                                .child(metric("已交付", finished, FG))
                                .child(metric("关联仓库", p.repositories.len(), BLUE)),
                        )
                        .child(self.template_panel(p, cx))
                        .child(self.work_list(p, cx))
                        .child(
                            h_flex()
                                .justify_between()
                                .child(caption(
                                    p.repositories
                                        .iter()
                                        .map(|path| basename(path))
                                        .collect::<Vec<_>>()
                                        .join("   /   "),
                                ))
                                .child(
                                    button("overview-manage-repos", "管理关联仓库", false)
                                        .on_click(cx.listener(|this, _, w, cx| {
                                            this.section(Section::Repositories, w, cx)
                                        })),
                                ),
                        );
                }
                Section::Memory => page = page.child(self.memory_board(cx)),
                Section::Settings => page = page.child(self.project_settings(p, cx)),
                Section::Repositories => page = page.child(self.repositories(p, cx)),
                Section::Work => {
                    if self.board.creating_work {
                        page = page.child(self.work_form(p, cx));
                    } else if let Some(work) = p.selected() {
                        let collab = self.board.desk.as_ref().is_some_and(|d| d.work == work.id);
                        page = page.child(self.work_tabs(work, collab, cx));
                        if let Some(flow) =
                            self.board.delivery.as_ref().filter(|s| s.work == work.id)
                        {
                            page = page.child(self.delivery_page(flow, work, cx));
                        } else if let Some(history) = self
                            .board
                            .timeline
                            .as_ref()
                            .filter(|s| s.work == work.id && s.project == p.id)
                        {
                            page = page.child(self.work_timeline(history, work, cx));
                        } else if collab {
                            page = page.child(self.work_collaboration(p, work, cx));
                        } else {
                            page = page.child(self.work_detail(p, work, cx));
                        }
                    }
                    if p.work.is_empty() && !self.board.creating_work {
                        page = page.child(self.work_list(p, cx));
                    }
                }
                Section::Library => {}
            }
        }
        let column = project
            .filter(|p| self.board.section == Section::Work && !p.work.is_empty())
            .map(|p| self.work_column(p, cx));
        h_flex()
            .size_full()
            .bg(rgb(CANVAS))
            .child(rail)
            .children(column)
            .child(page)
    }

    /// Exits for a stage whose run did not pass: send it back, retry, or waive with a reason.
    /// Every option stays available, so a failing gate can never strand the item.
    fn rework_panel(&self, work: &Work, from: usize, cx: &Context<Self>) -> Div {
        let stages = work.template.stages();
        let to = (0..from)
            .rev()
            .find(|&i| stages[i].role == StageRole::Build)
            .unwrap_or(from - 1);
        let report = work
            .current_attempt
            .get(&from)
            .and_then(|id| self.attempt_path(id))
            .and_then(|d| std::fs::read_to_string(d.join("result.txt")).ok())
            .unwrap_or_default();
        let blocked = report
            .trim()
            .lines()
            .last()
            .is_some_and(|l| l.trim().contains("BLOCKED"));
        let loops = work.reworks_between(from, to);
        let mut panel = v_flex()
            .gap_2()
            .mt_2()
            .pl(px(34.))
            .pt_3()
            .border_t_1()
            .border_color(rgb(LINE))
            .child(t("这一轮没有通过，选择下一步：", FG).font_semibold());
        if blocked {
            panel = panel.child(caption(
                "Agent 报告为 BLOCKED：多半是执行环境受限（沙箱、端口、GUI）。退回开发通常解决不了，建议修正环境后在上方继续对话重跑，或核实后人工放行。",
            ).text_color(rgb(AMBER)));
        }
        if loops >= 2 {
            panel = panel.child(
                caption(format!(
                    "已从「{}」退回「{}」{loops} 次。再次退回前，请先确认失败原因确实能由开发修复。",
                    stages[from].name, stages[to].name
                ))
                .text_color(rgb(AMBER)),
            );
        }
        let from_name = stages[from].name;
        let next_name = stages.get(from + 1).map_or("交付完成", |s| s.name);
        // An option row: label, explanation and a bordered action; the recommended one is filled.
        let option = |title: String, note: String, recommended: bool| {
            v_flex()
                .gap_1()
                .p_3()
                .rounded_md()
                .border_1()
                .border_color(rgb(if recommended { MINT } else { LINE }))
                .bg(rgb(CANVAS))
                .child(
                    h_flex()
                        .gap_2()
                        .child(t(title, FG).font_semibold())
                        .when(recommended, |r| r.child(t("推荐", MINT).text_xs())),
                )
                .child(caption(note))
        };
        let action = |id: &'static str, label: String, filled: bool| {
            let el = div()
                .id(id)
                .test_support()
                .role(Role::Button)
                .aria_label(SharedString::from(label.clone()))
                .tab_stop(true)
                .cursor_pointer()
                .flex_none()
                .px_4()
                .py_1p5()
                .rounded_md()
                .text_sm()
                .font_semibold()
                .border_1()
                .border_color(rgb(MINT))
                .child(label);
            if filled {
                el.bg(rgb(MINT))
                    .text_color(rgb(0x102920))
                    .hover(|s| s.bg(rgb(0xb3ebd8)))
            } else {
                el.text_color(rgb(MINT)).hover(|s| s.bg(rgb(SELECTED)))
            }
        };
        let reason_given = !self.board.waive.read(cx).value().trim().is_empty();
        let error = self.board.error.clone();
        panel
            .child(
                option(
                    format!("① 退回「{}」修复", stages[to].name),
                    format!(
                        "代码确实有问题时用。失败报告会带回「{}」的原会话，修好后重新推进会再跑一次「{from_name}」。",
                        stages[to].name
                    ),
                    !blocked,
                )
                .child(
                    h_flex().child(
                        action(
                            "stage-send-back",
                            format!("退回「{}」修复", stages[to].name),
                            !blocked,
                        )
                        .on_click(cx.listener(move |this, _, w, cx| {
                            this.send_back(from, to, from_name, w, cx)
                        })),
                    ),
                ),
            )
            .child(option(
                "② 继续对话重跑".into(),
                "环境已修好或要补证据时用：在上方「阶段会话」输入要求并发送，本阶段会再跑一轮。".into(),
                false,
            ))
            .child(
                option(
                    "③ 人工放行".into(),
                    format!(
                        "你已在别处核实过时用。写明理由后直接进入「{next_name}」，记录会保留理由和 Agent 的原始报告。"
                    ),
                    blocked,
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(input_field("stage-waive-reason", &self.board.waive))
                        .child(
                            action(
                                "stage-waive",
                                format!("放行并进入「{next_name}」"),
                                blocked && reason_given,
                            )
                            .on_click(cx.listener(move |this, _, w, cx| {
                                let reason = this.board.waive.read(cx).value().to_string();
                                let Some(home) = this.board.dir.clone() else {
                                    return;
                                };
                                if this.change_board(cx, |lib| {
                                    let p = lib.project_mut()?;
                                    let wid = p.selected().context("请选择开发事项")?.id.clone();
                                    p.waive_stage(&home, &wid, from, &reason)
                                }) {
                                    this.board.waive.update(cx, |s, cx| s.set_value("", w, cx));
                                    this.load_stage(w, cx);
                                }
                            })),
                        ),
                ),
            )
            .when_some(error, |p, e| {
                p.child(detail_text("stage-rework-error", &e).text_color(rgb(RED)))
            })
    }

    fn send_back(
        &mut self,
        from: usize,
        to: usize,
        from_name: &str,
        w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.has_running_agent() {
            self.board.error = Some("请先等待当前 CLI 完成，或取消执行".into());
            cx.notify();
            return;
        }
        let Some(work) = self.board.library.project().and_then(Project::selected) else {
            return;
        };
        let report = work
            .current_attempt
            .get(&from)
            .and_then(|id| self.attempt_path(id));
        let text = report
            .as_ref()
            .and_then(|d| std::fs::read_to_string(d.join("result.txt")).ok())
            .unwrap_or_default();
        let summary = aep::handoff::summary(&text);
        let reason = if summary.trim().is_empty() {
            format!("「{from_name}」未通过")
        } else {
            summary
        };
        let continuable = work.current_attempt.contains_key(&to);
        if !self.change_board(cx, |lib| {
            lib.project_mut()?.selected_mut()?.send_back(to, &reason)
        }) {
            return;
        }
        let mut message = format!("「{from_name}」未通过，请根据报告修复后再交回：\n{reason}");
        if let Some(d) = report {
            message.push_str(&format!("\n\n完整报告：{}", d.join("result.txt").display()));
        }
        self.load_stage(w, cx);
        if continuable {
            self.board
                .chat
                .update(cx, |s, cx| s.set_value(&message, w, cx));
        } else {
            self.board
                .note
                .update(cx, |s, cx| s.set_value(&message, w, cx));
        }
        cx.notify();
    }

    /// 项目库 › 项目 ▾ › 事项 › 阶段 — the current selection, each part clickable.
    fn breadcrumb(&self, p: &Project, cx: &Context<Self>) -> Div {
        let crumb = |id: &'static str, label: SharedString, on: bool| {
            div()
                .id(id)
                .test_support()
                .role(Role::Button)
                .aria_label(label.clone())
                .tab_stop(true)
                .cursor_pointer()
                .px_1p5()
                .rounded_sm()
                .text_xs()
                .text_color(rgb(if on { FG } else { MUTED }))
                .hover(|s| s.bg(rgb(SELECTED)))
                .child(label)
        };
        let mut row = h_flex()
            .gap_1()
            .child(
                crumb("crumb-library", "项目库".into(), false).on_click(cx.listener(
                    |this, _, w, cx| {
                        this.board.switcher = false;
                        this.section(Section::Library, w, cx)
                    },
                )),
            )
            .child(caption("›"))
            .child(
                crumb(
                    "crumb-project",
                    format!("{} ▾", p.name).into(),
                    self.board.switcher,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.board.switcher = !this.board.switcher;
                    cx.notify();
                })),
            );
        let section = match self.board.section {
            Section::Overview => None,
            Section::Work => Some("开发事项"),
            Section::Memory => Some("项目记忆"),
            Section::Settings => Some("项目设置"),
            _ => Some("关联仓库"),
        };
        if let Some(name) = section {
            row = row.child(caption("›")).child(caption(name));
        }
        if self.board.section == Section::Work
            && !self.board.creating_work
            && let Some(work) = p.selected()
        {
            let stages = work.template.stages();
            let viewing = self.board.viewing.min(stages.len() - 1);
            row = row
                .child(caption("›"))
                .child(
                    crumb("crumb-work", work.name.clone().into(), true)
                        .on_click(cx.listener(|this, _, w, cx| this.load_stage(w, cx))),
                )
                .child(caption("›"))
                .child(caption(if work.stage >= stages.len() {
                    "已交付".to_string()
                } else if viewing == work.stage {
                    format!("{}（当前）", stages[viewing].name)
                } else {
                    format!("{}（查看中）", stages[viewing].name)
                }));
        }
        row
    }

    fn project_switcher(&self, cx: &Context<Self>) -> Div {
        let mut list = panel().flex_none().gap_1().child(caption("切换项目"));
        for p in &self.board.library.projects {
            let id = p.id.clone();
            let on = self.board.library.active.as_ref() == Some(&id);
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("crumb-project-{id}")))
                    .test_support()
                    .role(Role::Button)
                    .aria_label(p.name.clone())
                    .tab_stop(true)
                    .cursor_pointer()
                    .justify_between()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .when(on, |r| r.bg(rgb(SELECTED)))
                    .hover(|s| s.bg(rgb(SELECTED)))
                    .child(t(p.name.clone(), if on { MINT } else { FG }))
                    .child(caption(format!(
                        "{} 个事项 · {} 个仓库",
                        p.work.len(),
                        p.repositories.len()
                    )))
                    .on_click(cx.listener(move |this, _, w, cx| {
                        this.board.switcher = false;
                        if this.change_board(cx, |lib| {
                            lib.active = Some(id.clone());
                            Ok(())
                        }) {
                            let section = if this.board.section == Section::Library {
                                Section::Overview
                            } else {
                                this.board.section
                            };
                            this.section(section, w, cx);
                        }
                    })),
            );
        }
        list.child(
            button("crumb-new-project", "＋ 新建项目…", false).on_click(cx.listener(
                |this, _, w, cx| {
                    this.board.switcher = false;
                    this.section(Section::Library, w, cx)
                },
            )),
        )
    }

    /// The work items of the project, always beside the open item.
    fn work_column(&self, p: &Project, cx: &Context<Self>) -> Stateful<Div> {
        let selected = p.selected().map(|w| w.id.clone());
        let mut column = v_flex()
            .id("work-column")
            .w(px(248.))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .gap_1()
            .p_3()
            .border_r_1()
            .border_color(rgb(LINE))
            .child(
                h_flex()
                    .justify_between()
                    .pb_2()
                    .child(t("开发事项", FG).font_semibold())
                    .child(
                        button("work-column-new", "＋ 新建", true).on_click(cx.listener(
                            |this, _, w, cx| {
                                this.section(Section::Work, w, cx);
                                this.board.creating_work = true;
                                this.board.new_work.focus_handle(cx).focus(w, cx);
                            },
                        )),
                    ),
            );
        for (title, finished) in [("进行中", false), ("已交付", true)] {
            let items: Vec<_> = p
                .work
                .iter()
                .rev()
                .filter(|w| (w.stage >= w.template.stages().len()) == finished)
                .collect();
            if items.is_empty() {
                continue;
            }
            column = column.child(caption(format!("{title} · {}", items.len())).pt_2());
            for work in items {
                let id = work.id.clone();
                let stages = work.template.stages();
                let mut bar = h_flex().gap(px(2.)).pt_1();
                for i in 0..stages.len() {
                    bar = bar.child(div().flex_1().h(px(3.)).rounded_sm().bg(rgb(
                        if i < work.stage {
                            MINT
                        } else if i == work.stage {
                            BLUE
                        } else {
                            LINE
                        },
                    )));
                }
                column = column.child(
                    item(
                        format!("project-item-{id}"),
                        selected.as_ref() == Some(&id) && !self.board.creating_work,
                    )
                    .gap_1()
                    .py_2()
                    .child(
                        h_flex().gap_2().child(caption(work.kind.label())).child(
                            t(work.name.clone(), FG)
                                .font_semibold()
                                .truncate()
                                .min_w_0(),
                        ),
                    )
                    .child(
                        h_flex()
                            .justify_between()
                            .child(caption(work.stage_name().to_owned()))
                            .child(caption(format!(
                                "{}/{}",
                                work.stage.min(stages.len()),
                                stages.len()
                            ))),
                    )
                    .child(bar)
                    .on_click(cx.listener(move |this, _, w, cx| {
                        if this.change_board(cx, |lib| {
                            lib.project_mut()?.selected = Some(id.clone());
                            Ok(())
                        }) {
                            this.section(Section::Work, w, cx);
                        }
                    })),
                );
            }
        }
        column
    }

    fn template_panel(&self, p: &Project, cx: &Context<Self>) -> Div {
        let mut result = panel().flex_none().child(heading("开发工作流"));
        for template in [Template::Software, Template::Game, Template::Legacy] {
            if template != Template::Software
                && !p.work.iter().any(|item| item.template == template)
            {
                continue;
            }
            let mut row = h_flex().gap_1().flex_wrap();
            for (i, stage) in template.stages().iter().enumerate() {
                let count = p
                    .work
                    .iter()
                    .filter(|item| item.template == template && item.stage == i)
                    .count();
                let filter = Some((template, i));
                let id = if template == Template::Software {
                    format!("project-filter-{i}")
                } else {
                    format!("project-filter-{template:?}-{i}")
                };
                if i > 0 {
                    row = row.child(caption("›"));
                }
                row = row.child(
                    button(
                        id,
                        format!("{} · {count}", stage.name),
                        self.board.filter_stage == filter,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.board.filter_stage = if this.board.filter_stage == filter {
                            None
                        } else {
                            filter
                        };
                        cx.notify();
                    })),
                );
            }
            result = result.child(caption(template.label())).child(row);
        }
        result
    }
    fn library_view(&self, cx: &Context<Self>) -> Div {
        let mut rows = v_flex().gap_2();
        for p in &self.board.library.projects {
            let id = p.id.clone();
            rows = rows.child(
                item(
                    format!("project-open-{id}"),
                    self.board.library.active.as_ref() == Some(&id),
                )
                .child(heading(p.name.clone()))
                .child(caption(format!(
                    "{} 个开发事项  ·  {} 个仓库",
                    p.work.len(),
                    p.repositories.len()
                )))
                .on_click(cx.listener(move |this, _, w, cx| {
                    if this.change_board(cx, |lib| {
                        lib.active = Some(id.clone());
                        Ok(())
                    }) {
                        this.section(Section::Overview, w, cx);
                    }
                })),
            );
        }
        v_flex()
            .gap_4()
            .child(heading("项目库").text_2xl())
            .child(
                panel().child(heading("创建项目")).child(
                    h_flex()
                        .gap_3()
                        .child(input_field("project-name", &self.board.new_project))
                        .child(primary("project-create", "创建项目").on_click(cx.listener(
                            |this, _, w, cx| {
                                let name = this.board.new_project.read(cx).value().to_string();
                                if this.change_board(cx, |lib| lib.create(&name, vec![])) {
                                    this.board
                                        .new_project
                                        .update(cx, |input, cx| input.set_value("", w, cx));
                                    this.section(Section::Overview, w, cx);
                                }
                            },
                        ))),
                ),
            )
            .child(rows)
    }
    fn work_form(&self, p: &Project, cx: &Context<Self>) -> Div {
        let mut types = h_flex().gap_2();
        for kind in [Kind::Feature, Kind::Release] {
            types = types.child(
                button(
                    format!("project-kind-{}", kind.label()),
                    kind.label(),
                    self.board.kind == kind,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.board.kind = kind;
                    cx.notify();
                })),
            );
        }
        let mut templates = h_flex().gap_2();
        for template in [Template::Software, Template::Game] {
            let on = self.board.template == template;
            templates = templates.child(
                v_flex()
                    .id(SharedString::from(format!("work-template-{template:?}")))
                    .test_support()
                    .role(Role::Button)
                    .aria_label(template.label())
                    .tab_stop(true)
                    .cursor_pointer()
                    .flex_1()
                    .gap_1()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(if on { MINT } else { LINE }))
                    .bg(rgb(if on { SELECTED } else { CANVAS }))
                    .hover(|s| s.bg(rgb(SELECTED)))
                    .child(t(template.label(), if on { MINT } else { FG }).font_semibold())
                    .child(caption(
                        template
                            .stages()
                            .iter()
                            .map(|s| s.name)
                            .collect::<Vec<_>>()
                            .join(" › "),
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.board.template = template;
                        cx.notify();
                    })),
            );
        }
        let mut repos = v_flex().gap_1();
        if p.repositories.is_empty() {
            repos = repos.child(
                h_flex()
                    .gap_2()
                    .child(caption(
                        "项目还没有关联仓库，Agent 会在项目文件目录中工作。",
                    ))
                    .child(button("new-work-link-repos", "关联仓库", false).on_click(
                        cx.listener(|this, _, w, cx| this.section(Section::Repositories, w, cx)),
                    )),
            );
        }
        for (i, path) in p.repositories.iter().enumerate() {
            let on = self.board.new_work_repos.contains(path);
            let repo = path.clone();
            repos = repos.child(
                h_flex()
                    .id(SharedString::from(format!("new-work-repo-{i}")))
                    .test_support()
                    .role(Role::Button)
                    .aria_label(basename(path))
                    .tab_stop(true)
                    .cursor_pointer()
                    .gap_2()
                    .px_3()
                    .py_1p5()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(if on { MINT } else { LINE }))
                    .bg(rgb(if on { SELECTED } else { CANVAS }))
                    .child(t(if on { "☑" } else { "☐" }, if on { MINT } else { MUTED }))
                    .child(t(basename(path), FG))
                    .child(
                        caption(path.display().to_string())
                            .truncate()
                            .min_w_0()
                            .flex_1(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let list = &mut this.board.new_work_repos;
                        if let Some(at) = list.iter().position(|r| r == &repo) {
                            list.remove(at);
                        } else {
                            list.push(repo.clone());
                        }
                        cx.notify();
                    })),
            );
        }
        let step = |n: &str, title: &str| {
            h_flex()
                .gap_2()
                .child(t(n.to_owned(), MINT).font_semibold())
                .child(t(title.to_owned(), FG).font_semibold())
        };
        panel()
            .flex_none()
            .gap_4()
            .child(
                v_flex()
                    .gap_2()
                    .child(step("1", "选择开发模板"))
                    .child(templates),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(step("2", "类型与名称"))
                    .child(types)
                    .child(input_field("project-work-name", &self.board.new_work)),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(step("3", "纳入仓库"))
                    .child(caption(
                        "每个仓库会为这个事项建立独立工作树，之后也可以在事项里调整。",
                    ))
                    .child(repos),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        primary("project-create-work", "创建开发事项")
                            .on_click(cx.listener(|this, _, w, cx| this.create_work(w, cx))),
                    )
                    .child(
                        button("project-cancel-work", "取消", false).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.board.creating_work = false;
                                cx.notify();
                            },
                        )),
                    ),
            )
    }
    fn work_list(&self, p: &Project, cx: &Context<Self>) -> Div {
        let filter = self.board.filter_stage;
        let hits: Vec<_> = p
            .work
            .iter()
            .rev()
            .filter(|work| {
                filter.is_none_or(|(template, stage)| {
                    work.template == template && work.stage == stage
                })
            })
            .collect();
        let name = filter.map_or("项目事项", |(template, stage)| {
            template.stages()[stage].name
        });
        let mut bar = h_flex()
            .justify_between()
            .child(heading(format!("{name}  ·  {}", hits.len())));
        if filter.is_some() {
            bar = bar.child(button("project-clear-filter", "查看全部", false).on_click(
                cx.listener(|this, _, _, cx| {
                    this.board.filter_stage = None;
                    cx.notify();
                }),
            ));
        }
        let mut rows = panel().flex_none().child(bar);
        if hits.is_empty() {
            return rows.child(caption(if filter.is_some() {
                "此阶段暂无事项。"
            } else {
                "创建一个功能或版本，即可开始跟进需求和交付。"
            }));
        }
        for work in hits {
            let id = work.id.clone();
            rows = rows.child(
                item(
                    format!("project-item-{id}"),
                    p.selected.as_ref() == Some(&work.id),
                )
                .child(
                    h_flex()
                        .gap_3()
                        .child(caption(work.kind.label()))
                        .child(t(work.name.clone(), FG).flex_1().min_w_0().truncate())
                        .child(chip(work.stage_name(), MINT)),
                )
                .on_click(cx.listener(move |this, _, w, cx| {
                    if this.change_board(cx, |lib| {
                        lib.project_mut()?.selected = Some(id.clone());
                        Ok(())
                    }) {
                        this.section(Section::Work, w, cx);
                    }
                })),
            );
        }
        rows
    }
    fn repositories(&self, p: &Project, cx: &Context<Self>) -> Div {
        let mut list = v_flex().gap_3().child(
            panel().child(heading("关联本地仓库")).child(
                h_flex()
                    .gap_2()
                    .child(input_field("project-repo-path", &self.board.repo_path))
                    .child(primary("project-add-path", "关联").on_click(cx.listener(
                        |this, _, w, cx| {
                            let path = this.board.repo_path.read(cx).value().trim().to_string();
                            if !path.is_empty() {
                                this.add_repositories(vec![path.into()], w, cx);
                            }
                        },
                    )))
                    .child(
                        button("project-browse-repos", "选择文件夹…", false)
                            .on_click(cx.listener(|this, _, w, cx| this.pick_repositories(w, cx))),
                    ),
            ),
        );
        for (i, path) in p.repositories.iter().enumerate() {
            let mut tools = h_flex().gap_2().flex_wrap();
            let configure = path.clone();
            tools = tools.child(
                button(format!("repo-settings-{i}"), "仓库执行设置", false).on_click(cx.listener(
                    move |this, _, w, cx| {
                        this.section(Section::Settings, w, cx);
                        this.edit_site(&configure, w, cx);
                    },
                )),
            );
            let unlink = path.clone();
            tools = tools.child(
                button(format!("project-unlink-{i}"), "解除关联", false).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.change_board(cx, |lib| {
                            let project = lib.project_mut()?;
                            if let Some(work) = project
                                .work
                                .iter()
                                .find(|work| work.repositories.contains(&unlink))
                            {
                                anyhow::bail!("请先从事项「{}」移出这个仓库", work.name);
                            }
                            project.repositories.retain(|path| path != &unlink);
                            project.https_sites.remove(&unlink);
                            Ok(())
                        });
                    },
                )),
            );
            list = list.child(
                panel()
                    .child(heading(basename(path)))
                    .child(caption(path.display().to_string()))
                    .child(tools),
            );
        }
        list
    }
    fn work_detail(&self, p: &Project, work: &Work, cx: &Context<Self>) -> Div {
        let stages = work.template.stages();
        let viewing = self.board.viewing.min(stages.len() - 1);
        let finished = work.stage >= stages.len();
        let is_current = viewing == work.stage;
        let running = self.has_running_agent();
        let current_ev = work
            .current_attempt
            .get(&viewing)
            .and_then(|id| workflow::evidence(&self.attempt_path(id)?).ok());
        let status = current_ev
            .as_ref()
            .and_then(|ev| ev["status"].as_str())
            .unwrap_or_default()
            .to_owned();

        let mut steps = h_flex().gap_1();
        for (i, stage) in stages.iter().enumerate() {
            let profile = work.profile(i);
            let tint = if i < work.stage {
                MINT
            } else if i == work.stage {
                BLUE
            } else {
                MUTED
            };
            let model = if profile.model.is_empty() {
                profile.client.label().to_owned()
            } else {
                format!("{} · {}", profile.client.label(), profile.model)
            };
            let dot = div()
                .size(px(16.))
                .flex_none()
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .border_1()
                .border_color(rgb(tint))
                .when(i < work.stage, |d| d.bg(rgb(MINT)).text_color(rgb(CANVAS)))
                .when(i >= work.stage, |d| d.text_color(rgb(tint)))
                .child(if i < work.stage {
                    "✓".to_string()
                } else {
                    (i + 1).to_string()
                });
            steps = steps.child(
                v_flex()
                    .id(format!("project-stage-{i}"))
                    .test_support()
                    .role(Role::Button)
                    .aria_label(format!("{} · {model}", stage.name))
                    .tab_stop(true)
                    .cursor_pointer()
                    .min_w_0()
                    .flex_1()
                    .gap_1()
                    .py_2()
                    .px_2()
                    .rounded_b_md()
                    .bg(rgb(if i == viewing { SELECTED } else { CANVAS }))
                    .border_t_2()
                    .border_color(rgb(if i <= work.stage { tint } else { LINE }))
                    .child(
                        h_flex().gap_1p5().min_w_0().child(dot).child(
                            t(stage.name, if i == viewing { FG } else { tint })
                                .font_semibold()
                                .truncate()
                                .min_w_0(),
                        ),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .min_w_0()
                            .when(i == work.stage, |r| {
                                r.child(
                                    div()
                                        .flex_none()
                                        .px_1()
                                        .rounded_sm()
                                        .border_1()
                                        .border_color(rgb(BLUE))
                                        .text_color(rgb(BLUE))
                                        .text_xs()
                                        .child("当前"),
                                )
                            })
                            .child(caption(model).truncate().min_w_0()),
                    )
                    .hover(|s| s.bg(rgb(SELECTED)))
                    .on_click(cx.listener(move |this, _, w, cx| this.show_stage(i, w, cx))),
            );
        }

        // Where am I, and what should I do next?
        let guide = if finished {
            panel().flex_none().flex_row().items_center().child(t(
                "✓ 此事项已交付归档。可点击各阶段查看当时的会话与结论。",
                MINT,
            ))
        } else if !is_current {
            let mut row = h_flex()
                .flex_none()
                .gap_3()
                .px_4()
                .py_2()
                .rounded_lg()
                .bg(rgb(0x2a2518))
                .border_1()
                .border_color(rgb(0x574a2c))
                .child(
                    t(
                        if viewing < work.stage {
                            format!(
                                "正在查看已完成的「{}」，以下为当时的会话与结论。",
                                stages[viewing].name
                            )
                        } else {
                            format!(
                                "「{}」尚未开始，可以先设置它使用的 CLI。",
                                stages[viewing].name
                            )
                        },
                        AMBER,
                    )
                    .flex_1(),
                );
            if viewing < work.stage {
                row = row.child(
                    button("project-reopen-stage", "重新打开此阶段", false).on_click(cx.listener(
                        move |this, _, w, cx| {
                            if this.has_running_agent() {
                                this.board.error = Some("请先等待当前 CLI 完成，或取消执行".into());
                                cx.notify();
                                return;
                            }
                            if this.change_board(cx, |lib| {
                                let item = lib.project_mut()?.selected_mut()?;
                                item.reopen_transfers(viewing);
                                item.stage = viewing;
                                item.current_attempt.retain(|s, _| *s < viewing);
                                Ok(())
                            }) {
                                this.load_stage(w, cx);
                            }
                        },
                    )),
                );
            }
            row.child(
                primary(
                    "project-back-current",
                    format!("回到当前阶段「{}」", stages[work.stage].name),
                )
                .on_click(cx.listener(|this, _, w, cx| this.load_stage(w, cx))),
            )
        } else {
            let next = if running {
                "Agent 正在执行，可随时查看输出或取消"
            } else {
                match status.as_str() {
                    "" => "写好阶段说明并运行 Agent，或直接记录人工结论后推进",
                    "succeeded" => "核对答复：可以继续对话，确认无误后推进到下一阶段",
                    "claimed" => "等待独立会话交回结果",
                    _ => "查看失败原因，调整配置后继续对话或重新执行",
                }
            };
            panel()
                .flex_none()
                .flex_row()
                .items_center()
                .gap_3()
                .py_2()
                .child(caption("下一步"))
                .child(t(next, MINT).font_semibold().flex_1())
                .child(caption(stages[viewing].brief).truncate().max_w(px(360.)))
        };

        let returned = work
            .reworks
            .last()
            .filter(|r| is_current && r.to == viewing)
            .map(|r| {
                let n = work.reworks_between(r.from, r.to);
                h_flex()
                    .flex_none()
                    .items_start()
                    .gap_3()
                    .px_4()
                    .py_2()
                    .rounded_lg()
                    .bg(rgb(0x2a2518))
                    .border_1()
                    .border_color(rgb(0x574a2c))
                    .child(
                        t(
                            format!("↩ 由「{}」退回 · 第 {n} 次", stages[r.from].name),
                            AMBER,
                        )
                        .flex_none(),
                    )
                    .child(
                        detail_text(
                            "stage-rework-reason",
                            &r.reason.chars().take(400).collect::<String>(),
                        )
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .min_w_0()
                        .flex_1(),
                    )
            });
        let card = |n: &str, title: &str, summary: String, tone: u32| {
            h_flex()
                .gap_3()
                .child(
                    div()
                        .size(px(22.))
                        .flex_none()
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_xs()
                        .font_semibold()
                        .bg(rgb(tone))
                        .text_color(rgb(if tone == SELECTED { MUTED } else { CANVAS }))
                        .child(n.to_owned()),
                )
                .child(heading(title.to_owned()).text_base())
                .child(caption(summary).truncate().min_w_0().flex_1())
        };

        // 1 · CLI settings: collapsed to one line once the stage has a working session.
        let open = match &self.board.config_open {
            Some((id, s, open)) if id == &work.id && *s == viewing => *open,
            _ => {
                !finished
                    && !matches!(
                        status.as_str(),
                        "succeeded" | "running" | "initializing" | "claimed"
                    )
            }
        };
        let toggle_work = work.id.clone();
        let mut config = panel().flex_none().child(
            card(
                "1",
                "执行配置",
                if open {
                    "选择这个阶段使用的 CLI、模型和仓库".into()
                } else {
                    self.config_summary(work, viewing, cx)
                },
                MINT,
            )
            .child(
                button(
                    "workflow-config-toggle",
                    if open { "收起" } else { "修改" },
                    false,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.board.config_open = Some((toggle_work.clone(), viewing, !open));
                    cx.notify();
                })),
            ),
        );
        if open {
            config = config.child(
                div()
                    .pl(px(34.))
                    .child(self.agent_config(p, work, viewing, cx)),
            );
        }

        // 2 · Brief, run and conversation.
        let turns = work.attempts.iter().filter(|a| a.stage == viewing).count();
        let mut session = panel().flex_none().child(card(
            "2",
            "阶段会话",
            match (turns, running && is_current) {
                (_, true) => "运行中…".into(),
                (0, _) if is_current => "写给阶段 Agent 的说明".into(),
                (0, _) => "暂无会话".into(),
                (n, _) => format!("{n} 轮"),
            },
            if is_current && !finished {
                BLUE
            } else {
                SELECTED
            },
        ));
        let mut body = v_flex().gap_3().pl(px(34.));
        if let Some(t) = work.incoming(viewing) {
            body = body.child(
                v_flex().gap_1().child(caption("已接收上一阶段交接")).child(
                    detail_text("stage-incoming", &t.context())
                        .text_color(rgb(MUTED))
                        .pl_3()
                        .border_l_2()
                        .border_color(rgb(BLUE)),
                ),
            );
        }
        body = body.child(
            v_flex()
                .gap_1()
                .child(caption(
                    "阶段说明 · 首次运行时发给 Agent，也作为本阶段记录交给下一阶段",
                ))
                .child(
                    div().id("project-stage-note").test_support().child(
                        Textarea::new(&self.board.note)
                            .aria_label("阶段记录")
                            .h(px(if turns == 0 { 82. } else { 56. })),
                    ),
                ),
        );
        let mut run_row = h_flex().gap_2().flex_wrap().child(
            button("project-save-note", "保存说明", false)
                .on_click(cx.listener(|this, _, w, cx| this.save_stage(false, w, cx))),
        );
        if is_current && !finished && !running {
            let run = |id, label| {
                if turns == 0 {
                    primary(id, label)
                } else {
                    button(id, label, false)
                }
            };
            run_row = run_row
                .child(
                    run(
                        "workflow-run",
                        if turns == 0 {
                            "运行阶段 Agent"
                        } else {
                            "新会话重新执行"
                        },
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Err(e) = this.launch_agent(cx) {
                            this.board.error = Some(format!("{e:#}"));
                        }
                        cx.notify();
                    })),
                )
                .child(caption(format!(
                    "使用 {}",
                    self.config_summary(work, viewing, cx)
                )));
        }
        body = body.child(run_row);
        if turns > 0 {
            body = body.child(self.stage_console(p, work, viewing, cx));
        }
        session = session.child(body);

        // 3 · Confirm and advance.
        let mut actions = h_flex().gap_2().flex_wrap();
        if let Some(repo) = work.target(viewing) {
            actions = actions.child(button("stage-relay", "阶段两轮接力", false).on_click(
                cx.listener(move |s, _, w, cx| s.open_stage_relay(&repo, viewing, w, cx)),
            ));
        }
        let has_relay = work
            .records
            .iter()
            .any(|r| r.kind == aep::project::RecordKind::Relay && r.stage == Some(viewing));
        if viewing + 1 < stages.len() && (has_relay || work.current_attempt.contains_key(&viewing))
        {
            actions = actions.child(
                button("stage-transfer-review", "查看验收与阶段交接", false)
                    .on_click(cx.listener(|s, _, _, cx| s.load_timeline(cx))),
            );
        }
        let mut advance = panel().flex_none().child(card(
            "3",
            "核对并推进",
            if finished || viewing < work.stage {
                "已确认".into()
            } else if !is_current {
                "—".into()
            } else if status == "succeeded" {
                "Agent 已答复，请核对后推进".into()
            } else if work.current_attempt.contains_key(&viewing) {
                "当前一轮未成功，暂不能按 Agent 结论推进".into()
            } else {
                "未运行 Agent 时，以阶段说明作为人工结论推进".into()
            },
            if is_current && status == "succeeded" {
                MINT
            } else {
                SELECTED
            },
        ));
        if is_current && !finished {
            let label = if viewing + 1 == stages.len() {
                if work.incoming(viewing).is_some_and(|t| {
                    stages
                        .get(t.from)
                        .is_some_and(|s| s.role == StageRole::Accept)
                }) && work.current_attempt.contains_key(&viewing)
                {
                    "确认交付并归档".to_string()
                } else {
                    "确认完成交付".to_string()
                }
            } else {
                format!("确认并进入「{}」", stages[viewing + 1].name)
            };
            let blocked = work.current_attempt.contains_key(&viewing) && status != "succeeded";
            let next = if blocked {
                button("project-next", label, false)
            } else {
                primary("project-next", label)
            };
            actions = actions
                .child(next.on_click(cx.listener(|this, _, w, cx| this.save_stage(true, w, cx))));
        }
        advance = advance.child(div().pl(px(34.)).child(actions));
        if is_current
            && !finished
            && !running
            && viewing > 0
            && matches!(status.as_str(), "failed" | "cancelled")
        {
            advance = advance.child(self.rework_panel(work, viewing, cx));
        }

        let detail = v_flex()
            .flex_none()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(chip(work.template.label(), BLUE))
                    .child(chip(work.stage_name(), MINT))
                    .child(caption(if work.repositories.is_empty() {
                        "未纳入仓库".to_string()
                    } else {
                        work.repositories
                            .iter()
                            .map(|r| basename(r))
                            .collect::<Vec<_>>()
                            .join(" · ")
                    })),
            )
            .child(steps)
            .children(returned)
            .child(guide)
            .child(config)
            .child(session)
            .child(advance);
        let mut repos = panel().flex_none().child(
            h_flex()
                .justify_between()
                .child(heading("事项仓库与任务"))
                .child(
                    button("project-refresh-work", "刷新进度", false)
                        .on_click(cx.listener(|this, _, w, cx| this.section(Section::Work, w, cx))),
                ),
        );
        if p.repositories.is_empty() {
            repos = repos.child(button("project-go-repos", "关联项目仓库", true).on_click(
                cx.listener(|this, _, w, cx| this.section(Section::Repositories, w, cx)),
            ));
        }
        for (i, path) in p.repositories.iter().enumerate() {
            let included = work.repositories.contains(path);
            let repo = path.clone();
            let mut row = h_flex()
                .gap_2()
                .child(t(basename(path), FG).min_w_0().flex_1().truncate());
            if included {
                row = row.child(caption(
                    self.board
                        .statuses
                        .get(path)
                        .map_or("正在读取…", String::as_str)
                        .to_owned(),
                ));
                let (context_label, context_page) = match stages[viewing].role {
                    StageRole::Plan => ("仓库记忆", Page::Memory),
                    StageRole::Build => ("仓库接力", Page::Relay),
                    _ => ("运行证据", Page::Runs),
                };
                let context_repo = path.clone();
                row = row.child(
                    button(format!("work-context-{i}"), context_label, false).on_click(
                        cx.listener(move |this, _, w, cx| {
                            this.project_tool(&context_repo, context_page, None, w, cx)
                        }),
                    ),
                );
                let target = path.clone();
                let ticket = work.ticket(path);
                row = row.child(
                    button(
                        format!("work-task-{i}"),
                        match stages[viewing].role {
                            StageRole::Plan => "任务 Spec",
                            StageRole::Build => "实现进度",
                            StageRole::Test => "验收与审查",
                            StageRole::Accept | StageRole::Deliver => "交付记录",
                        },
                        true,
                    )
                    .on_click(cx.listener(move |this, _, w, cx| {
                        this.project_tool(&target, Page::Local, Some(&ticket), w, cx)
                    })),
                );
            }
            row = row.child(
                button(
                    format!("work-toggle-{i}"),
                    if included {
                        "移出事项"
                    } else {
                        "纳入事项"
                    },
                    false,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.change_board(cx, |lib| {
                        let work = lib.project_mut()?.selected_mut()?;
                        if included {
                            work.repositories.retain(|r| r != &repo);
                        } else {
                            work.include(&repo);
                        }
                        Ok(())
                    }) {
                        this.refresh_work(cx);
                    }
                })),
            );
            repos = repos.child(row);
        }
        v_flex().flex_none().gap_3().child(detail).child(repos)
    }
}
