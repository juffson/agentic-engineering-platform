use super::*;
use crate::relay::{self, Relay, Round};

fn phase(name: &str) -> &str {
    match name {
        "preparing" => "准备工作区",
        "implementing" => "Codex 实现",
        "checking" => "运行验收",
        "reviewing" => "Claude 审查",
        "complete" => "等待人工审查",
        _ => "启动执行器",
    }
}

impl Studio {
    pub(super) fn relay_page(&self, cx: &Context<Self>) -> Div {
        let picked = self
            .relays
            .iter()
            .find(|relay| Some(&relay.id) == self.relay_id.as_ref());
        let task = self
            .relay_tasks
            .iter()
            .find(|task| Some(&task.name) == self.relay_task.as_ref());
        let mut list = v_flex()
            .id("relay-list")
            .overflow_y_scroll()
            .h_full()
            .min_h_0()
            .w(px(265.))
            .flex_none()
            .border_r_1()
            .border_color(rgb(LINE))
            .p_3()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .gap_1()
                    .child(heading("已派发任务"))
                    .child(
                        button(
                            "load-relay-tasks",
                            if self.tasks_loading {
                                "读取中"
                            } else {
                                "刷新任务"
                            },
                            false,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.load_relay_tasks(cx))),
                    ),
            );
        if self.relay_tasks.is_empty() && !self.tasks_loading {
            list = list.child(caption("可从 origin 上已派发的任务发起接力。"));
        }
        for task in &self.relay_tasks {
            let name = task.name.clone();
            list = list.child(
                item(
                    format!("relay-task-{name}"),
                    Some(&name) == self.relay_task.as_ref(),
                )
                .child(t(name.clone(), FG).truncate())
                .child(caption(task.sha.get(..8).unwrap_or(&task.sha).to_owned()))
                .on_click(cx.listener(move |this, _, w, cx| {
                    this.relay_task = Some(name.clone());
                    this.relay_id = None;
                    this.relay_error = None;
                    this.relay_model
                        .update(cx, |input, cx| input.set_value("", w, cx));
                    cx.notify();
                })),
            );
        }
        list = list.child(heading("接力记录").mt_3());
        if self.relays.is_empty() {
            list = list.child(caption("此仓库暂无接力记录。"));
        }
        for relay in &self.relays {
            let id = relay.id.clone();
            let model = relay.model.clone();
            let summary = if relay.active(&self.snapshot) {
                ("运行中", MINT)
            } else if relay::passed(relay.run(Round::Second, &self.snapshot)) {
                ("两轮已完成", MINT)
            } else if relay.latest(Round::Second).is_some() {
                ("第二轮需处理", RED)
            } else if relay::passed(relay.run(Round::First, &self.snapshot)) {
                ("等待决策", AMBER)
            } else {
                ("第一轮需处理", RED)
            };
            list = list.child(
                item(id.clone(), Some(&id) == self.relay_id.as_ref())
                    .child(t(relay.task.name.clone(), FG).truncate())
                    .child(
                        h_flex()
                            .justify_between()
                            .child(chip(summary.0, summary.1))
                            .child(caption(id.get(6..14).unwrap_or(&id).to_owned())),
                    )
                    .on_click(cx.listener(move |this, _, w, cx| {
                        this.relay_id = Some(id.clone());
                        this.relay_task = None;
                        this.relay_error = None;
                        this.relay_model
                            .update(cx, |input, cx| input.set_value(&model, w, cx));
                        cx.notify();
                    })),
            );
        }

        let mut detail = v_flex()
            .id("relay-detail")
            .test_support()
            .overflow_y_scroll()
            .h_full()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .p_5()
            .gap_3();
        if let Some(error) = &self.relay_error {
            detail = detail.child(detail_text("relay-error", error).text_color(rgb(RED)));
        }
        for error in &self.snapshot.issues {
            detail = detail.child(caption(error.clone()));
        }
        if let Some(relay) = picked {
            detail = detail
                .child(heading(relay.task.name.clone()).text_2xl())
                .child(field("Spec SHA", &relay.task.sha))
                .child(self.relay_round(relay, Round::First, cx))
                .child(self.relay_round(relay, Round::Second, cx));
            if !relay.active(&self.snapshot) && !self.relay_launching {
                detail = detail.child(self.relay_controls(relay, cx));
            }
        } else if let Some(task) = task {
            let task = task.clone();
            detail = detail
                .child(heading(task.name.clone()).text_2xl())
                .child(field("Spec SHA", &task.sha))
                .child(
                    panel()
                        .child(heading("第一轮"))
                        .child(caption(
                            "Codex 实现，Claude 审查。运行会使用本机的模型账户和任务验收命令。",
                        ))
                        .child(self.relay_model_input())
                        .when(!self.relay_launching, |card| {
                            card.child(button("start-relay", "发起第一轮", true).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    let id = relay::new_id();
                                    this.relay_id = Some(id.clone());
                                    this.relay_task = None;
                                    let task = task.clone();
                                    this.submit_relay(cx, move |ws, runner, model| {
                                        relay::start(ws, runner, id, task, model)
                                    });
                                }),
                            ))
                        }),
                );
        } else {
            detail = detail.child(heading(if self.relay_launching {
                "正在准备接力"
            } else {
                "选择任务或接力记录"
            }));
        }

        h_flex()
            .size_full()
            .bg(rgb(CANVAS))
            .child(self.rail(cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(
                        h_flex()
                            .justify_between()
                            .px_5()
                            .py_3()
                            .border_b_1()
                            .border_color(rgb(LINE))
                            .child(heading("两轮接力").text_2xl())
                            .child(
                                button("refresh-relays", "刷新进度", false)
                                    .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .min_h_0()
                            .items_start()
                            .child(list)
                            .child(detail),
                    ),
            )
    }

    fn relay_model_input(&self) -> Div {
        v_flex().gap_1().child(caption("Codex 模型（可选）")).child(
            Input::new(&self.relay_model)
                .id("relay-model")
                .aria_label("Codex 模型"),
        )
    }

    fn relay_round(&self, relay: &Relay, round: Round, cx: &Context<Self>) -> Div {
        let (title, key) = match round {
            Round::First => ("第一轮", "first"),
            Round::Second => ("第二轮", "second"),
        };
        let mut card = panel().child(heading(title));
        let Some(attempt) = relay.latest(round) else {
            return card.child(caption("尚未发起"));
        };
        let run = relay.run(round, &self.snapshot);
        let (progress, tone) = match run.map(|run| run.text("status")) {
            Some("awaiting_human_review") => ("验收和审查已完成，待人工审查", MINT),
            Some("failed") => ("运行失败", RED),
            Some("needs_reconciliation") => ("运行中断，需核对", RED),
            Some("running" | "initializing") => (phase(run.map_or("", |r| r.text("phase"))), MINT),
            _ if relay.busy => ("准备派发版本并启动执行器", MINT),
            _ => ("启动失败或已中断", RED),
        };
        card = card.child(
            detail_text(
                SharedString::from(format!("relay-{key}-progress")),
                progress,
            )
            .text_color(rgb(tone)),
        );
        if let Some(id) = attempt.decision {
            card = card.child(
                h_flex()
                    .gap_2()
                    .child(caption("本轮决策"))
                    .child(self.note_link(id, "relay-decision", cx)),
            );
        }
        if let Some(run) = run {
            let error = run.text("error");
            if !error.is_empty() {
                card = card.child(
                    detail_text(SharedString::from(format!("relay-{key}-failure")), error)
                        .text_color(rgb(RED)),
                );
            }
            if run.text("status") != "unstarted" {
                card = card.child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .child(caption("执行证据"))
                        .child(self.open_run_button(
                            format!("relay-{key}-evidence").into(),
                            &run.id,
                            cx,
                        )),
                );
            }
        }
        if !relay.busy
            && run.is_none_or(|run| run.text("status") == "unstarted")
            && let Some(log) = attempt.diagnostics.as_deref()
        {
            card = card.child(
                detail_text(SharedString::from(format!("relay-{key}-launch-error")), log)
                    .text_color(rgb(RED)),
            );
        }
        for previous in relay
            .attempts
            .iter()
            .filter(|previous| previous.round == round && previous.id != attempt.id)
        {
            if self
                .snapshot
                .runs
                .iter()
                .any(|run| run.id == previous.id && run.text("status") != "unstarted")
            {
                card = card.child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .child(caption("先前尝试"))
                        .child(self.open_run_button(
                            format!("relay-attempt-{}", previous.id).into(),
                            &previous.id,
                            cx,
                        )),
                );
            } else if let Some(log) = &previous.diagnostics {
                card = card.child(
                    v_flex()
                        .gap_1()
                        .child(caption("先前启动失败"))
                        .child(t(log.clone(), RED).text_xs()),
                );
            }
        }
        card
    }

    fn relay_controls(&self, relay: &Relay, cx: &Context<Self>) -> Div {
        let id = relay.id.clone();
        if !relay::passed(relay.run(Round::First, &self.snapshot)) {
            return panel().child(self.relay_model_input()).child(
                button("retry-relay-first", "重试第一轮", true).on_click(cx.listener(
                    move |this, _, _, cx| {
                        let id = id.clone();
                        this.submit_relay(cx, move |ws, runner, model| {
                            relay::advance(ws, runner, &id, Round::First, None, model)
                        });
                    },
                )),
            );
        }
        if relay::passed(relay.run(Round::Second, &self.snapshot)) {
            return div();
        }
        let questions = relay.questions(&self.snapshot);
        let decisions = relay.decisions(&self.snapshot);
        let mut card = panel().child(heading("交接决策"));
        if questions.is_empty() {
            let run = relay
                .latest(Round::First)
                .map(|attempt| attempt.id.clone())
                .unwrap_or_default();
            card = card
                .child(caption(
                    "第一轮没有留下问题。可新建任务决策，或使用该 Spec 下的现有决策。",
                ))
                .child(
                    button("relay-write-decision", "写任务决策", false).on_click(cx.listener(
                        move |this, _, w, cx| this.compose(Start::FromRun(run.clone()), w, cx),
                    )),
                );
        }
        for question in questions {
            let mut current = question;
            while let Some(next) = current["successor"].as_i64()
                && current["id"].as_i64().is_some_and(|id| next > id)
                && let Some(note) = self.snapshot.notes.iter().find(|note| note["id"] == next)
            {
                current = note;
            }
            let Some(question_id) = current["id"].as_i64() else {
                continue;
            };
            if current["kind"] == "decision" && current["successor"].is_null() {
                continue;
            }
            card = card.child(
                v_flex()
                    .gap_2()
                    .child(t(string(current, "body").to_owned(), FG))
                    .child(
                        button(format!("relay-resolve-{question_id}"), "修订为决策", true)
                            .on_click(cx.listener(move |this, _, w, cx| {
                                this.compose(Start::Resolve(question_id), w, cx)
                            })),
                    ),
            );
        }
        if !decisions.is_empty() {
            card = card.child(self.relay_model_input());
        }
        let action = if relay.latest(Round::Second).is_some() {
            "用此决策重试第二轮"
        } else {
            "用此决策发起第二轮"
        };
        for note in decisions {
            let Some(decision) = note["id"].as_i64() else {
                continue;
            };
            let id = id.clone();
            card = card.child(
                v_flex()
                    .gap_2()
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .pt_2()
                    .child(t(format!("#{decision}  {}", string(note, "body")), FG))
                    .child(
                        button(format!("relay-continue-{decision}"), action, true).on_click(
                            cx.listener(move |this, _, _, cx| {
                                let id = id.clone();
                                this.submit_relay(cx, move |ws, runner, model| {
                                    relay::advance(
                                        ws,
                                        runner,
                                        &id,
                                        Round::Second,
                                        Some(decision),
                                        model,
                                    )
                                });
                            }),
                        ),
                    ),
            );
        }
        card
    }
}
