use super::{
    https_pilot_page::{check_state, phase},
    *,
};
use crate::{
    rolling::{self, History, Session, Site, Status},
    rolling_gate::{Action, Gate},
};
use anyhow::Context as _;
use std::path::Path;

pub(crate) struct Form {
    site: Option<String>,
    sites: Vec<String>,
    access: Entity<InputState>,
    probe: Option<Site>,
    selected: Vec<String>,
    pending: bool,
}

fn rolling_phase(stage: &str) -> &'static str {
    match stage {
        "build-studio" | "prepare-isolated-tasks" => "准备隔离分支和固定 Spec",
        "start-rotation" => "启动轮换并生成待切换密钥",
        "simulate-failed-cutover" => "演练第一台新密钥读取失败",
        "dispatch-during-failure" => "第二台在故障期间执行任务",
        "retry-and-retire-first" => "重试并验证第一台密钥",
        "dispatch-after-first-retirement" => "第一台使用新密钥执行任务",
        "rotate-and-retire-second" => "切换并验证第二台密钥",
        "verify" => "核对归档及 GitHub 密钥状态",
        "completed" => "滚动轮换与归档已完成",
        _ => "检查站点与登录凭证",
    }
}

fn rolling_status(status: &Status, active: bool, archived: bool) -> (&'static str, u32) {
    if active {
        return ("运行中", BLUE);
    }
    match status.exit_code {
        Some(0) if archived => ("已完成", MINT),
        Some(4) => ("已暂停", AMBER),
        Some(_) => ("已停止或失败", RED),
        None => ("已断开", AMBER),
    }
}

fn gate_scope(gate: &Gate) -> (&'static str, String, &'static str) {
    match &gate.action {
        Action::Revoke { old, replacement } => (
            "撤销旧密钥",
            format!("旧密钥 {old}\n已验证的新密钥 {replacement}"),
            "确认撤销旧密钥",
        ),
        Action::Publish { target, candidate } => (
            "发布候选",
            format!("目标 {target}\n候选 {candidate}"),
            "确认发布并创建 PR",
        ),
        Action::Merge {
            target,
            candidate,
            pr,
        } => (
            "合并 PR",
            format!("PR #{pr} → {target}\n候选 {candidate}"),
            "确认合并 PR",
        ),
    }
}

impl Studio {
    pub(super) fn load_rolling_history(&mut self, source: &Workspace) {
        if self
            .https_rolling_history
            .as_ref()
            .is_none_or(|history| history.source != source.root)
        {
            let history = History::load(source);
            let preferred = history.preferred().map(Path::to_path_buf);
            self.https_rolling_history = Some(history);
            if let Some(root) = preferred {
                if let Err(error) = self.show_rolling(source, &root) {
                    self.https_rolling_error = Some(format!("{error:#}"));
                }
            } else {
                self.stash_rolling();
            }
        } else if let Some(history) = &mut self.https_rolling_history {
            history.refresh(true);
        }
    }

    pub(super) fn poll_rolling_history(&mut self, visible: bool) {
        if visible && let Some(history) = &mut self.https_rolling_history {
            history.refresh(false);
        }
        if let Some(session) = &mut self.https_rolling {
            session.refresh();
        }
        for session in &mut self.https_rolling_background {
            session.refresh();
        }
        self.https_rolling_background.retain(Session::active);
    }

    pub(super) fn rolling_busy(&self) -> bool {
        self.https_rolling.as_ref().is_some_and(Session::active)
            || self.https_rolling_background.iter().any(Session::active)
            || self
                .https_rolling_history
                .as_ref()
                .is_some_and(History::any_active)
    }

    fn stash_rolling(&mut self) {
        if let Some(mut previous) = self.https_rolling.take().filter(Session::active) {
            previous.acknowledged = None;
            self.https_rolling_background.push(previous);
        }
    }

    fn show_rolling(&mut self, source: &Workspace, root: &Path) -> Result<()> {
        if self.https_rolling.as_ref().is_some_and(|session| {
            session.root == root && (session.source == source.root || session.active())
        }) {
            return Ok(());
        }
        let session = if let Some(index) = self
            .https_rolling_background
            .iter()
            .position(|s| s.root == root)
        {
            self.https_rolling_background.swap_remove(index)
        } else {
            Session::open(source, root.to_path_buf()).context("无法打开演练记录")?
        };
        self.stash_rolling();
        self.https_rolling = Some(session);
        Ok(())
    }

