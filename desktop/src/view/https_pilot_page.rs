use super::task_authoring::Authoring;
use super::*;
use crate::{
    Page,
    https_pilot::{Artifact as Proof, Connection, Progress, Session, TaskSource},
    lifecycle::{self, Choices},
};
use anyhow::{Context as _, ensure};

pub(crate) struct RemoteForm {
    source: Workspace,
    endpoint: Entity<InputState>,
    ca: Entity<InputState>,
    operator_file: Entity<InputState>,
    choices: Option<Choices>,
    authoring: Option<Authoring>,
}

impl RemoteForm {
    fn connection(&self, cx: &App) -> Result<Connection> {
        let endpoint = self.endpoint.read(cx).value().trim().to_owned();
        let ca = self.ca.read(cx).value().trim().to_owned();
        let token = self.operator_file.read(cx).value().trim().to_owned();
        ensure!(
            endpoint.starts_with("https://"),
            "远程控制面需要 HTTPS 地址"
        );
        let operator_file = PathBuf::from(token);
        ensure!(
            operator_file.is_absolute() && operator_file.is_file(),
            "请选择操作员令牌文件的绝对路径"
        );
        let ca = (!ca.is_empty()).then(|| PathBuf::from(ca));
        ensure!(
            ca.as_ref()
                .is_none_or(|path| path.is_absolute() && path.is_file()),
            "请填写 CA 证书文件的绝对路径"
        );
        Ok(Connection {
            endpoint,
            ca,
            operator_file,
        })
    }
}

const PROOFS: [(Proof, &str, &str); 8] = [
    (Proof::Log, "log", "运行日志"),
    (Proof::Diff, "diff", "候选 Diff"),
    (Proof::Checks, "checks", "验收日志"),
    (Proof::Review, "review", "Claude 审查"),
    (Proof::FirstRun, "first", "断线前证据"),
    (Proof::RecoveryRun, "recovery", "恢复后证据"),
    (Proof::CiLog, "ci", "CI 验收日志"),
    (Proof::Result, "result", "运行回执"),
];

fn phase(stage: &str) -> (usize, &'static str) {
    match stage {
        "preparation" => (0, "构建镜像并固定隔离分支"),
        "https" => (0, "启动 HTTPS 控制面并校验证书"),
        "candidate" => (1, "Codex 生成候选并进入独立验收"),
        "remote_preflight" => (0, "校验 HTTPS、远程 Worker 和固定版本"),
        "remote_create" => (0, "创建并固定任务 Spec"),
        "remote_dispatch" => (0, "通过 HTTPS 派发真实任务"),
        "queue_wait" => (0, "等待 HTTPS Worker 领取"),
        "remote_cancelling" => (1, "等待 Worker 确认停止"),
        "cancelled" => (5, "任务已取消，证据已归档"),
        "remote_checking" => (1, "远程 Worker 正在验收固定候选"),
        "remote_reviewing" => (1, "远程 Claude 正在审查固定候选"),
        "remote_outage" => (2, "HTTPS 连接中断，等待控制面恢复"),
        "remote_retry" => (2, "使用原 Spec 重新派发实现"),
        "outage" => (2, "HTTPS 入口已暂停，等待 Lease 过期"),
        "reconnect" => (2, "HTTPS 已恢复，核对过期发布拒绝和事件回放"),
        "recovery" => (2, "对同一候选重新验收，运行 Claude 审查"),
        "publish_confirmation" => (3, "待人工确认发布"),
        "publication" => (3, "发布固定候选并创建 PR"),
        "ci" => (4, "等待并核对 GitHub CI"),
        "merge_confirmation" => (4, "CI 已通过，待人工确认合并"),
        "merge" => (4, "合并已确认的 PR"),
        "cleanup" => (5, "同步目标分支并清理候选与工作区"),
        "completed" => (5, "已合并并归档"),
        _ => (0, "检查 Docker 和登录凭证"),
    }
}

