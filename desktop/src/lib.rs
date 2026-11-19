mod cleanup;
mod github_pr;
pub mod lifecycle;
mod publish;
mod recent;
mod relay;
mod task_queue;
mod view;
pub use relay::Runner;

use aep::{
    studio::{Artifact, Snapshot, preview},
    workspace::Workspace,
};
use anyhow::Result;
use gpui_kit::{
    component::{
        Theme, ThemeMode,
        input::{InputEvent, InputState},
    },
    *,
};
use serde_json::Value;
use std::{collections::HashSet, path::PathBuf, time::Duration};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Runs,
    Memory,
    Relay,
    Review,
    PullRequest,
    Lifecycle,
}
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Filter {
    #[default]
    All,
    Pending,
    Attention,
    Current,
    History,
}

fn string<'a>(value: &'a Value, field: &str) -> &'a str {
    value[field].as_str().unwrap_or("")
}

fn pending(status: &str) -> bool {
    status == "awaiting_human_review"
}
fn attention(status: &str) -> bool {
    matches!(
        status,
        "failed" | "cancelled" | "needs_reconciliation" | "unstarted"
    )
}

struct Studio {
    editor: Option<view::editor::Editor>,
    workspace: Option<Workspace>,
    snapshot: Snapshot,
    page: Page,
    filter: Filter,
    run: Option<String>,
    note: Option<i64>,
    search: Entity<InputState>,
    _subscription: Subscription,
    artifact: Option<(String, Artifact, String)>,
    loading: bool,
    generation: u64,
    repo_error: Option<String>,
    runner: Runner,
    relays: Vec<relay::Relay>,
    relay_id: Option<String>,
    relay_tasks: Vec<relay::Task>,
    relay_task: Option<String>,
    relay_model: Entity<InputState>,
    relay_error: Option<String>,
    tasks_loading: bool,
    tasks_loaded: bool,
    relay_launching: bool,
    relay_polling: bool,
    workspace_generation: u64,
    remember_repository: bool,
    review: Option<view::human_review::State>,
    pull_request: Option<view::github_page::State>,
    lifecycle: Option<lifecycle::Fixture>,
    task_authoring: Option<view::task_authoring::Authoring>,
    lifecycle_preparing: bool,
    lifecycle_loading: bool,
    lifecycle_choices: Option<lifecycle::Choices>,
    lifecycle_packaging: bool,
    lifecycle_error: Option<String>,
    lifecycle_doc: Option<PathBuf>,
    lifecycle_shots: HashSet<&'static str>,
    task_sessions: Vec<lifecycle::catalog::Entry>,
    task_sessions_loading: bool,
    task_sessions_error: Option<String>,
    queue_source: Option<Workspace>,
    task_queue: task_queue::Queue,
    queue_polling: bool,
    queue_driving: bool,
    queue_generation: u64,
    queue_error: Option<String>,
    queue_cancel_confirm: Option<(uuid::Uuid, task_queue::State)>,
}