    fn select_rolling(&mut self, root: &Path, cx: &mut Context<Self>) {
        let result = (|| {
            let source = self.https_source().context("请选择源码仓库")?;
            let history = self
                .https_rolling_history
                .as_mut()
                .context("演练历史尚未加载")?;
            history.lookup(root).context("该演练不在此仓库的历史中")?;
            self.show_rolling(&source, root)?;
            if let Some(history) = &mut self.https_rolling_history {
                history.select(root)?;
            }
            Ok::<_, anyhow::Error>(())
        })();
        self.https_rolling_error = result.err().map(|e| format!("{e:#}"));
        cx.notify();
    }

    pub(super) fn open_rolling(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rolling_busy() || self.https_rolling_form.is_some() {
            return;
        }
        let access = std::env::var_os("AEP_STUDIO_CONTROL_OPERATOR_FILE")
            .map(PathBuf::from)
            .or_else(|| {
                self.https_queue
                    .as_ref()
                    .map(|site| site.connection.operator_file.clone())
            })
            .and_then(|path| path.parent().map(|parent| parent.display().to_string()))
            .or_else(|| {
                self.https_rolling
                    .as_ref()
                    .map(|s| s.site.access.display().to_string())
            })
            .unwrap_or_default();
        let access = cx.new(|cx| {
            let mut input =
                InputState::new(window, cx).placeholder("包含 ca.pem 和 operator 的目录");
            input.set_value(&access, window, cx);
            input
        });
        self.https_rolling_form = Some(Form {
            site: None,
            sites: Vec::new(),
            access,
            probe: None,
            selected: Vec::new(),
            pending: false,
        });
        self.load_rolling_sites(cx);
    }