fn progress_state(progress: &Progress, busy: bool, spawn_error: bool) -> (&'static str, u32) {
    match progress.exit_code {
        Some(0) if progress.stage == "completed" && progress.archive => ("已完成", MINT),
        Some(0) if progress.stage == "cancelled" => ("已取消", AMBER),
        Some(3) => ("已停止", AMBER),
        Some(4) => ("已暂停", AMBER),
        Some(_) => ("失败", RED),
        None if spawn_error => ("启动失败", RED),
        None if busy => ("运行中", BLUE),
        None if progress.remote => ("可恢复", AMBER),
        _ => ("试点未完成", AMBER),
    }
}

fn check_state(check: &Value) -> (&str, u32) {
    match string(check, "conclusion") {
        "SUCCESS" => ("通过", MINT),
        "SKIPPED" => ("已跳过", MUTED),
        "NEUTRAL" => ("中性", MUTED),
        "" => match string(check, "state") {
            "SUCCESS" => ("通过", MINT),
            "FAILURE" | "ERROR" => ("失败", RED),
            _ if string(check, "status") == "IN_PROGRESS" => ("运行中", BLUE),
            _ => ("等待", AMBER),
        },
        result => (result, RED),
    }
}

impl Studio {
    fn https_source(&self) -> Option<Workspace> {
        let ws = self.workspace.as_ref()?;
        self.lifecycle
            .as_ref()
            .and_then(|fixture| Workspace::open(&fixture.source).ok())
            .or_else(|| Some(ws.clone()))
    }

    fn select_https_session(&mut self, source: &Workspace, root: PathBuf) {
        if self
            .https_pilot
            .as_ref()
            .is_some_and(|session| session.root == root)
        {
            return;
        }
        if let Some(previous) = self.https_pilot.take().filter(Session::active) {
            self.https_background.push(previous);
        }
        self.https_pilot = if let Some(index) = self
            .https_background
            .iter()
            .position(|session| session.root == root)
        {
            Some(self.https_background.swap_remove(index))
        } else {
            Session::open(source, root)
        };
    }

    pub(crate) fn load_https_pilot(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.https_source() else {
            return;
        };
        if let Some(session) = self
            .https_pilot
            .as_mut()
            .filter(|s| s.source == source.root)
        {
            session.refresh();
        } else {
            if let Some(previous) = self.https_pilot.take().filter(Session::active) {
                self.https_background.push(previous);
            }
            if let Some(root) = Session::latest(&source) {
                self.select_https_session(&source, root);
            }
            self.https_pilot_error = None;
        }
        self.https_sessions = Session::entries(&source);
        self.restore_https_queue(&source, cx);
        self.poll_https(cx);
    }

    fn start_https_pilot(&mut self, cx: &mut Context<Self>) {
        if self.https_pilot.as_ref().is_some_and(Session::active) {
            return;
        }
        let Some(source) = self.https_source() else {
            return;
        };
        let result = Session::launch(&source, &self.runner);
        self.attach_https_pilot(result, cx);
    }

    fn resume_https(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.https_source() else {
            return;
        };
        if self
            .https_pilot
            .as_ref()
            .is_none_or(|s| s.source != source.root || !s.can_resume())
        {
            return;
        }
        let session = self.https_pilot.take().expect("selected resumable task");
        let root = session.root.clone();
        let result = session.resume(&source, &self.runner);
        if result.is_err() {
            self.https_pilot = Session::open(&source, root);
        }
        self.attach_https_pilot(result, cx);
    }

    pub(super) fn follow_https_cancellation(&mut self, ticket: &str, cx: &mut Context<Self>) {
        if let Some(source) = self.https_source()
            && let Some(entry) = Session::entries(&source)
                .into_iter()
                .find(|entry| entry.progress.ticket == ticket)
        {
            self.select_https_session(&source, entry.root);
            self.resume_https(cx);
        }
    }

