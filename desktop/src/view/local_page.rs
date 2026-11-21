use super::task_authoring::DraftFields;
use super::*;
use crate::lifecycle::{Branch, Draft};
use anyhow::{Context as _, ensure};
use std::{
    io::{Seek, Write},
    process::{Command, Stdio},
};

const FIELDS: [(&str, &str, &str); 5] = [
    ("ticket", "任务票据", "如 volume-fix"),
    ("task", "Spec 任务目录", "如 market-volume-dedup-rust"),
    ("spec", "Spec 提交或本地引用", "完整 SHA 或本地分支"),
    ("target", "本地目标分支", "main"),
    ("model", "Codex 模型", "可选，默认使用本机配置"),
];
const TABS: [(&str, &str); 5] = [
    ("spec", "Spec"),
    ("diff", "Diff"),
    ("implementation", "实现日志"),
    ("checks", "验收"),
    ("review", "审查"),
];

pub(crate) struct State {
    fields: [Entity<InputState>; FIELDS.len()],
    rows: Vec<Value>,
    selected: Option<String>,
    inspection: Inspection,
    targets: Vec<Branch>,
    authoring: Option<Authoring>,
    detail: Value,
    tab: &'static str,
    loading: bool,
    operation: Option<&'static str>,
    cancelling: bool,
    pub(super) show_catalog: bool,
    error: Option<String>,
}

#[derive(Clone, Default, Eq, PartialEq)]
struct Inspection {
    version: Option<u64>,
    run: Option<String>,
}

struct Authoring {
    ticket: String,
    target: Branch,
    version: Option<u64>,
    model: Entity<InputState>,
    fields: DraftFields,
}

pub(super) fn call(
    ws: &Workspace,
    runner: &Runner,
    args: &[String],
    payload: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let stdin = if let Some(bytes) = payload {
        let mut draft = tempfile::tempfile()?;
        draft.write_all(bytes)?;
        draft.rewind()?;
        Stdio::from(draft)
    } else {
        Stdio::null()
    };
    let output = Command::new(runner.binary()?)
        .current_dir(&ws.root)
        .arg("local")
        .args(args)
        .env("PATH", runner.path()?)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove(aep::CONTROL_OPERATOR_TOKEN)
        .env_remove(aep::CONTROL_DEVICE_TOKEN)
        .stdin(stdin)
        .output()
        .context("运行 AEP 本地任务命令")?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}

pub(super) fn label(phase: &str) -> (&str, u32) {
    match phase {
        "plan_pending" => ("待批准计划", AMBER),
        "ready" => ("待实现", BLUE),
        "running" => ("运行中", BLUE),
        "retry" => ("可重试", AMBER),
        "blocked" => ("已阻塞", RED),
        "review_pending" => ("待审查交付", AMBER),
        "approved" => ("待合并", MINT),
        "merge_pending" => ("待核对合并", AMBER),
        "merged" => ("已合并", MINT),
        "history" => ("历史证据", MUTED),
        _ => ("载入中", MUTED),
    }
}

impl Studio {
    pub(super) fn focus_project_ticket(
        &mut self,
        ticket: &str,
        tab: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = &mut self.local_tasks {
            state.show_catalog = false;
            state.fields[0].update(cx, |input, cx| input.set_value(ticket, window, cx));
            state.selected = Some(ticket.into());
            state.inspection = Inspection::default();
            state.authoring = None;
            state.detail = Value::Null;
            state.tab = tab;
            state.error = None;
        }
        self.refresh_local(cx);
        cx.notify();
    }

