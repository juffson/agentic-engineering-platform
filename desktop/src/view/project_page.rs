use super::*;
mod agents;
use aep::project::{Kind, Library, Project, Work};
use aep::workflow::{self, Client, Profile, Role as StageRole, Template};
use anyhow::{Context as _, ensure};
use gpui_kit::component::input::{Textarea, TextareaState};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Library,
    Overview,
    Work,
    Repositories,
    Memory,
}
pub(crate) struct Board {
    library: Library,
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
    statuses: BTreeMap<PathBuf, String>,
    generation: u64,
}
impl Board {
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
        let mut library = dir
            .as_ref()
            .map_or_else(|| Ok(Library::default()), |d| Library::open(d))
            .unwrap_or_else(|e| {
                error = Some(format!("{e:#}"));
                Library::default()
            });
        if remember
            && error.is_none()
            && library.projects.is_empty()
            && let (Some(ws), Some(dir)) = (ws, dir.as_ref())
        {
            let name = basename(&ws.root);
            if let Err(e) = library.change(dir, |lib| lib.create(&name, vec![ws.root.clone()])) {
                error = Some(format!("{e:#}"));
            }
        }
        let section = if library.project().is_some() {
            Section::Overview
        } else {
            Section::Library
        };
        Self {
            library,
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
            statuses: BTreeMap::new(),
            generation: 0,
        }
    }
}
fn basename(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}
fn primary(id: &'static str, title: &'static str) -> ObservedElement<Stateful<Div>> {
    div()
        .id(id)
        .test_support()
        .role(Role::Button)
        .aria_label(title)
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
    pub(crate) fn adopt_first_repository(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.board.library.projects.is_empty()
            && self.board.error.is_none()
            && self.board.new_project.read(cx).value().trim().is_empty()
            && self.change_board(cx, |lib| lib.create(&basename(path), vec![path.to_owned()]))
        {
            self.board.section = Section::Overview;
        }
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
        self.board.section = section;
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
        self.section(section, w, cx);
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
        let mut value = self.board.note.read(cx).value().to_string();
        if advance {
            match self.reviewed_result(stage) {
                Ok(Some(result)) if value.trim().is_empty() => value = result,
                Ok(_) => (),
                Err(e) => {
                    self.board.error = Some(format!("{e:#}"));
                    cx.notify();
                    return;
                }
            }
        }
        if self.change_board(cx, |lib| {
            let work = lib.project_mut()?.selected_mut()?;
            ensure!(work.id == id, "事项已更新，请重新打开");
            if advance {
                ensure!(work.stage == stage, "仅可完成当前阶段");
                ensure!(!value.trim().is_empty(), "请先填写阶段结论");
                work.stage += 1;
            }
            work.notes[stage] = value;
            Ok(())
        }) && advance
        {
            self.load_stage(w, cx);
        }
    }
    fn create_work(&mut self, w: &mut Window, cx: &mut Context<Self>) {
        let name = self.board.new_work.read(cx).value().to_string();
        let kind = self.board.kind;
        let template = self.board.template;
        if self.change_board(cx, |lib| lib.project_mut()?.add_work(&name, kind, template)) {
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

    pub(super) fn context_work(&self) -> Option<&Work> {
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
                ));
        }
        if self.page != Page::Project {
            rail = rail.child(div().flex_1()).child(
                button("back-to-project", "返回开发事项", true)
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
        }
        if self.board.section == Section::Library || project.is_none() {
            page = page.child(self.library_view(cx));
        } else if let Some(p) = project {
            let mut head = v_flex().gap_1().child(caption(match self.board.section {
                Section::Overview => "项目概览",
                Section::Work => "项目 / 开发事项",
                Section::Memory => "项目 / 共享记忆",
                _ => "项目 / 关联仓库",
            }));
            let title = match self.board.section {
                Section::Overview => p.name.clone(),
                Section::Work => "开发事项".into(),
                Section::Memory => "项目记忆".into(),
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
                Section::Repositories => page = page.child(self.repositories(p, cx)),
                Section::Work => {
                    if self.board.creating_work {
                        page = page.child(self.work_form(cx));
                    } else if let Some(work) = p.selected() {
                        page = page.child(self.work_detail(p, work, cx));
                    }
                    page = page.child(self.work_list(p, cx));
                }
                Section::Library => {}
            }
        }
        h_flex().size_full().bg(rgb(CANVAS)).child(rail).child(page)
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
    fn work_form(&self, cx: &Context<Self>) -> Div {
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
            templates = templates.child(
                button(
                    format!("work-template-{template:?}"),
                    template.label(),
                    self.board.template == template,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.board.template = template;
                    cx.notify();
                })),
            );
        }
        panel()
            .flex_none()
            .child(heading("新建开发事项"))
            .child(templates)
            .child(caption(
                self.board
                    .template
                    .stages()
                    .iter()
                    .map(|s| s.name)
                    .collect::<Vec<_>>()
                    .join("  ›  "),
            ))
            .child(types)
            .child(input_field("project-work-name", &self.board.new_work))
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
                item(format!("project-item-{id}"), false)
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
            for (label, page) in [
                ("本地任务", Page::Local),
                ("运行记录", Page::Runs),
                ("共享记忆", Page::Memory),
                ("两轮接力", Page::Relay),
                ("隔离全流程", Page::Lifecycle),
                ("HTTPS 试点", Page::HttpsPilot),
            ] {
                let path = path.clone();
                tools = tools.child(
                    button(format!("project-tool-{i}-{label}"), label, false).on_click(
                        cx.listener(move |this, _, w, cx| {
                            this.project_tool(&path, page, None, w, cx)
                        }),
                    ),
                );
            }
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
        let mut steps = h_flex().gap_2().flex_wrap();
        let stages = work.template.stages();
        let viewing = self.board.viewing.min(stages.len() - 1);
        for (i, stage) in stages.iter().enumerate() {
            let profile = work.profile(i);
            let state = if i < work.stage {
                "✓ "
            } else if i == work.stage {
                "● "
            } else {
                ""
            };
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
            steps = steps.child(
                v_flex()
                    .id(format!("project-stage-{i}"))
                    .test_support()
                    .role(Role::Button)
                    .aria_label(format!("{} · {model}", stage.name))
                    .tab_stop(true)
                    .cursor_pointer()
                    .min_w(px(92.))
                    .flex_1()
                    .gap_1()
                    .py_2()
                    .px_2()
                    .rounded_md()
                    .bg(rgb(if i == viewing { SELECTED } else { CANVAS }))
                    .border_t_2()
                    .border_color(rgb(if i == viewing {
                        MINT
                    } else if i <= work.stage {
                        tint
                    } else {
                        LINE
                    }))
                    .child(t(format!("{state}{}", stage.name), tint).font_semibold())
                    .child(caption(model).truncate())
                    .hover(|s| s.bg(rgb(SELECTED)))
                    .on_click(cx.listener(move |this, _, w, cx| this.show_stage(i, w, cx))),
            );
        }
        let mut detail = panel()
            .flex_none()
            .child(
                h_flex()
                    .gap_2()
                    .child(chip(work.template.label(), BLUE))
                    .child(heading(work.name.clone()))
                    .child(chip(work.stage_name(), MINT)),
            )
            .child(steps)
            .child(heading(stages[viewing].name))
            .child(caption(stages[viewing].brief))
            .child(
                div().id("project-stage-note").test_support().child(
                    Textarea::new(&self.board.note)
                        .aria_label("阶段记录")
                        .h(px(82.)),
                ),
            );
        let mut actions = h_flex().gap_2().child(
            button("project-save-note", "保存记录", false)
                .on_click(cx.listener(|this, _, w, cx| this.save_stage(false, w, cx))),
        );
        if viewing == work.stage {
            actions = actions.child(
                primary(
                    "project-next",
                    if viewing + 1 == stages.len() {
                        "确认完成交付"
                    } else {
                        "完成并进入下一阶段"
                    },
                )
                .on_click(cx.listener(|this, _, w, cx| this.save_stage(true, w, cx))),
            );
        }
        if viewing < work.stage {
            actions = actions.child(
                button("project-reopen-stage", "返回此阶段跟进", false).on_click(cx.listener(
                    move |this, _, w, cx| {
                        if this.has_running_agent() {
                            this.board.error = Some("请先等待当前 CLI 完成，或取消执行".into());
                            cx.notify();
                            return;
                        }
                        if this.change_board(cx, |lib| {
                            let item = lib.project_mut()?.selected_mut()?;
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
        detail = detail
            .child(actions)
            .child(self.agent_panel(p, work, viewing, cx));
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
