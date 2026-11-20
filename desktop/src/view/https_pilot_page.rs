use super::*;
use crate::{
    https_pilot::{Artifact as Proof, Request, Session},
    lifecycle::{self, Choices},
};
use anyhow::{Context as _, ensure};

pub(crate) struct RemoteForm {
    source: Workspace,
    endpoint: Entity<InputState>,
    ca: Entity<InputState>,
    operator_file: Entity<InputState>,
    choices: Option<Choices>,
}

impl RemoteForm {
    fn request(&self, cx: &App) -> Result<Request> {
        let choices = self.choices.as_ref().context("请先加载仓库分支")?;
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
        Ok(Request {
            task: choices.task.clone().context("请选择已派发的任务")?,
            branch: choices.branch.clone().context("请选择目标分支")?,
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
        "remote_dispatch" => (0, "通过 HTTPS 派发真实任务"),
        "remote_checking" => (1, "远程 Worker 正在验收固定候选"),
        "remote_reviewing" => (1, "远程 Claude 正在审查固定候选"),
        "remote_outage" => (2, "HTTPS 连接中断，等待控制面恢复"),
        "remote_retry" => (2, "原 Run 尚无候选，重新派发实现"),
        "outage" => (2, "HTTPS 入口已暂停，等待 Lease 过期"),
        "reconnect" => (2, "HTTPS 已恢复，核对过期发布拒绝和事件回放"),
        "recovery" => (2, "对同一候选重新验收，运行 Claude 审查"),
        "publish_confirmation" => (3, "待人工确认发布"),
        "publication" => (3, "发布固定候选并创建 PR"),
        "ci" => (4, "等待并核对 GitHub CI"),
        "merge_confirmation" => (4, "CI 已通过，待人工确认合并"),
        "merge" => (4, "合并已确认的隔离 PR"),
        "cleanup" => (5, "同步隔离分支并清理候选与工作区"),
        "completed" => (5, "已合并并归档"),
        _ => (0, "检查 Docker 和登录凭证"),
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

    pub(crate) fn load_https_pilot(&mut self) {
        if self.https_pilot.as_ref().is_some_and(Session::active) {
            return;
        }
        let Some(source) = self.https_source() else {
            return;
        };
        if let Some(session) = self.https_pilot.as_mut()
            && session.source == source.root
        {
            session.refresh();
        } else {
            self.https_pilot = Session::latest(&source);
            self.https_pilot_error = None;
        }
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

    fn attach_https_pilot(&mut self, result: Result<Session>, cx: &mut Context<Self>) {
        self.https_pilot_error = None;
        match result {
            Ok(session) => self.https_pilot = Some(session),
            Err(error) => self.https_pilot_error = Some(format!("启动失败：{error:#}")),
        }
        cx.notify();
        if !self.https_pilot.as_ref().is_some_and(Session::active) {
            return;
        }
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            loop {
                executor.timer(Duration::from_millis(750)).await;
                let active = this
                    .update(cx, |this, cx| {
                        let Some(session) = this.https_pilot.as_mut() else {
                            return false;
                        };
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
                        if this.page == Page::HttpsPilot {
                            cx.notify();
                        }
                        session.active()
                    })
                    .unwrap_or(false);
                if !active {
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

    fn start_https_remote(&mut self, cx: &mut Context<Self>) {
        if self.https_pilot.as_ref().is_some_and(Session::active) {
            return;
        }
        let Some(form) = self.https_remote_form.as_ref() else {
            return;
        };
        let result = form
            .request(cx)
            .and_then(|request| Session::launch_remote(&form.source, &self.runner, request));
        if result.is_ok() {
            self.https_remote_form = None;
        }
        self.attach_https_pilot(result, cx);
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
        let mut contents =
            panel()
                .flex_none()
                .child(heading("HTTPS 真实任务"))
                .child(field("仓库", &form.source.root.display().to_string()))
                .child(
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
                .child(input("HTTPS 控制面", &form.endpoint))
                .child(
                    h_flex()
                        .gap_3()
                        .child(input("CA 证书路径", &form.ca))
                        .child(input("操作员令牌文件路径", &form.operator_file)),
                );
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
        } else if self.https_pilot_error.is_none() {
            contents = contents.child(caption("正在读取 GitHub 任务和目标分支"));
        }
        contents
    }

    pub(super) fn https_pilot_page(&self, cx: &Context<Self>) -> Div {
        let session = self.https_pilot.as_ref();
        let running = session.is_some_and(Session::active);
        let mut header = h_flex()
            .justify_between()
            .gap_3()
            .child(heading("HTTPS 试点").text_2xl());
        if !running {
            header = header.child(
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
                    .child(
                        button("https-start", "启动隔离试点", true)
                            .on_click(cx.listener(|this, _, _, cx| this.start_https_pilot(cx))),
                    ),
            );
        }
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
        if !running && let Some(form) = &self.https_remote_form {
            page = page.child(self.https_remote_form(form, cx));
        }
        let Some(session) = session else {
            return h_flex()
                .size_full()
                .bg(rgb(CANVAS))
                .child(self.rail(cx))
                .child(page.child(panel().child(t("从本地 AEP 源码仓库启动 deploy/pilot/live.sh。需要 Docker 和已登录的 GitHub、Codex 与 Claude。", FG))));
        };
        let progress = &session.progress;
        let (index, message) = phase(&progress.stage);
        let (result, color) = match progress.exit_code {
            Some(0) if progress.stage == "completed" && session.archive().is_some() => {
                ("已完成", MINT)
            }
            Some(3) => ("已停止", AMBER),
            Some(_) => ("失败", RED),
            None if session.error.is_some() => ("启动失败", RED),
            None if running => ("运行中", BLUE),
            _ => ("试点未完成", AMBER),
        };
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
        let brief = panel()
            .flex_none()
            .child(steps)
            .child(
                h_flex()
                    .gap_3()
                    .child(chip(result, color))
                    .child(detail_text("https-stage", message)),
            )
            .child(field("源码", &session.source.display().to_string()))
            .when(!progress.branch.is_empty(), |brief| {
                brief.child(field("目标分支", &progress.branch))
            })
            .when(!progress.candidate.is_empty(), |brief| {
                brief.child(field("候选 SHA", &progress.candidate))
            });
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
                    .child(heading(if progress.exit_code == Some(3) {
                        "停止记录"
                    } else {
                        "失败原因"
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