    fn attach_https_pilot(&mut self, result: Result<Session>, cx: &mut Context<Self>) {
        self.https_pilot_error = None;
        match result {
            Ok(session) => {
                if let Some(previous) = self.https_pilot.replace(session).filter(Session::active) {
                    self.https_background.push(previous);
                }
            }
            Err(error) => self.https_pilot_error = Some(format!("启动失败：{error:#}")),
        }
        if let Some(source) = self.https_source() {
            self.https_sessions = Session::entries(&source);
        }
        self.poll_https(cx);
        cx.notify();
    }

    fn poll_https(&mut self, cx: &mut Context<Self>) {
        if self.https_polling {
            return;
        }
        self.https_polling = true;
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            loop {
                executor.timer(Duration::from_millis(750)).await;
                let keep_polling = this
                    .update(cx, |this, cx| {
                        for session in &mut this.https_background {
                            session.refresh();
                        }
                        this.https_background.retain(Session::active);
                        if let Some(session) = this.https_pilot.as_mut() {
                            let active = session.active();
                            let was_gate = session.gate();
                            session.refresh();
                            if session.gate() != was_gate {
                                this.https_pilot_error = None;
                                if let Some(gate) = session.gate() {
                                    session.select(if gate.pr.is_some() {
                                        Proof::CiLog
                                    } else {
                                        Proof::Diff
                                    });
                                }
                            }
                            if active && !session.active() && session.archive().is_some() {
                                session.select(Proof::Result);
                            }
                        }
                        let visible = this.page == Page::HttpsPilot;
                        if visible {
                            if let Some(source) = this.https_source() {
                                this.https_sessions = Session::entries(&source);
                            }
                            this.poll_https_queue(cx);
                            cx.notify();
                        }
                        let keep_polling = visible
                            || !this.https_background.is_empty()
                            || this.https_pilot.as_ref().is_some_and(Session::active);
                        this.https_polling = keep_polling;
                        keep_polling
                    })
                    .unwrap_or(false);
                if !keep_polling {
                    break;
                }
            }
        })
        .detach();
    }

    pub(crate) fn open_https_remote(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(source) = self.https_source() else {
            return;
        };
        let mut input = |name, placeholder| {
            let value = std::env::var(name).unwrap_or_default();
            cx.new(|cx| {
                let mut state = InputState::new(window, cx).placeholder(placeholder);
                state.set_value(&value, window, cx);
                state
            })
        };
        self.https_remote_form = Some(RemoteForm {
            source,
            endpoint: input("AEP_STUDIO_CONTROL_URL", "https://control.example.org"),
            ca: input("AEP_STUDIO_CONTROL_CA", "系统信任的证书可留空"),
            operator_file: input("AEP_STUDIO_CONTROL_OPERATOR_FILE", "/path/to/operator"),
            choices: None,
            authoring: None,
        });
        self.load_https_remote_choices(cx);
    }

    fn load_https_remote_choices(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.https_remote_form.as_mut() else {
            return;
        };
        self.https_pilot_error = None;
        form.choices = None;
        let source = form.source.clone();
        let runner = self.runner.clone();
        let generation = self.workspace_generation;
        let task = cx
            .background_executor()
            .spawn(async move { lifecycle::choices(&source, &runner) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.workspace_generation == generation
                    && let Some(form) = this.https_remote_form.as_mut()
                {
                    match result {
                        Ok(choices) => form.choices = Some(choices),
                        Err(error) => {
                            this.https_pilot_error =
                                Some(format!("读取 GitHub 任务和分支失败：{error:#}"))
                        }
                    }
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn connect_remote_queue(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.https_remote_form.as_ref() else {
            return;
        };
        let source = form.source.clone();
        let result = form
            .connection(cx)
            .and_then(|connection| self.set_https_queue(source, connection, cx));
        self.https_pilot_error = result.err().map(|error| format!("{error:#}"));
        cx.notify();
    }

    fn start_https_remote(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.https_remote_form.as_ref() else {
            return;
        };
        let prepared = (|| {
            let connection = form.connection(cx)?;
            let (task, branch) = if let Some(authoring) = &form.authoring {
                (
                    TaskSource::Draft(authoring.draft(cx)?),
                    authoring.branch.clone(),
                )
            } else {
                let choices = form.choices.as_ref().context("请先加载仓库分支")?;
                (
                    TaskSource::Existing(choices.task.clone().context("请选择已派发的任务")?),
                    choices.branch.clone().context("请选择目标分支")?,
                )
            };
            Ok::<_, anyhow::Error>((form.source.clone(), task, branch, connection))
        })();
        let result = prepared.and_then(|(source, task, branch, connection)| {
            self.set_https_queue(source.clone(), connection.clone(), cx)?;
            Session::launch_remote(&source, &self.runner, task, branch, connection)
        });
        if result.is_ok() {
            self.https_remote_form = None;
        }
        self.attach_https_pilot(result, cx);
    }

    fn open_https_authoring(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(form) = self.https_remote_form.as_mut()
            && let Some(branch) = form
                .choices
                .as_ref()
                .and_then(|choices| choices.branch.clone())
        {
            form.authoring = Some(Authoring::new(branch, window, cx));
            self.https_pilot_error = None;
            cx.notify();
        }
    }

    fn https_remote_form(&self, form: &RemoteForm, cx: &Context<Self>) -> Div {
        let input = |label, state| {
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(caption(label))
                .child(Input::new(state).aria_label(label))
        };
        let mut contents = panel()
            .flex_none()
            .child(heading(if form.authoring.is_some() {
                "创建 HTTPS 真实任务"
            } else {
                "HTTPS 真实任务"
            }))
            .child(field("仓库", &form.source.root.display().to_string()))
            .when(form.authoring.is_none(), |contents| {
                contents.child(
                    h_flex()
                        .gap_2()
                        .child(button("https-change-repo", "选择仓库…", false).on_click(
                            cx.listener(|this, _, window, cx| this.choose_repo(window, cx)),
                        ))
                        .child(
                            button("https-refresh-choices", "刷新任务和分支", false).on_click(
                                cx.listener(|this, _, _, cx| this.load_https_remote_choices(cx)),
                            ),
                        ),
                )
            })
            .child(input("HTTPS 控制面", &form.endpoint))
            .child(
                h_flex()
                    .gap_3()
                    .child(input("CA 证书路径", &form.ca))
                    .child(input("操作员令牌文件路径", &form.operator_file)),
            )
            .child(
                button("https-connect-queue", "连接队列", false)
                    .on_click(cx.listener(|this, _, _, cx| this.connect_remote_queue(cx))),
            );
        if let Some(authoring) = &form.authoring {
            return contents.child(authoring.fields(&form.source, false)).child(
                h_flex()
                    .gap_2()
                    .child(
                        button("https-create-and-dispatch", "创建并派发给远程 Worker", true)
                            .on_click(cx.listener(|this, _, _, cx| this.start_https_remote(cx))),
                    )
                    .child(
                        button("https-cancel-authoring", "返回任务选择", false).on_click(
                            cx.listener(|this, _, _, cx| {
                                if let Some(form) = this.https_remote_form.as_mut() {
                                    form.authoring = None;
                                    this.https_pilot_error = None;
                                    cx.notify();
                                }
                            }),
                        ),
                    ),
            );
        }
        if let Some(choices) = &form.choices {
            let mut tasks = h_flex().gap_2().flex_wrap();
            for task in &choices.tasks {
                let task = task.clone();
                tasks = tasks.child(
                    button(
                        format!("https-task-{}", task.name),
                        task.name.clone(),
                        choices.task.as_ref().is_some_and(|selected| {
                            selected.sha == task.sha && selected.name == task.name
                        }),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(choices) = this
                            .https_remote_form
                            .as_mut()
                            .and_then(|form| form.choices.as_mut())
                        {
                            choices.task = Some(task.clone());
                            cx.notify();
                        }
                    })),
                );
            }
            let mut branches = h_flex().gap_2().flex_wrap();
            for branch in &choices.branches {
                let branch = branch.clone();
                branches = branches.child(
                    button(
                        format!("https-target-{}", branch.name),
                        branch.name.clone(),
                        choices
                            .branch
                            .as_ref()
                            .is_some_and(|selected| selected.name == branch.name),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(choices) = this
                            .https_remote_form
                            .as_mut()
                            .and_then(|form| form.choices.as_mut())
                        {
                            choices.branch = Some(branch.clone());
                            cx.notify();
                        }
                    })),
                );
            }
            contents = contents
                .child(caption("选择已派发的任务"))
                .child(
                    div()
                        .id("https-task-list")
                        .test_support()
                        .max_h(px(125.))
                        .overflow_y_scroll()
                        .child(tasks),
                )
                .child(caption("选择 GitHub 目标分支"))
                .child(
                    div()
                        .id("https-target-list")
                        .test_support()
                        .max_h(px(125.))
                        .overflow_y_scroll()
                        .child(branches),
                );
            if let (Some(task), Some(branch)) = (&choices.task, &choices.branch) {
                contents = contents
                    .child(field("Spec SHA", &task.sha))
                    .child(field("目标 SHA", &branch.sha))
                    .child(
                        button("https-dispatch", "派发给远程 Worker", true)
                            .on_click(cx.listener(|this, _, _, cx| this.start_https_remote(cx))),
                    );
            }
            if choices.branch.is_some() {
                contents = contents.child(
                    button("https-open-authoring", "创建 HTTPS 任务…", true).on_click(
                        cx.listener(|this, _, window, cx| this.open_https_authoring(window, cx)),
                    ),
                );
            }
        } else if self.https_pilot_error.is_none() {
            contents = contents.child(caption("正在读取 GitHub 任务和目标分支"));
        }
        contents
    }

    fn https_session_list(&self, cx: &Context<Self>) -> Div {
        let panel = panel().flex_none().child(heading("仓库的 HTTPS 任务"));
        let mut list = v_flex()
            .id("https-session-list")
            .test_support()
            .max_h(px(230.))
            .overflow_y_scroll()
            .gap_2();
        if self.https_sessions.is_empty() {
            return panel.child(caption("此仓库还没有 HTTPS 真实任务。"));
        }
        for entry in &self.https_sessions {
            let root = entry.root.clone();
            let id = root.file_name().unwrap_or_default().to_string_lossy();
            let task = &entry.progress.task;
            let branch = &entry.progress.branch;
            let (_, message) = phase(&entry.progress.stage);
            let (status, color) = progress_state(&entry.progress, entry.busy, false);
            let current = self
                .https_pilot
                .as_ref()
                .is_some_and(|session| session.root == root);
            let mut row = v_flex()
                .gap_1()
                .child(
                    h_flex()
                        .justify_between()
                        .gap_2()
                        .child(
                            button(format!("https-session-{id}"), task.clone(), current).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    if let Some(source) = this.https_source() {
                                        this.https_pilot_error = None;
                                        this.select_https_session(&source, root.clone());
                                        cx.notify();
                                    }
                                }),
                            ),
                        )
                        .child(chip(status, color)),
                )
                .child(caption(format!("{branch} · {message}")));
            if !entry.progress.reason.is_empty() {
                row = row.child(
                    caption(&entry.progress.reason)
                        .max_h(px(44.))
                        .overflow_hidden(),
                );
            }
            list = list.child(row.flex_none().pb_2());
        }
        panel.child(list)
    }

    pub(super) fn https_pilot_page(&self, cx: &Context<Self>) -> Div {
        let session = self.https_pilot.as_ref();
        let running = session.is_some_and(Session::active);
        let header = h_flex()
            .justify_between()
            .gap_3()
            .child(heading("HTTPS 试点").text_2xl())
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        button(
                            "https-real-mode",
                            "真实任务",
                            self.https_remote_form.is_some(),
                        )
                        .on_click(cx.listener(|this, _, w, cx| this.open_https_remote(w, cx))),
                    )
                    .when(!running, |row| {
                        row.child(
                            button("https-start", "启动隔离试点", true)
                                .on_click(cx.listener(|this, _, _, cx| this.start_https_pilot(cx))),
                        )
                    }),
            );
        let mut page = v_flex()
            .id("https-pilot-page")
            .test_support()
            .flex_1()
            .min_w_0()
            .h_full()
            .min_h_0()
            .overflow_y_scroll()
            .gap_3()
            .p_5()
            .child(header)
            .child(caption("在独立 HTTPS 站点和 GitHub 测试分支运行 Codex、Claude 与断线恢复。发布和合并分别确认。"));
        if let Some(error) = &self.https_pilot_error {
            page = page.child(detail_text("https-action-error", error).text_color(rgb(RED)));
        }
        if let Some(form) = &self.https_remote_form {
            page = page.child(self.https_remote_form(form, cx));
        }
        if let Some(site) = &self.https_queue {
            page = page.child(self.https_queue_panel(site, cx));
        }
        page = page.child(self.https_session_list(cx));
        let Some(session) = session else {
            return h_flex()
                .size_full()
                .bg(rgb(CANVAS))
                .child(self.rail(cx))
                .child(page.child(panel().child(t("从本地 AEP 源码仓库启动 deploy/pilot/live.sh。需要 Docker 和已登录的 GitHub、Codex 与 Claude。", FG))));
        };
        let progress = &session.progress;
        let (index, message) = phase(&progress.stage);
        let (result, color) =
            progress_state(progress, running || session.busy, session.error.is_some());
        let mut steps = h_flex().gap_3().flex_wrap();
        for (i, label) in [
            "部署 HTTPS",
            "生成候选",
            "断线恢复",
            "发布",
            "CI 与合并",
            "归档",
        ]
        .into_iter()
        .enumerate()
        {
            steps = steps.child(chip(
                label,
                if i < index || result == "已完成" {
                    MINT
                } else if i == index {
                    color
                } else {
                    MUTED
                },
            ));
        }
        let mut brief = panel()
            .flex_none()
            .child(steps)
            .child(
                h_flex()
                    .gap_3()
                    .child(chip(result, color))
                    .child(detail_text("https-stage", message)),
            )
            .child(field("源码", &session.source.display().to_string()))
            .when(progress.remote && !progress.task.is_empty(), |brief| {
                brief.child(field("任务", &progress.task))
            })
            .when(!progress.branch.is_empty(), |brief| {
                brief.child(field("目标分支", &progress.branch))
            })
            .when(!progress.worker.is_empty(), |brief| {
                brief.child(field("Worker", &progress.worker))
            })
            .when(!progress.candidate.is_empty(), |brief| {
                brief.child(field("候选 SHA", &progress.candidate))
            });
        if session.can_resume() {
            brief = brief.child(
                button(
                    "https-resume",
                    if session.retry_needed() {
                        "重试当前步骤"
                    } else {
                        "继续任务"
                    },
                    true,
                )
                .on_click(cx.listener(|this, _, _, cx| this.resume_https(cx))),
            );
        }
        page = page.child(brief);
        if let Some(gate) = session.gate() {
            let is_merge = gate.pr.is_some();
            let acknowledged = session.acknowledged.as_ref() == Some(&gate);
            let mut controls = h_flex().gap_2().flex_wrap().child(
                button(
                    "https-ack",
                    if is_merge {
                        "我已核对 CI 和目标"
                    } else {
                        "我已审查 Diff、验收和审查报告"
                    },
                    acknowledged,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(session) = this.https_pilot.as_mut()
                        && let Some(gate) = session.gate()
                    {
                        session.acknowledged =
                            (session.acknowledged.as_ref() != Some(&gate)).then_some(gate);
                        cx.notify();
                    }
                })),
            );
            if acknowledged {
                controls = controls.child(
                    button(
                        "https-confirm",
                        if is_merge {
                            "确认合并 PR"
                        } else {
                            "确认发布并创建 PR"
                        },
                        true,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(session) = this.https_pilot.as_mut() {
                            this.https_pilot_error =
                                session.confirm().err().map(|e| format!("{e:#}"));
                            cx.notify();
                        }
                    })),
                );
            }
            page = page.child(
                panel()
                    .flex_none()
                    .child(heading(if is_merge {
                        "合并确认"
                    } else {
                        "发布确认"
                    }))
                    .child(caption(if let Some(pr) = gate.pr {
                        format!("合并 GitHub PR #{pr} 到 {}", progress.branch)
                    } else {
                        format!("将此候选发布为指向 {} 的 GitHub PR", progress.branch)
                    }))
                    .child(
                        controls.child(button("https-stop", "停止并归档", false).on_click(
                            cx.listener(|this, _, _, cx| {
                                if let Some(session) = this.https_pilot.as_mut() {
                                    session.stop_at_gate();
                                    cx.notify();
                                }
                            }),
                        )),
                    ),
            );
        }
        if let Some(error) = session.failure() {
            page = page.child(
                panel()
                    .flex_none()
                    .child(heading(match progress.exit_code {
                        Some(3) => "停止记录",
                        Some(4) => "暂停记录",
                        _ => "失败原因",
                    }))
                    .child(
                        detail_text("https-failure", &error)
                            .text_sm()
                            .max_h(px(140.))
                            .overflow_hidden(),
                    ),
            );
        }
        if let Some(url) = progress.pr_url() {
            let mut checks = panel().flex_none().child(
                h_flex()
                    .justify_between()
                    .child(heading("GitHub CI"))
                    .child(
                        button(
                            "https-pr",
                            format!("查看 PR #{}", progress.pr.unwrap_or_default()),
                            false,
                        )
                        .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url))),
                    ),
            );
            if session.checks.is_empty() {
                checks = checks.child(caption("等待 GitHub 上报检查"));
            }
            let mut rows = v_flex().gap_2().id("https-checks").test_support();
            for (i, check) in session.checks.iter().enumerate() {
                let name = check["name"]
                    .as_str()
                    .unwrap_or_else(|| string(check, "context"));
                let (state, tone) = check_state(check);
                rows = rows.child(
                    h_flex()
                        .justify_between()
                        .child(detail_text(format!("https-check-{i}"), name))
                        .child(chip(state, tone)),
                );
            }
            page = page.child(checks.child(rows));
        }
        let mut tabs = h_flex().gap_2().flex_wrap();
        for (proof, id, label) in PROOFS {
            if session.has(proof) {
                tabs = tabs.child(
                    button(
                        format!("https-proof-{id}"),
                        label,
                        session.selected == proof,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(session) = this.https_pilot.as_mut() {
                            session.select(proof);
                            cx.notify();
                        }
                    })),
                );
            }
        }
        let directory = session.evidence_dir();
        let mut links = h_flex().gap_2().flex_wrap().child(
            button("https-evidence-dir", "在 Finder 中查看证据", false)
                .on_click(cx.listener(move |_, _, _, cx| cx.reveal_path(&directory))),
        );
        if let Some(archive) = session.archive() {
            links = links.child(
                button("https-archive", "显示 evidence.tar.gz", true)
                    .on_click(cx.listener(move |_, _, _, cx| cx.reveal_path(&archive))),
            );
        }
        h_flex()
            .size_full()
            .bg(rgb(CANVAS))
            .child(self.rail(cx))
            .child(
                page.child(
                    panel()
                        .flex_none()
                        .child(
                            h_flex()
                                .justify_between()
                                .gap_2()
                                .child(heading("日志与证据"))
                                .child(links),
                        )
                        .child(tabs)
                        .child(
                            div()
                                .id("https-preview")
                                .test_support()
                                .min_h(px(100.))
                                .max_h(px(250.))
                                .overflow_y_scroll()
                                .bg(rgb(CANVAS))
                                .rounded_md()
                                .p_3()
                                .child(
                                    detail_text("https-proof-text", &session.text)
                                        .font_family("Menlo")
                                        .text_xs(),
                                ),
                        ),
                ),
            )
    }
}
