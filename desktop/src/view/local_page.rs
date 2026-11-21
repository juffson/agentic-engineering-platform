use super::*;
use anyhow::{Context as _, ensure};
use std::process::Command;

const FIELDS: [(&str, &str, &str); 5] = [
    ("ticket", "任务票据", "如 volume-fix"),
    ("task", "Spec 任务目录", "如 market-volume-dedup-rust"),
    ("spec", "Spec 提交或本地引用", "完整 SHA 或本地分支"),
    ("target", "本地目标分支", "main"),
    ("model", "Codex 模型", "可选，默认使用本机配置"),
];
const TABS: [(&str, &str); 4] = [
    ("spec", "Spec"),
    ("diff", "Diff"),
    ("checks", "验收"),
    ("review", "审查"),
];

pub(crate) struct State {
    fields: [Entity<InputState>; FIELDS.len()],
    rows: Vec<Value>,
    selected: Option<String>,
    detail: Value,
    tab: &'static str,
    loading: bool,
    operation: Option<&'static str>,
    cancelling: bool,
    error: Option<String>,
}

fn call(ws: &Workspace, runner: &Runner, args: &[String]) -> Result<Vec<u8>> {
    let output = Command::new(runner.binary()?)
        .current_dir(&ws.root)
        .arg("local")
        .args(args)
        .env("PATH", runner.path()?)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove(aep::CONTROL_OPERATOR_TOKEN)
        .env_remove(aep::CONTROL_DEVICE_TOKEN)
        .output()
        .context("运行 AEP 本地任务命令")?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}

fn label(phase: &str) -> (&str, u32) {
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
        _ => ("载入中", MUTED),
    }
}

impl Studio {
    pub(crate) fn open_local(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.local_tasks.is_none() {
            self.local_tasks = Some(State {
                fields: FIELDS
                    .map(|(_, _, hint)| cx.new(|cx| InputState::new(window, cx).placeholder(hint))),
                rows: Vec::new(),
                selected: None,
                detail: Value::Null,
                tab: "spec",
                loading: false,
                operation: None,
                cancelling: false,
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
        let generation = self.workspace_generation;
        let job = cx.background_executor().spawn(async move {
            let rows =
                serde_json::from_slice::<Vec<Value>>(&call(&ws, &runner, &["list".into()])?)?;
            let detail = if let Some(ticket) = selected {
                serde_json::from_slice(&call(
                    &ws,
                    &runner,
                    &["inspect".into(), "--ticket".into(), ticket],
                )?)?
            } else {
                Value::Null
            };
            Ok::<_, anyhow::Error>((rows, detail))
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
                    Ok((rows, detail)) => {
                        state.rows = rows;
                        if state.selected == requested {
                            state.detail = detail;
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
            state.detail = Value::Null;
            state.tab = "spec";
        }
        state.error = None;
        let runner = self.runner.clone();
        let generation = self.workspace_generation;
        let job = cx.background_executor().spawn(async move {
            let mut command = vec![operation.to_owned()];
            command.extend(args);
            call(&ws, &runner, &command).map(|_| ())
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
                state.error = result.err().map(|error| format!("{error:#}"));
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
        self.local_command("plan", ticket, args, cx);
    }

    fn local_page_action(&mut self, operation: &'static str, cx: &mut Context<Self>) {
        let Some(state) = &self.local_tasks else {
            return;
        };
        let detail = &state.detail;
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
        self.local_command(operation, ticket, args, cx);
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
            .child(heading("本地任务"));
        let mut form = panel().flex_none().child(heading("拟定任务计划"));
        for ((id, name, _), input) in FIELDS.iter().zip(&state.fields) {
            form = form.child(
                v_flex().gap_1().child(caption(*name)).child(
                    div()
                        .id(format!("local-{id}"))
                        .test_support()
                        .child(Input::new(input)),
                ),
            );
        }
        let mut controls = h_flex().gap_2();
        if state.operation.is_none() {
            controls = controls
                .child(
                    button("local-plan", "生成计划", true)
                        .on_click(cx.listener(|this, _, _, cx| this.propose_local(false, cx))),
                )
                .child(
                    button("local-revise", "修订计划", false)
                        .on_click(cx.listener(|this, _, _, cx| this.propose_local(true, cx))),
                );
        }
        page = page.child(form.child(controls));
        if let Some(error) = &state.error {
            page = page.child(detail_text("local-command-error", error));
        }
        if !state.rows.is_empty() {
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
        if let Some(sha) = detail["run"]["candidate_sha"].as_str() {
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
                let (description, tone) = status(string(run, "status"));
                review = review.child(
                    h_flex()
                        .gap_2()
                        .child(caption(string(run, "id")))
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
            && (state.operation.is_none() || op == "cancel")
        {
            review =
                review.child(h_flex().child(
                    button(format!("local-{op}"), title, true).on_click(
                        cx.listener(move |this, _, _, cx| this.local_page_action(op, cx)),
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