impl Studio {
    fn new(
        workspace: Option<Workspace>,
        runner: Runner,
        remember_repository: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("搜索 ID、任务或内容"));
        let sub = cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let lifecycle = workspace.as_ref().and_then(lifecycle::load);
        let mut studio = Self {
            editor: None,
            workspace,
            snapshot: Snapshot::default(),
            page: Page::Runs,
            filter: Filter::All,
            run: None,
            note: None,
            search,
            _subscription: sub,
            artifact: None,
            loading: false,
            generation: 0,
            repo_error: None,
            runner,
            relays: Vec::new(),
            relay_id: None,
            relay_tasks: Vec::new(),
            relay_task: None,
            relay_model: cx
                .new(|cx| InputState::new(window, cx).placeholder("由 Codex 本机配置决定")),
            relay_error: None,
            tasks_loading: false,
            tasks_loaded: false,
            relay_launching: false,
            relay_polling: false,
            workspace_generation: 0,
            remember_repository,
            review: None,
            pull_request: None,
            lifecycle,
            task_authoring: None,
            lifecycle_preparing: false,
            lifecycle_loading: false,
            lifecycle_choices: None,
            lifecycle_packaging: false,
            lifecycle_error: None,
            lifecycle_doc: None,
            lifecycle_shots: HashSet::new(),
            task_sessions: Vec::new(),
            task_sessions_loading: false,
            task_sessions_error: None,
            queue_source: None,
            task_queue: task_queue::Queue::default(),
            queue_polling: false,
            queue_driving: false,
            queue_generation: 0,
            queue_error: None,
            queue_cancel_confirm: None,
        };
        if studio.remember_repository && studio.workspace.is_some() {
            studio.remember_workspace();
            studio.navigate(Page::Lifecycle, window, cx);
        }
        studio.attach_task_queue(cx);
        studio.reload(cx);
        studio
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        let Some(ws) = self.workspace.clone() else {
            return;
        };
        self.loading = true;
        self.generation += 1;
        let generation = self.generation;
        let task = cx.background_executor().spawn(async move {
            let relays = relay::load(&ws);
            (Snapshot::load(&ws), relays)
        });
        cx.spawn(async move |this, cx| {
            let (snapshot, relays) = task.await;
            let _ = this.update(cx, |this, cx| {
                if generation != this.generation {
                    return;
                }
                this.snapshot = snapshot;
                match relays {
                    Ok(relays) => this.relays = relays,
                    Err(error) => this.snapshot.issues.push(format!("两轮接力：{error:#}")),
                }
                this.loading = false;
                if this.relays.iter().any(|relay| relay.active(&this.snapshot)) {
                    this.poll_relays(cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn choose_repo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.lifecycle_preparing || self.lifecycle_packaging {
            return;
        }
        let page = if self.page == Page::Lifecycle {
            Page::Lifecycle
        } else {
            Page::Runs
        };
        let dialog = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("打开 Git 仓库".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let selection: Result<_> = async {
                dialog
                    .await??
                    .and_then(|v| v.into_iter().next())
                    .map(|p| Workspace::open(&p))
                    .transpose()
            }
            .await;
            let _ = this.update_in(cx, |this, window, cx| match selection {
                Ok(None) => (),
                Ok(Some(ws)) => this.set_workspace(ws, page, window, cx),
                Err(error) => {
                    this.repo_error = Some(format!("打开仓库失败：{error:#}"));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn set_workspace(
        &mut self,
        ws: Workspace,
        page: Page,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspace_generation += 1;
        self.lifecycle = lifecycle::load(&ws);
        self.task_authoring = None;
        self.lifecycle_doc = None;
        self.lifecycle_error = None;
        self.lifecycle_preparing = false;
        self.lifecycle_loading = false;
        self.lifecycle_choices = None;
        self.lifecycle_shots.clear();
        self.queue_cancel_confirm = None;
        self.task_sessions.clear();
        self.task_sessions_loading = false;
        self.task_sessions_error = None;
        self.workspace = Some(ws);
        self.snapshot = Snapshot::default();
        self.editor = None;
        self.run = None;
        self.note = None;
        self.repo_error = None;
        if self.remember_repository {
            self.remember_workspace();
        }
        self.relays.clear();
        self.relay_id = None;
        self.relay_tasks.clear();
        self.relay_task = None;
        self.relay_error = None;
        self.tasks_loading = false;
        self.tasks_loaded = false;
        self.relay_launching = false;
        self.review = None;
        self.pull_request = None;
        self.relay_model
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.loading = false;
        self.attach_task_queue(cx);
        self.navigate(page, window, cx);
        self.reload(cx);
    }

    fn remember_workspace(&mut self) {
        if let Some(ws) = &self.workspace {
            let path = self
                .lifecycle
                .as_ref()
                .map_or(&ws.root, |fixture| &fixture.source);
            if let Err(error) = recent::remember(path) {
                self.repo_error = Some(format!("保存上次仓库失败：{error:#}"));
            }
        }
    }

    fn navigate(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.page = page;
        self.filter = Filter::All;
        self.artifact = None;
        self.search
            .update(cx, |input, cx| input.set_value("", window, cx));
        if page == Page::Relay {
            self.reload(cx);
            if !self.tasks_loaded {
                self.load_relay_tasks(cx);
            }
        }
        if page == Page::Lifecycle && self.lifecycle_choices.is_none() {
            self.load_lifecycle_choices(cx);
        }
        if page == Page::Lifecycle {
            self.load_task_sessions(cx);
        }
        cx.notify();
    }

    fn load_relay_tasks(&mut self, cx: &mut Context<Self>) {
        if self.tasks_loading {
            return;
        }
        let Some(ws) = self.workspace.clone() else {
            return;
        };
        self.tasks_loading = true;
        self.relay_error = None;
        let generation = self.workspace_generation;
        let work = cx
            .background_executor()
            .spawn(async move { relay::tasks(&ws) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.workspace_generation != generation {
                    return;
                }
                this.tasks_loading = false;
                this.tasks_loaded = true;
                match result {
                    Ok(mut tasks) => {
                        if let Some(test) = &this.lifecycle {
                            tasks.retain(|task| task.name == test.task && task.sha == test.spec);
                        }
                        this.relay_tasks = tasks;
                    }
                    Err(error) => this.relay_error = Some(format!("读取派发任务失败：{error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn poll_relays(&mut self, cx: &mut Context<Self>) {
        if self.relay_polling {
            return;
        }
        self.relay_polling = true;
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            loop {
                executor.timer(Duration::from_millis(800)).await;
                let keep_waiting = this
                    .update(cx, |this, cx| {
                        let active = this.relay_launching
                            || this.relays.iter().any(|relay| relay.active(&this.snapshot));
                        if active {
                            this.reload(cx);
                        } else {
                            this.relay_polling = false;
                        }
                        active
                    })
                    .unwrap_or(false);
                if !keep_waiting {
                    break;
                }
            }
        })
        .detach();
    }

    fn start_relay(&mut self, task: relay::Task, cx: &mut Context<Self>) {
        if self.relay_launching {
            return;
        }
        if let Some(session) = self.lifecycle.as_ref().filter(|session| session.real_task) {
            self.enqueue_round(session.initial_relay_id(), task_queue::Action::Start, cx);
            return;
        }
        let id = relay::new_id();
        self.relay_id = Some(id.clone());
        self.relay_task = None;
        self.submit_relay(cx, move |ws, runner, model| {
            relay::start(ws, runner, id, task, model)
        });
    }

    fn advance_relay(&mut self, id: String, action: task_queue::Action, cx: &mut Context<Self>) {
        if self
            .lifecycle
            .as_ref()
            .is_some_and(|session| session.real_task)
        {
            self.enqueue_round(id, action, cx);
            return;
        }
        let decision = match action {
            task_queue::Action::Second { decision } => Some(decision),
            _ => None,
        };
        self.submit_relay(cx, move |ws, runner, model| {
            relay::advance(ws, runner, &id, action.round(), decision, model)
        });
    }

    fn submit_relay(
        &mut self,
        cx: &mut Context<Self>,
        run: impl FnOnce(&Workspace, &Runner, String) -> Result<()> + Send + 'static,
    ) {
        if self.relay_launching {
            return;
        }
        let Some(ws) = self.workspace.clone() else {
            return;
        };
        let runner = self.runner.clone();
        let model = self.relay_model.read(cx).value().trim().to_owned();
        let generation = self.workspace_generation;
        self.relay_launching = true;
        self.relay_error = None;
        self.poll_relays(cx);
        let job = cx
            .background_executor()
            .spawn(async move { run(&ws, &runner, model) });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |this, cx| {
                if this.workspace_generation != generation {
                    return;
                }
                this.relay_launching = false;
                this.relay_error = result.err().map(|error| format!("{error:#}"));
                // Supersede any poll that began before the child completed.
                this.loading = false;
                this.reload(cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn show_artifact(&mut self, id: &str, kind: Artifact, cx: &mut Context<Self>) {
        let Some(ws) = &self.workspace else { return };
        let text = match preview(&ws.data, id, kind) {
            Ok(Some(p)) if p.truncated => format!("仅显示末尾 48 KiB\n\n{}", p.text),
            Ok(Some(p)) if p.text.trim().is_empty() => "文件为空".to_owned(),
            Ok(Some(p)) => p.text,
            Ok(None) => "此次运行尚未生成这份文件。".to_owned(),
            Err(e) => format!("读取失败：{e:#}"),
        };
        self.artifact = Some((id.to_owned(), kind, text));
        cx.notify();
    }
}

pub fn init(cx: &mut App) {
    gpui_kit::init(cx);
    Theme::change(ThemeMode::Dark, None, cx);
}

/// Open Studio using the caller's GPUI platform and window options.
pub fn open_studio(
    ws: Option<Workspace>,
    options: WindowOptions,
    cx: &mut App,
) -> Result<AnyWindowHandle> {
    let ws = ws
        .or_else(recent::restore)
        .or_else(|| Workspace::open(std::path::Path::new(".")).ok());
    open_window(ws, Runner::default(), true, options, cx)
}

pub fn open_studio_with_runner(
    ws: Option<Workspace>,
    runner: Runner,
    options: WindowOptions,
    cx: &mut App,
) -> Result<AnyWindowHandle> {
    open_window(ws, runner, false, options, cx)
}

fn open_window(
    ws: Option<Workspace>,
    runner: Runner,
    remember_repository: bool,
    options: WindowOptions,
    cx: &mut App,
) -> Result<AnyWindowHandle> {
    let (window, _) = gpui_kit::open_window(options, cx, move |window, cx| {
        window.set_window_title("AEP Studio");
        cx.new(|cx| Studio::new(ws, runner, remember_repository, window, cx))
    })?;
    Ok(window)
}