    fn load_rolling_sites(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self
            .https_rolling_form
            .as_mut()
            .filter(|form| !form.pending)
        else {
            return;
        };
        form.pending = true;
        self.https_rolling_error = None;
        let runner = self.runner.clone();
        let generation = self.workspace_generation;
        let job = cx
            .background_executor()
            .spawn(async move { rolling::sites(&runner) });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |this, cx| {
                if generation == this.workspace_generation
                    && let Some(form) = &mut this.https_rolling_form
                {
                    form.pending = false;
                    match result {
                        Ok(sites) => {
                            if form.site.as_ref().is_some_and(|site| !sites.contains(site)) {
                                form.site = None;
                                form.probe = None;
                                form.selected.clear();
                            }
                            form.sites = sites;
                        }
                        Err(error) => this.https_rolling_error = Some(format!("{error:#}")),
                    }
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn probe_rolling_site(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.https_source() else {
            return;
        };
        let Some(form) = self
            .https_rolling_form
            .as_mut()
            .filter(|form| !form.pending)
        else {
            return;
        };
        let Some(site) = form.site.clone() else {
            return;
        };
        let access = PathBuf::from(form.access.read(cx).value().trim());
        if !access.is_absolute() {
            self.https_rolling_error = Some("请输入访问目录的绝对路径".into());
            cx.notify();
            return;
        }
        self.https_rolling_error = None;
        form.pending = true;
        form.probe = None;
        form.selected.clear();
        let runner = self.runner.clone();
        let generation = self.workspace_generation;
        let job = cx
            .background_executor()
            .spawn(async move { rolling::probe(&source, &runner, site, access) });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |this, cx| {
                if generation == this.workspace_generation
                    && let Some(form) = &mut this.https_rolling_form
                {
                    form.pending = false;
                    match result {
                        Ok(site) => form.probe = Some(site),
                        Err(error) => this.https_rolling_error = Some(format!("{error:#}")),
                    }
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn start_rolling(&mut self, cx: &mut Context<Self>) {
        if self.rolling_busy() {
            return;
        }
        let result = (|| {
            let source = self.https_source().context("请打开 AEP 源码仓库")?;
            let form = self.https_rolling_form.as_ref().context("请选择私有站点")?;
            let site = form.probe.clone().context("请读取站点的 Worker")?;
            let access = PathBuf::from(form.access.read(cx).value().trim()).canonicalize()?;
            anyhow::ensure!(
                access == site.access
                    && !form.pending
                    && form.selected.len() == 2
                    && site.workers.iter().all(|id| form.selected.contains(id)),
                "访问目录或 Worker 选择已变化，请重新核对",
            );
            Session::launch(&source, &self.runner, site)
        })();
        match result {
            Ok(session) => {
                self.https_rolling = Some(session);
                self.https_rolling_form = None;
                self.https_rolling_error = None;
                if let Some(history) = &mut self.https_rolling_history
                    && let Some(session) = &self.https_rolling
                {
                    history.refresh(true);
                    self.https_rolling_error = history
                        .select(&session.root)
                        .err()
                        .map(|e| format!("{e:#}"));
                }
                self.poll_https(cx);
            }
            Err(error) => self.https_rolling_error = Some(format!("{error:#}")),
        }
        cx.notify();
    }

    pub(super) fn rolling_panel(&self, cx: &Context<Self>) -> Option<Div> {
        let mut section = v_flex().gap_3().flex_none();
        if let Some(error) = &self.https_rolling_error {
            section = section.child(
                panel().child(detail_text("rolling-action-error", error).text_color(rgb(RED))),
            );
        }
        let history = self
            .https_rolling_history
            .as_ref()
            .filter(|history| !history.is_empty());
        if let Some(history) = history {
            section = section.child(self.rolling_history_panel(history, cx));
        }
        if let Some(form) = &self.https_rolling_form {
            section = section.child(self.rolling_form(form, cx));
        }
        if let Some(session) = &self.https_rolling {
            section = section.child(self.rolling_run(session, cx));
        }
        (self.https_rolling.is_some()
            || self.https_rolling_form.is_some()
            || self.https_rolling_error.is_some()
            || history.is_some())
        .then_some(section)
    }

    fn rolling_history_panel(&self, history: &History, cx: &Context<Self>) -> Div {
        let mut sites = h_flex().gap_2().flex_wrap().child(
            button(
                "rolling-history-site-all",
                "全部站点",
                history.site().is_none(),
            )
            .on_click(cx.listener(|this, _, _, cx| this.filter_rolling_history(None, true, cx))),
        );
        for site in history.sites() {
            let site = site.to_owned();
            sites = sites.child(
                button(
                    format!("rolling-history-site-{site}"),
                    site.clone(),
                    history.site() == Some(site.as_str()),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.filter_rolling_history(Some(site.clone()), true, cx)
                })),
            );
        }
        let mut workers = h_flex().gap_2().flex_wrap().child(
            button(
                "rolling-history-worker-all",
                "全部 Worker",
                history.worker().is_none(),
            )
            .on_click(cx.listener(|this, _, _, cx| this.filter_rolling_history(None, false, cx))),
        );
        for worker in history.workers() {
            let worker = worker.to_owned();
            workers = workers.child(
                button(
                    format!("rolling-history-worker-{worker}"),
                    worker.clone(),
                    history.worker() == Some(worker.as_str()),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.filter_rolling_history(Some(worker.clone()), false, cx)
                })),
            );
        }
        let mut list = v_flex()
            .id("rolling-history-list")
            .test_support()
            .max_h(px(230.))
            .overflow_y_scroll()
            .gap_2();
        let mut entries = history.visible().peekable();
        if entries.peek().is_none() {
            list = list.child(caption("没有符合筛选条件的演练"));
        }
        for entry in entries {
            let root = entry.root.clone();
            let (label, color) = rolling_status(&entry.status, entry.active, entry.archived);
            let current = self.https_rolling.as_ref().is_some_and(|s| s.root == root);
            list =
                list.child(
                    v_flex()
                        .flex_none()
                        .gap_1()
                        .child(
                            h_flex()
                                .gap_2()
                                .flex_wrap()
                                .child(
                                    button(
                                        format!("rolling-history-entry-{}", entry.id),
                                        entry.id.clone(),
                                        current,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| this.select_rolling(&root, cx),
                                    )),
                                )
                                .child(chip(label, color))
                                .when(entry.archived, |row| row.child(caption("有证据归档"))),
                        )
                        .child(caption(format!(
                            "{} · {}",
                            entry.site.name,
                            rolling_phase(&entry.status.stage)
                        )))
                        .child(caption(entry.site.workers.join(" · "))),
                );
        }
        panel()
            .flex_none()
            .child(heading("滚动轮换演练历史"))
            .child(sites)
            .child(workers)
            .child(list)
    }

    fn filter_rolling_history(
        &mut self,
        value: Option<String>,
        site: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(history) = &mut self.https_rolling_history {
            let result = if site {
                history.filter_site(value)
            } else {
                history.filter_worker(value)
            };
            self.https_rolling_error = result.err().map(|e| format!("{e:#}"));
            cx.notify();
        }
    }

    fn rolling_form(&self, form: &Form, cx: &Context<Self>) -> Div {
        let mut view = panel().flex_none().child(heading("私有站点滚动轮换演练"));
        if form.pending {
            view = view.child(caption("正在读取站点"));
        } else {
            view = view.child(
                button("rolling-refresh-sites", "刷新站点列表", false)
                    .on_click(cx.listener(|this, _, _, cx| this.load_rolling_sites(cx))),
            );
            if form.sites.is_empty() {
                view = view.child(caption("未发现正在运行的 Docker HTTPS 站点"));
            }
        }
        let mut sites = h_flex().gap_2().flex_wrap();
        for site in &form.sites {
            let site = site.clone();
            sites = sites.child(
                button(
                    format!("rolling-site-{site}"),
                    site.clone(),
                    form.site.as_ref() == Some(&site),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(form) = this
                        .https_rolling_form
                        .as_mut()
                        .filter(|form| !form.pending)
                    {
                        form.site = Some(site.clone());
                        form.probe = None;
                        form.selected.clear();
                        this.https_rolling_error = None;
                        cx.notify();
                    }
                })),
            );
        }
        view = view
            .child(sites)
            .child(caption("站点访问目录"))
            .child(Input::new(&form.access).aria_label("站点访问目录"));
        if form.site.is_some() && !form.pending {
            view = view.child(
                button("rolling-probe", "读取站点 Worker", false)
                    .on_click(cx.listener(|this, _, _, cx| this.probe_rolling_site(cx))),
            );
        }
        if let Some(site) = &form.probe {
            view = view
                .child(field("私有 GitHub 仓库", &site.repository))
                .child(field("HTTPS 控制面", &site.endpoint));
            let mut workers = h_flex().gap_2().flex_wrap();
            for worker in &site.workers {
                let worker = worker.clone();
                workers = workers.child(
                    button(
                        format!("rolling-worker-{worker}"),
                        worker.clone(),
                        form.selected.contains(&worker),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(form) = &mut this.https_rolling_form {
                            if form.selected.contains(&worker) {
                                form.selected.retain(|id| id != &worker);
                            } else {
                                form.selected.push(worker.clone());
                            }
                            cx.notify();
                        }
                    })),
                );
            }
            view = view
                .child(caption("选择参与演练的两台 Worker"))
                .child(workers);
            if form.selected.len() == 2 {
                view = view.child(
                    button("rolling-start", "启动滚动轮换演练", true)
                        .on_click(cx.listener(|this, _, _, cx| this.start_rolling(cx))),
                );
            }
        }
        view
    }

    fn rolling_run(&self, session: &Session, cx: &Context<Self>) -> Div {
        let stage = rolling_phase(&session.status.stage);
        let (label, color) = rolling_status(
            &session.status,
            session.active(),
            session.archive().is_some(),
        );
        let mut view = v_flex().gap_3().child(
            panel()
                .flex_none()
                .child(heading("滚动轮换演练"))
                .child(
                    h_flex()
                        .gap_3()
                        .child(chip(label, color))
                        .child(detail_text("rolling-stage", stage)),
                )
                .child(detail_text(
                    "rolling-history-current",
                    &session
                        .root
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy(),
                ))
                .child(field("站点", &session.site.name))
                .child(field("私有仓库", &session.site.repository))
                .child(field("Worker", &session.site.workers.join(" · "))),
        );
        if let Some(reason) = &session.cutover_error {
            view = view.child(
                panel()
                    .child(heading("已演练切换失败 · 原密钥保留"))
                    .child(detail_text("rolling-cutover-error", reason)),
            );
        }
        if !session.status.reason.is_empty() {
            view = view.child(panel().child(
                detail_text("rolling-failure", &session.status.reason).text_color(rgb(RED)),
            ));
        }
        if session.can_resume() {
            view = view.child(
                panel().child(
                    button(
                        "rolling-resume",
                        if session.status.exit_code.is_some_and(|code| code != 4) {
                            "安全重试"
                        } else {
                            "继续演练"
                        },
                        true,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(session) = &mut this.https_rolling {
                            this.https_rolling_error =
                                session.resume(&this.runner).err().map(|e| format!("{e:#}"));
                            this.poll_https(cx);
                            cx.notify();
                        }
                    })),
                ),
            );
        }
        if let Some(gate) = session.pending_gate() {
            let (title, details, action) = gate_scope(gate);
            let acknowledged = session.acknowledged.as_ref() == Some(gate);
            let mut actions = h_flex().gap_2().flex_wrap().child(
                button("rolling-ack", "我已核对", acknowledged).on_click(cx.listener(
                    |this, _, _, cx| {
                        if let Some(session) = &mut this.https_rolling
                            && let Some(gate) = session.gate().cloned()
                        {
                            session.acknowledged =
                                (session.acknowledged.as_ref() != Some(&gate)).then_some(gate);
                            cx.notify();
                        }
                    },
                )),
            );
            if acknowledged {
                actions = actions.child(button("rolling-confirm", action, true).on_click(
                    cx.listener(|this, _, _, cx| {
                        if let Some(session) = &mut this.https_rolling {
                            this.https_rolling_error =
                                session.confirm().err().map(|e| format!("{e:#}"));
                            cx.notify();
                        }
                    }),
                ));
            }
            actions = actions.child(button("rolling-stop", "暂停并保留证据", false).on_click(
                cx.listener(|this, _, _, cx| {
                    if let Some(session) = &mut this.https_rolling {
                        this.https_rolling_error =
                            session.stop_at_gate().err().map(|e| format!("{e:#}"));
                        cx.notify();
                    }
                }),
            ));
            view = view.child(
                panel()
                    .flex_none()
                    .child(heading(title))
                    .child(detail_text("rolling-gate-worker", &gate.worker))
                    .child(detail_text("rolling-gate-scope", &details))
                    .when(session.gate().is_some(), |panel| panel.child(actions)),
            );
        }
        if !session.attempts.is_empty() {
            let mut attempts = panel().flex_none().child(heading("运行尝试"));
            for (index, attempt) in session.attempts.iter().enumerate() {
                let (status, color) =
                    rolling_status(&attempt.status, false, attempt.status.exit_code == Some(0));
                attempts = attempts.child(
                    v_flex()
                        .gap_1()
                        .child(
                            h_flex()
                                .gap_2()
                                .flex_wrap()
                                .child(self.rolling_proof(
                                    format!("rolling-attempt-{index}"),
                                    format!(
                                        "第 {} 次 · {}",
                                        index + 1,
                                        rolling_phase(&attempt.status.stage)
                                    ),
                                    attempt.receipt.clone(),
                                    session,
                                    cx,
                                ))
                                .child(chip(status, color)),
                        )
                        .when(!attempt.status.reason.is_empty(), |attempts| {
                            attempts.child(detail_text(
                                format!("rolling-attempt-reason-{index}"),
                                &attempt.status.reason,
                            ))
                        }),
                );
            }
            view = view.child(attempts);
        }
        if !session.approvals.is_empty() {
            let mut receipts = panel().flex_none().child(heading("确认回执"));
            for (index, approval) in session.approvals.iter().enumerate() {
                let (action, scope, _) = gate_scope(&approval.gate);
                receipts = receipts.child(
                    v_flex()
                        .gap_1()
                        .child(self.rolling_proof(
                            format!("rolling-receipt-{index}"),
                            format!("{action} · {}", approval.gate.worker),
                            approval.receipt.clone(),
                            session,
                            cx,
                        ))
                        .child(caption(scope)),
                );
            }
            view = view.child(receipts);
        }
        let mut tabs = h_flex().gap_2().flex_wrap();
        for run in &session.deliveries {
            let mut card = panel()
                .flex_none()
                .child(heading(if run.role == "peer" {
                    "故障期间的任务"
                } else {
                    "切换后的任务"
                }))
                .child(field("Worker", &run.progress.worker))
                .child(detail_text(
                    format!("rolling-delivery-{}", run.role),
                    phase(&run.progress.stage).1,
                ));
            if !run.progress.reason.is_empty() {
                card = card.child(t(run.progress.reason.clone(), RED));
            }
            if let Some(url) = run.progress.pr_url() {
                card = card.child(
                    button(
                        format!("rolling-pr-{}", run.role),
                        "在 GitHub 查看 PR",
                        false,
                    )
                    .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url))),
                );
                if run.progress.ci_provider == "https_worker" {
                    card = card.child(caption("Worker CI"));
                }
                for (index, check) in run.checks.iter().enumerate() {
                    let (state, tone) = check_state(check);
                    card = card.child(
                        h_flex()
                            .gap_3()
                            .child(detail_text(
                                format!("rolling-ci-{}-{index}", run.role),
                                check["name"]
                                    .as_str()
                                    .or_else(|| check["context"].as_str())
                                    .unwrap_or("验收检查"),
                            ))
                            .child(chip(state, tone)),
                    );
                }
            }
            for (index, (label, path)) in run.proofs().into_iter().enumerate() {
                tabs = tabs.child(self.rolling_proof(
                    format!("rolling-proof-{}-{index}", run.role),
                    format!(
                        "{} · {label}",
                        if run.role == "peer" {
                            "故障期间"
                        } else {
                            "切换后"
                        }
                    ),
                    path,
                    session,
                    cx,
                ));
            }
            view = view.child(card);
        }

        for (index, (label, path)) in session.shots.iter().enumerate() {
            tabs = tabs.child(self.rolling_proof(
                format!("rolling-shot-{index}"),
                label.clone(),
                path.clone(),
                session,
                cx,
            ));
        }
        let mut evidence = panel()
            .flex_none()
            .child(heading("演练日志与证据"))
            .child(tabs);
        if let Some(archive) = session.archive() {
            evidence = evidence.child(
                button("rolling-archive", "显示演练归档", true)
                    .on_click(cx.listener(move |_, _, _, cx| cx.reveal_path(&archive))),
            );
        }
        let mut preview = div()
            .id("rolling-preview")
            .test_support()
            .max_h(px(420.))
            .overflow_y_scroll()
            .bg(rgb(CANVAS))
            .rounded_md()
            .p_3();
        if let Some(path) = session
            .selected
            .as_ref()
            .filter(|path| path.extension().is_some_and(|ext| ext == "png"))
        {
            preview = preview.child(
                img(path.clone())
                    .w_full()
                    .h(px(390.))
                    .object_fit(ObjectFit::Contain),
            );
        } else {
            let content = session
                .selected
                .as_ref()
                .and_then(|path| crate::https_pilot::preview(path, false))
                .unwrap_or_else(|| session.log.clone());
            preview = preview.child(t(content, FG).font_family("Menlo"));
        }
        view.child(
            evidence
                .child(
                    button("rolling-log", "查看演练日志", session.selected.is_none()).on_click(
                        cx.listener(|this, _, _, cx| {
                            if let Some(session) = &mut this.https_rolling {
                                session.selected = None;
                                cx.notify();
                            }
                        }),
                    ),
                )
                .child(preview),
        )
    }

    fn rolling_proof(
        &self,
        id: String,
        label: String,
        path: PathBuf,
        session: &Session,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        button(id, label, session.selected.as_ref() == Some(&path)).on_click(cx.listener(
            move |this, _, _, cx| {
                if let Some(session) = &mut this.https_rolling {
                    session.selected = Some(path.clone());
                    cx.notify();
                }
            },
        ))
    }
}