    pub(crate) fn open_local(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.local_tasks.is_none() {
            self.local_tasks = Some(State {
                fields: FIELDS
                    .map(|(_, _, hint)| cx.new(|cx| InputState::new(window, cx).placeholder(hint))),
                rows: Vec::new(),
                selected: None,
                inspection: Inspection::default(),
                targets: Vec::new(),
                authoring: None,
                detail: Value::Null,
                tab: "spec",
                loading: false,
                operation: None,
                cancelling: false,
                show_catalog: false,
                error: None,
            });
        }
        self.refresh_local(cx);
        if self.local_polling {
            return;
        }
        self.local_polling = true;
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            loop {
                executor.timer(Duration::from_millis(850)).await;
                let active = this.update(cx, |studio, cx| {
                    if studio.page != Page::Local {
                        studio.local_polling = false;
                        return false;
                    }
                    studio.refresh_local(cx);
                    true
                });
                if !active.unwrap_or(false) {
                    break;
                }
            }
        })
        .detach();
    }

    fn refresh_local(&mut self, cx: &mut Context<Self>) {
        let (Some(state), Some(ws)) = (&mut self.local_tasks, self.workspace.clone()) else {
            return;
        };
        if state.loading {
            return;
        }
        state.loading = true;
        let runner = self.runner.clone();
        let selected = state.selected.clone();
        let requested = selected.clone();
        let inspection = state.inspection.clone();
        let requested_scope = inspection.clone();
        let generation = self.workspace_generation;
        let job = cx.background_executor().spawn(async move {
            let rows =
                serde_json::from_slice::<Vec<Value>>(&call(&ws, &runner, &["list".into()], None)?)?;
            let targets = serde_json::from_slice::<Vec<Branch>>(&call(
                &ws,
                &runner,
                &["targets".into()],
                None,
            )?)?;
            let detail = if let Some(ticket) =
                selected.filter(|ticket| rows.iter().any(|row| row["ticket"] == *ticket))
            {
                let mut args = vec!["inspect".into(), "--ticket".into(), ticket];
                if let Some(version) = inspection.version {
                    args.extend(["--version".into(), version.to_string()]);
                }
                if let Some(run) = inspection.run {
                    args.extend(["--run".into(), run]);
                }
                call(&ws, &runner, &args, None)
                    .and_then(|bytes| Ok(serde_json::from_slice(&bytes)?))
            } else {
                Ok(Value::Null)
            };
            Ok::<_, anyhow::Error>((rows, targets, detail))
        });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |studio, cx| {
                if studio.workspace_generation != generation {
                    return;
                }
                let Some(state) = &mut studio.local_tasks else {
                    return;
                };
                state.loading = false;
                match result {
                    Ok((rows, targets, detail)) => {
                        state.rows = rows;
                        state.targets = targets;
                        if state.selected == requested && state.inspection == requested_scope {
                            match detail {
                                Ok(detail) => state.detail = detail,
                                Err(error) if state.error.is_none() => {
                                    state.error = Some(format!("{error:#}"));
                                }
                                Err(_) => {}
                            }
                        }
                    }
                    Err(error) => state.error = Some(format!("{error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn local_command(
        &mut self,
        operation: &'static str,
        ticket: String,
        args: Vec<String>,
        payload: Option<Vec<u8>>,
        cx: &mut Context<Self>,
    ) {
        let (Some(state), Some(ws)) = (&mut self.local_tasks, self.workspace.clone()) else {
            return;
        };
        if operation == "cancel" {
            if state.cancelling {
                return;
            }
            state.cancelling = true;
        } else {
            if state.operation.is_some() {
                return;
            }
            state.operation = Some(operation);
        }
        if state.selected.as_deref() != Some(&ticket) {
            state.selected = Some(ticket);
            state.inspection = Inspection::default();
            state.detail = Value::Null;
            state.tab = "spec";
        }
        state.error = None;
        let runner = self.runner.clone();
        let generation = self.workspace_generation;
        let job = cx.background_executor().spawn(async move {
            let mut command = vec![operation.to_owned()];
            command.extend(args);
            call(&ws, &runner, &command, payload.as_deref()).map(|_| ())
        });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |studio, cx| {
                if studio.workspace_generation != generation {
                    return;
                }
                let Some(state) = &mut studio.local_tasks else {
                    return;
                };
                if operation == "cancel" {
                    state.cancelling = false;
                } else {
                    state.operation = None;
                }
                state.error = result.as_ref().err().map(|error| format!("{error:#}"));
                if result.is_ok() && matches!(operation, "author" | "plan") {
                    state.authoring = None;
                    state.inspection = Inspection::default();
                    state.detail = Value::Null;
                }
                studio.refresh_local(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn propose_local(&mut self, revise: bool, cx: &mut Context<Self>) {
        let Some(state) = &mut self.local_tasks else {
            return;
        };
        let mut inputs = state
            .fields
            .iter()
            .map(|f| f.read(cx).value().trim().to_owned());
        let ticket = inputs.next().unwrap_or_default();
        let task = inputs.next().unwrap_or_default();
        let spec = inputs.next().unwrap_or_default();
        let target = inputs
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "main".into());
        let model = inputs.next().filter(|value| !value.is_empty());
        if ticket.is_empty() || task.is_empty() || spec.is_empty() {
            state.error = Some("请填写任务票据、Spec 任务目录和 Spec 提交。".into());
            cx.notify();
            return;
        }
        let mut args = vec![
            "--ticket".into(),
            ticket.clone(),
            "--task".into(),
            task,
            "--spec".into(),
            spec,
            "--target".into(),
            target,
        ];
        if revise {
            args.push("--revise".into());
        }
        if let Some(model) = model {
            args.extend(["--codex-model".into(), model]);
        }
        self.local_command("plan", ticket, args, None, cx);
    }

    fn edit_local_spec(&mut self, revise: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = &mut self.local_tasks else {
            return;
        };
        let setup = (|| -> Result<_> {
            if revise {
                let detail = &state.detail;
                ensure!(detail["historical"] != true, "请先打开当前计划");
                let plan = &detail["current"];
                let proposal = &detail["proposal"];
                let draft = Draft {
                    name: string(plan, "task").into(),
                    spec: string(proposal, "spec").into(),
                    manifest: serde_json::from_value(proposal["manifest"].clone())?,
                };
                let version = plan["version"].as_u64().context("缺少当前计划版本")?;
                return Ok((
                    string(detail, "ticket").to_owned(),
                    string(plan, "target").to_owned(),
                    Some(version),
                    plan["codex_model"].as_str().map(str::to_owned),
                    Some(draft),
                ));
            }
            let ticket = state.fields[0].read(cx).value().trim().to_owned();
            ensure!(!ticket.is_empty(), "请填写任务票据");
            let target = state.fields[3].read(cx).value().trim().to_owned();
            let target = if target.is_empty() {
                "main".into()
            } else {
                target
            };
            let model = state.fields[4].read(cx).value().trim().to_owned();
            Ok((
                ticket,
                target,
                None,
                (!model.is_empty()).then_some(model),
                None,
            ))
        })();
        let result = setup.and_then(|(ticket, target, version, model, draft)| {
            let target = state
                .targets
                .iter()
                .find(|branch| branch.name == target)
                .cloned()
                .context("请选择已存在的本地目标分支")?;
            let fields = DraftFields::new(draft.as_ref(), window, cx);
            let model = cx.new(|cx| {
                let mut field = InputState::new(window, cx).placeholder("默认使用本机配置");
                if let Some(model) = &model {
                    field.set_value(model, window, cx);
                }
                field
            });
            Ok(Authoring {
                ticket,
                target,
                version,
                model,
                fields,
            })
        });
        match result {
            Ok(editor) => {
                state.authoring = Some(editor);
                state.error = None;
            }
            Err(error) => state.error = Some(format!("{error:#}")),
        }
        cx.notify();
    }

    fn save_local_spec(&mut self, cx: &mut Context<Self>) {
        let Some(state) = &mut self.local_tasks else {
            return;
        };
        let Some(editor) = &state.authoring else {
            return;
        };
        let draft = editor.fields.draft(cx).and_then(|draft| {
            ensure!(draft.manifest.title.is_some(), "请填写任务标题");
            Ok(serde_json::to_vec(&draft)?)
        });
        let payload = match draft {
            Ok(payload) => payload,
            Err(error) => {
                state.error = Some(format!("{error:#}"));
                cx.notify();
                return;
            }
        };
        let ticket = editor.ticket.clone();
        let mut args = vec![
            "--ticket".into(),
            ticket.clone(),
            "--target".into(),
            editor.target.name.clone(),
            "--baseline".into(),
            editor.target.sha.clone(),
        ];
        if let Some(version) = editor.version {
            args.extend(["--version".into(), version.to_string()]);
        }
        let model = editor.model.read(cx).value().trim().to_owned();
        if model.is_empty() {
            args.push("--default-codex-model".into());
        } else {
            args.extend(["--codex-model".into(), model]);
        }
        self.local_command("author", ticket, args, Some(payload), cx);
    }

    fn select_local_evidence(&mut self, inspection: Inspection, cx: &mut Context<Self>) {
        if let Some(state) = &mut self.local_tasks {
            state.tab = if inspection.run.is_some() {
                "checks"
            } else {
                "spec"
            };
            state.inspection = inspection;
            state.detail = Value::Null;
            state.error = None;
        }
        self.refresh_local(cx);
        cx.notify();
    }

    fn local_page_action(&mut self, operation: &'static str, cx: &mut Context<Self>) {
        let Some(state) = &self.local_tasks else {
            return;
        };
        let detail = &state.detail;
        if !self.ticket_in_context(string(detail, "ticket")) {
            return;
        }
        if detail["historical"] == true {
            return;
        }
        let ticket = string(detail, "ticket").to_owned();
        let mut args = vec!["--ticket".into(), ticket.clone()];
        if matches!(operation, "approve-plan" | "approve-delivery" | "merge") {
            let pin = &detail["current"];
            args.extend([
                "--version".into(),
                pin["version"].to_string(),
                "--spec".into(),
                string(pin, "spec").into(),
                "--baseline".into(),
                string(pin, "baseline").into(),
            ]);
        }
        if matches!(operation, "approve-delivery" | "merge" | "cancel") {
            args.extend(["--run".into(), string(&detail["run"], "run_id").into()]);
        }
        if matches!(operation, "approve-delivery" | "merge") {
            args.extend([
                "--candidate".into(),
                string(&detail["run"], "candidate_sha").into(),
            ]);
        }
        if operation == "approve-delivery" {
            args.extend([
                "--report-sha256".into(),
                string(detail, "report_sha256").into(),
            ]);
        }
        self.local_command(operation, ticket, args, None, cx);
    }

    pub(super) fn local_page(&self, cx: &mut Context<Self>) -> Div {
        let Some(state) = &self.local_tasks else {
            return div().child(heading("本地任务"));
        };
        let mut page = v_flex()
            .id("local-page")
            .test_support()
            .flex_1()
            .min_w_0()
            .h_full()
            .min_h_0()
            .overflow_x_hidden()
            .overflow_y_scroll()
            .gap_4()
            .p_5()
            .child(heading(
                self.context_work()
                    .map(|work| work.name.as_str())
                    .unwrap_or("本地任务")
                    .to_owned(),
            ));
        let scoped = self.context_work().is_some();
        if scoped {
            let repo = self
                .workspace
                .as_ref()
                .map(|ws| ws.root.display().to_string())
                .unwrap_or_default();
            page = page.child(
                h_flex().justify_between().child(caption(repo)).child(
                    button(
                        "project-toggle-catalog",
                        if state.show_catalog {
                            "返回事项任务"
                        } else {
                            "关联已有任务"
                        },
                        false,
                    )
                    .on_click(cx.listener(|this, _, w, cx| {
                        let current = this
                            .context_work()
                            .zip(this.workspace.as_ref())
                            .map(|(work, ws)| work.ticket(&ws.root));
                        if let Some(state) = &mut this.local_tasks {
                            state.show_catalog = !state.show_catalog;
                            if !state.show_catalog
                                && let Some(ticket) = current
                            {
                                this.focus_project_ticket(&ticket, "spec", w, cx);
                            }
                        }
                        cx.notify();
                    })),
                ),
            );
        }
        let mut form = panel().flex_none().child(heading("拟定任务计划"));
        for ((id, name, _), input) in FIELDS.iter().zip(&state.fields) {
            if scoped && matches!(*id, "ticket" | "task" | "spec") {
                continue;
            }
            form = form.child(
                v_flex().gap_1().child(caption(*name)).child(
                    div()
                        .id(format!("local-{id}"))
                        .test_support()
                        .child(Input::new(input)),
                ),
            );
        }
        if !state.targets.is_empty() {
            let mut choices = h_flex().gap_2().flex_wrap();
            for branch in &state.targets {
                let name = branch.name.clone();
                choices = choices.child(
                    button(
                        format!("local-target-{name}"),
                        name.clone(),
                        name == state.fields[3].read(cx).value().trim(),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(state) = &this.local_tasks {
                            state.fields[3]
                                .update(cx, |input, cx| input.set_value(&name, window, cx));
                        }
                        cx.notify();
                    })),
                );
            }
            form = form.child(caption("本地分支")).child(
                div()
                    .id("local-target-options")
                    .test_support()
                    .max_h(px(96.))
                    .overflow_y_scroll()
                    .child(choices),
            );
        }
        let mut controls = h_flex().gap_2();
        if state.operation.is_none() {
            controls =
                controls
                    .child(button("local-new-spec", "编写 Spec", true).on_click(
                        cx.listener(|this, _, window, cx| this.edit_local_spec(false, window, cx)),
                    ))
                    .when(!scoped, |controls| {
                        controls
                            .child(button("local-plan", "从已有提交生成计划", false).on_click(
                                cx.listener(|this, _, _, cx| this.propose_local(false, cx)),
                            ))
                            .child(button("local-revise", "用已有提交修订", false).on_click(
                                cx.listener(|this, _, _, cx| this.propose_local(true, cx)),
                            ))
                    });
        }
        if let Some(editor) = &state.authoring {
            let saving = state.operation.is_some();
            let mut editor_view = panel()
                .flex_none()
                .child(heading(if editor.version.is_some() {
                    "修订本地 Spec"
                } else {
                    "编写本地 Spec"
                }))
                .child(field("票据", &editor.ticket))
                .child(field("目标分支", &editor.target.name))
                .child(field("目标提交", &editor.target.sha));
            if let Some(target) = state
                .targets
                .iter()
                .find(|branch| branch.name == editor.target.name && branch.sha != editor.target.sha)
                && !saving
            {
                let target = target.clone();
                editor_view = editor_view.child(
                    h_flex()
                        .gap_2()
                        .child(caption("目标已更新，核对后可采用新基线。"))
                        .child(
                            button("local-update-baseline", "采用当前目标", false).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    if let Some(editor) = this
                                        .local_tasks
                                        .as_mut()
                                        .and_then(|state| state.authoring.as_mut())
                                    {
                                        editor.target = target.clone();
                                        cx.notify();
                                    }
                                }),
                            ),
                        ),
                );
            }
            editor_view = editor_view.child(editor.fields.render(saving, true)).child(
                v_flex().gap_1().child(caption("Codex 模型（可选）")).child(
                    div().id("local-author-model").test_support().child(
                        Input::new(&editor.model)
                            .aria_label("Codex 模型")
                            .disabled(saving),
                    ),
                ),
            );
            if !saving {
                editor_view = editor_view.child(
                    h_flex()
                        .gap_2()
                        .child(
                            button("local-save-spec", "保存 Spec 与计划", true)
                                .on_click(cx.listener(|this, _, _, cx| this.save_local_spec(cx))),
                        )
                        .child(button("local-cancel-spec", "返回任务", false).on_click(
                            cx.listener(|this, _, _, cx| {
                                if let Some(state) = &mut this.local_tasks {
                                    state.authoring = None;
                                    state.error = None;
                                }
                                cx.notify();
                            }),
                        )),
                );
            }
            page = page.child(editor_view);
        } else if !scoped || (state.detail.is_null() && !state.show_catalog) {
            page = page.child(form.child(controls));
        }
        if let Some(error) = &state.error {
            page = page.child(detail_text("local-command-error", error));
        }
        if (!scoped || state.show_catalog) && !state.rows.is_empty() {
            let mut rows = panel().flex_none().child(heading("任务记录"));
            for row in &state.rows {
                let ticket = string(row, "ticket").to_owned();
                let (phase, tone) = label(string(row, "phase"));
                rows = rows.child(
                    h_flex()
                        .gap_3()
                        .child(
                            button(
                                format!("local-ticket-{ticket}"),
                                ticket.clone(),
                                state.selected.as_ref() == Some(&ticket),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if let Some(state) = &mut this.local_tasks {
                                        state.selected = Some(ticket.clone());
                                        state.inspection = Inspection::default();
                                        state.detail = Value::Null;
                                        state.tab = "spec";
                                        state.error = None;
                                    }
                                    this.refresh_local(cx);
                                    cx.notify();
                                },
                            )),
                        )
                        .child(chip(phase, tone)),
                );
            }
            page = page.child(rows);
        }
        let detail = &state.detail;
        if detail.is_null() {
            return h_flex()
                .size_full()
                .bg(rgb(CANVAS))
                .child(self.rail(cx))
                .child(page);
        }
        let phase = string(detail, "phase");
        let (title, tone) = label(phase);
        let pin = &detail["current"];
        let mut review = panel()
            .flex_none()
            .child(heading(string(detail, "ticket")))
            .child(
                h_flex()
                    .gap_4()
                    .child(chip(title, tone))
                    .child(caption(format!("计划版本 {}", pin["version"]))),
            )
            .child(field("目标", string(pin, "target")))
            .child(field("基线", string(pin, "baseline")))
            .child(field("Spec", string(pin, "spec")));
        if let (Some(work), Some(ws)) = (self.context_work(), self.workspace.as_ref()) {
            let ticket = string(detail, "ticket").to_owned();
            if work.ticket(&ws.root) == ticket {
                review = review.child(field("开发事项", &work.name));
            } else {
                review = review.child(
                    button(
                        "project-bind-ticket",
                        format!("将此任务关联到「{}」", work.name),
                        false,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.link_project_ticket(ticket.clone(), cx)
                    })),
                );
            }
            if let Some(error) = &self.board.error {
                review = review.child(t(error.clone(), AMBER));
            }
        }
        if let Some(model) = pin["codex_model"].as_str() {
            review = review.child(field("Codex 模型", model));
        }
        if let Some(actor) = pin["plan_approval"]["actor"].as_str() {
            review = review.child(field("计划批准人", actor));
        }
        if !pin["delivery"]["approval"].is_null()
            && detail["run"]["run_id"] == pin["delivery"]["run"]
        {
            review = review.child(field(
                "交付批准人",
                string(&pin["delivery"]["approval"], "actor"),
            ));
        }
        if phase == "merged" {
            review = review.child(
                div()
                    .id("local-merge-receipt")
                    .test_support()
                    .child(field("已合并提交", string(&pin["delivery"], "candidate"))),
            );
        }
        if let Some(versions) = detail["history"]
            .as_array()
            .filter(|versions| !versions.is_empty())
        {
            let mut choices = h_flex().gap_2().flex_wrap();
            choices = choices.child(
                button(
                    "local-current-version",
                    "当前计划",
                    detail["historical"] != true,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.select_local_evidence(Inspection::default(), cx)
                })),
            );
            for version in versions
                .iter()
                .filter_map(|version| version["version"].as_u64())
            {
                choices = choices.child(
                    button(
                        format!("local-version-{version}"),
                        format!("版本 {version}"),
                        pin["version"] == version,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_local_evidence(
                            Inspection {
                                version: Some(version),
                                run: None,
                            },
                            cx,
                        )
                    })),
                );
            }
            review = review.child(choices);
        } else if detail["historical"] == true {
            review = review.child(h_flex().child(
                button("local-current-version", "返回当前 Run", true).on_click(cx.listener(
                    |this, _, _, cx| this.select_local_evidence(Inspection::default(), cx),
                )),
            ));
        }
        if phase != "merged"
            && let Some(sha) = detail["run"]["candidate_sha"].as_str()
        {
            review = review.child(field("变更提交", sha));
        }
        if let Some(tokens) = detail["usage"]["tokens"]
            .as_u64()
            .filter(|total| *total > 0)
        {
            let usage = &detail["usage"];
            let cost = usage["reported_cost_micros"].as_u64().unwrap_or(0) as f64 / 1_000_000.0;
            let unknown = usage["unpriced_calls"].as_u64().unwrap_or(0);
            review = review.child(caption(format!(
                "{tokens} tokens · 已报告费用 ${cost:.4} · {unknown} 次调用费用未知"
            )));
        }
        if let Some(reason) = detail["reason"]
            .as_str()
            .or_else(|| detail["run"]["error"].as_str())
        {
            review = review.child(detail_text("local-run-reason", reason));
        }
        if let Some(runs) = detail["runs"].as_array() {
            for run in runs {
                let id = string(run, "id").to_owned();
                let (description, tone) = if pin["delivery"]["run"] == id {
                    if !pin["merged_at"].is_null() {
                        label("merged")
                    } else if !pin["merging"].is_null() {
                        label("merge_pending")
                    } else {
                        label("approved")
                    }
                } else {
                    status(string(run, "status"))
                };
                let version = pin["version"].as_u64();
                review = review.child(
                    h_flex()
                        .gap_2()
                        .child(
                            button(
                                format!("local-run-evidence-{id}"),
                                id.clone(),
                                id == string(&detail["run"], "run_id"),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.select_local_evidence(
                                        Inspection {
                                            version,
                                            run: Some(id.clone()),
                                        },
                                        cx,
                                    )
                                },
                            )),
                        )
                        .child(chip(description, tone)),
                );
            }
        }
        let action = match phase {
            "plan_pending" => Some(("approve-plan", "批准计划")),
            "ready" => Some(("run", "开始实现与审查")),
            "retry" => Some(("run", "重试当前任务")),
            "review_pending" => Some(("approve-delivery", "确认验收与审查")),
            "approved" | "merge_pending" => Some(("merge", "确认合入本地目标")),
            "running" if !state.cancelling && detail["run"]["status"] == "running" => {
                Some(("cancel", "取消当前执行"))
            }
            _ => None,
        };
        if let Some((op, title)) = action
            && self.ticket_in_context(string(detail, "ticket"))
            && ((state.operation.is_none() && state.authoring.is_none()) || op == "cancel")
        {
            review =
                review.child(h_flex().child(
                    button(format!("local-{op}"), title, true).on_click(
                        cx.listener(move |this, _, _, cx| this.local_page_action(op, cx)),
                    ),
                ));
        }
        if self.ticket_in_context(string(detail, "ticket"))
            && !matches!(phase, "running" | "history" | "merge_pending")
            && state.operation.is_none()
            && state.authoring.is_none()
        {
            review = review.child(h_flex().child(
                button("local-edit-spec", "修订 Spec", false).on_click(
                    cx.listener(|this, _, window, cx| this.edit_local_spec(true, window, cx)),
                ),
            ));
        }
        page = page.child(review);
        let mut tabs = h_flex().gap_2().flex_wrap();
        for (id, title) in TABS {
            tabs = tabs.child(
                button(format!("local-tab-{id}"), title, state.tab == id).on_click(cx.listener(
                    move |this, _, _, cx| {
                        if let Some(state) = &mut this.local_tasks {
                            state.tab = id;
                        }
                        cx.notify();
                    },
                )),
            );
        }
        if let (Some(ws), Some(run)) = (&self.workspace, detail["run"]["run_id"].as_str())
            && aep::workspace::ident(run, 64).is_ok()
        {
            let folder = ws.data.join("runs").join(run);
            tabs = tabs.child(
                button("local-open-evidence", "显示证据目录", false)
                    .on_click(cx.listener(move |_, _, _, cx| cx.reveal_path(&folder))),
            );
        }
        let body = if state.tab == "spec" {
            let proposal = &detail["proposal"];
            let manifest = serde_json::to_string_pretty(&proposal["manifest"]).unwrap_or_default();
            format!("{}\n\n{}", string(proposal, "spec"), manifest)
        } else {
            string(&detail[state.tab], "text").to_owned()
        };
        let page = page.child(
            panel()
                .flex_none()
                .child(tabs)
                .when(detail[state.tab]["truncated"] == true, |panel| {
                    panel.child(caption("预览已截断，可从证据目录查看完整日志。"))
                })
                .child(
                    div()
                        .id("local-evidence")
                        .test_support()
                        .max_h(px(430.))
                        .overflow_y_scroll()
                        .child(detail_text("local-evidence-text", &body).font_family("Menlo")),
                ),
        );
        h_flex()
            .size_full()
            .bg(rgb(CANVAS))
            .child(self.rail(cx))
            .child(page)
    }
}
