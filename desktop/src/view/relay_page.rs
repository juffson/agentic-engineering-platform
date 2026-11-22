use super::*;
use crate::relay::RelayAgents;
use crate::{
    relay::{self, Relay, Round},
    task_queue::{Action, State},
};
use aep::{project::RecordKind, workflow::Client};

impl Studio {
    pub(in crate::view) fn select_relay_record(
        &mut self,
        id: &str,
        w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(record) = self.relays.iter().find(|r| r.id == id) {
            self.relay_id = Some(record.id.clone());
            self.relay_task = None;
            self.relay_error = None;
            let agents = record.agents.clone();
            self.relay_form_round = None;
            self.set_relay_agents(&agents, w, cx);
            cx.notify();
        }
    }
    pub(super) fn relay_page(&self, cx: &Context<Self>) -> Div {
        let picked = self.relays.iter().find(|relay| {
            Some(&relay.id) == self.relay_id.as_ref()
                && self.record_visible(RecordKind::Relay, &relay.id)
        });
        let task = self
            .relay_tasks
            .iter()
            .find(|task| Some(&task.name) == self.relay_task.as_ref());
        let mut list = v_flex()
            .id("relay-list")
            .test_support()
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
        let completed_setup = self.lifecycle.is_some() && !self.relays.is_empty();
        for task in self.relay_tasks.iter().filter(|_| !completed_setup) {
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
                    this.relay_form_round = None;
                    this.reset_relay_agents(w, cx);
                    cx.notify();
                })),
            );
        }
        list = list.child(heading("接力记录").mt_3());
        if self.relays.is_empty() {
            list = list.child(caption("此仓库暂无接力记录。"));
        }
        if self.context_work().is_some() {
            list = list.child(self.record_picker(RecordKind::Relay, cx));
        }
        for relay in &self.relays {
            if !self.record_visible(RecordKind::Relay, &relay.id) {
                if let Some(choice) =
                    self.record_choice(RecordKind::Relay, &relay.id, &relay.task.name, cx)
                {
                    list = list.child(choice);
                }
                continue;
            }
            let id = relay.id.clone();
            let summary = if relay.active(&self.snapshot) {
                ("运行中", MINT)
            } else if relay
                .latest(Round::Second)
                .is_some_and(|attempt| attempt.finalized())
            {
                ("已收尾", MINT)
            } else if relay
                .latest(Round::Second)
                .is_some_and(|attempt| attempt.merged())
            {
                ("PR 已合并", MINT)
            } else if relay
                .latest(Round::Second)
                .and_then(|attempt| attempt.publication.as_ref())
                .is_some_and(|saved| saved.pr.is_some())
            {
                ("已创建 PR", MINT)
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
                        this.select_relay_record(&id, w, cx);
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
        let job = self
            .current_queue_job()
            .filter(|j| self.record_visible(RecordKind::Relay, &j.relay));
        let pending = job.is_some_and(|job| job.state.active());
        if let Some(job) = job {
            if pending {
                detail = detail
                    .child(
                        detail_text("relay-queue-state", &self.job_label(job))
                            .text_color(rgb(BLUE)),
                    )
                    .child(self.queue_controls(job, cx));
            } else if let State::Failed(error) = &job.state {
                detail =
                    detail.child(detail_text("relay-queue-failure", error).text_color(rgb(RED)));
            }
        }
        if let Some(error) = self.relay_error.as_ref().or(self.queue_error.as_ref()) {
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
            if !relay.active(&self.snapshot) && !self.relay_launching && !pending {
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
                        .child(caption("选择本轮实现端和审查端。验收命令来自任务 Spec。"))
                        .child(self.relay_model_input(cx))
                        .when(!self.relay_launching && !pending, |card| {
                            card.child(button("start-relay", "发起第一轮", true).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.start_relay(task.clone(), cx);
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

    pub(crate) fn reset_relay_agents(&mut self, w: &mut Window, cx: &mut Context<Self>) {
        let agents = RelayAgents {
            zcode_model: self.relay_project_default(),
            ..Default::default()
        };
        self.set_relay_agents(&agents, w, cx);
    }
    pub(super) fn sync_relay_form(&mut self, w: &mut Window, cx: &mut Context<Self>) {
        let Some(relay) = self
            .relays
            .iter()
            .find(|r| Some(&r.id) == self.relay_id.as_ref())
        else {
            return;
        };
        let second = relay.latest(Round::Second).is_some()
            || relay::passed(relay.run(Round::First, &self.snapshot));
        let round = if second { Round::Second } else { Round::First };
        let key = (relay.id.clone(), round);
        if self.relay_form_round.as_ref() == Some(&key) {
            return;
        }
        let mut agents = relay.agents.clone();
        if second && relay.latest(Round::Second).is_none() {
            agents.zcode_model = self.relay_project_default();
        }
        self.relay_form_round = Some(key);
        self.set_relay_agents(&agents, w, cx);
    }
    pub(crate) fn relay_agents(&self, cx: &App) -> RelayAgents {
        RelayAgents {
            implementer: self.relay_implementer,
            reviewer: self.relay_reviewer,
            model: self.relay_model.read(cx).value().trim().to_owned(),
            zcode_model: self.relay_zcode_model.read(cx).value().trim().to_owned(),
        }
    }

    pub(crate) fn set_relay_agents(
        &mut self,
        agents: &RelayAgents,
        w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if [agents.implementer, agents.reviewer].contains(&Client::ZCode) {
            self.ensure_zcode_catalog(cx);
        }
        self.relay_implementer = agents.implementer;
        self.relay_reviewer = agents.reviewer;
        self.relay_model
            .update(cx, |s, cx| s.set_value(&agents.model, w, cx));
        self.relay_zcode_model
            .update(cx, |s, cx| s.set_value(&agents.zcode_model, w, cx));
    }

    pub(super) fn relay_model_input(&self, cx: &Context<Self>) -> Div {
        let mut form = v_flex().gap_1();
        for (role, label, selected, default) in [
            (
                "implementer",
                "实现端",
                self.relay_implementer,
                Client::Codex,
            ),
            ("reviewer", "审查端", self.relay_reviewer, Client::Claude),
        ] {
            let mut row = h_flex().gap_2().child(caption(label));
            for client in [default, Client::ZCode] {
                row = row.child(
                    button(
                        format!("relay-{role}-{}", client.binary()),
                        client.label(),
                        selected == client,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if role == "implementer" {
                            this.relay_implementer = client;
                        } else {
                            this.relay_reviewer = client;
                        }
                        if client == Client::ZCode {
                            this.ensure_zcode_catalog(cx);
                        }
                        cx.notify();
                    })),
                );
            }
            form = form.child(row);
        }
        if self.relay_implementer == Client::Codex {
            form = form.child(caption("Codex 模型（可选）")).child(
                Input::new(&self.relay_model)
                    .id("relay-model")
                    .aria_label("Codex 模型"),
            );
        }
        if [self.relay_implementer, self.relay_reviewer].contains(&Client::ZCode) {
            form = form.child(self.relay_zcode_picker(cx));
        }
        form
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
        if let Some(run) = run {
            let request = &run.record["request"];
            if let (Some(implementation), Some(review)) =
                (request["source"].as_str(), request["reviewer"].as_str())
            {
                card = card.child(caption(format!(
                    "实现端 {implementation}  ·  审查端 {review}"
                )));
                if let Some(model) = request["zcode_model"].as_str() {
                    card = card.child(field("ZCode 模型", model));
                }
            }
        }
        let (progress, tone) = match run.map(|run| run.text("status")) {
            Some("awaiting_human_review") if attempt.finalized() => ("合并与收尾已完成", MINT),
            Some("awaiting_human_review") if attempt.merged() => ("GitHub PR 已合并", MINT),
            Some("awaiting_human_review")
                if attempt
                    .publication
                    .as_ref()
                    .is_some_and(|saved| saved.pr.is_some()) =>
            {
                ("已创建 GitHub PR", MINT)
            }
            Some("awaiting_human_review")
                if attempt
                    .publication
                    .as_ref()
                    .is_some_and(|saved| saved.pushed) =>
            {
                ("已推送候选，待创建 PR", AMBER)
            }
            Some("awaiting_human_review") => ("验收和审查已完成，待人工审查", MINT),
            Some("failed") => ("运行失败", RED),
            Some("cancelled") => ("本轮已取消", AMBER),
            Some("needs_reconciliation") => ("运行中断，需核对", RED),
            Some("running" | "initializing") => (
                relay::phase_label(run.map_or("", |r| r.text("phase"))),
                MINT,
            ),
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
        if let Some(saved) = &attempt.publication {
            if let Some(pr) = &saved.pr {
                let url = pr.url(&saved.target);
                let publication = saved.clone();
                card = card.child(
                    button("relay-pr-progress", "查看 GitHub CI 与审查", true).on_click(
                        cx.listener(move |this, _, w, cx| {
                            this.open_github_pr(publication.clone(), w, cx)
                        }),
                    ),
                );
                card = card.child(
                    button(
                        "relay-created-pr",
                        format!("打开 GitHub PR #{} ↗", pr.number),
                        true,
                    )
                    .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url))),
                );
            }
            if let Some(error) = &saved.error {
                card = card.child(caption(error.clone()));
            }
        }
        if let Some(run) = run {
            if let Some(ws) = &self.workspace {
                let folder = ws.data.join("runs").join(&run.id);
                for (mode, role, phase) in [
                    ("edit", "实现", "implementing"),
                    ("plan", "审查", "reviewing"),
                ] {
                    let dir = folder.join(format!("zcode-{mode}"));
                    if let Ok(err) = aep::workspace::read(&dir.join("desktop-error.json"))
                        && let Some(reason) = err["error"].as_str()
                    {
                        card = card.child(caption(reason.to_owned()).text_color(rgb(RED)));
                    }
                    if let Ok(receipt) = aep::workspace::read(&dir.join("session.json"))
                        && receipt["transport"] == "desktop"
                    {
                        let text = format!(
                            "{} · {}/{} · {}",
                            receipt["session"].as_str().unwrap_or(""),
                            receipt["providerId"].as_str().unwrap_or(""),
                            receipt["modelId"].as_str().unwrap_or(""),
                            receipt["reasoningLevel"].as_str().unwrap_or("默认")
                        );
                        card = card.child(caption(format!("ZCode 桌面{role}回执"))).child(
                            detail_text(
                                SharedString::from(format!("relay-{key}-{mode}-receipt")),
                                &text,
                            ),
                        );
                    } else if run.text("status") == "running"
                        && run.text("phase") == phase
                        && let Some(dispatch) = self.desktop_dispatch(
                            &dir,
                            format!("relay-{key}-desktop-{mode}"),
                            role,
                            cx,
                        )
                    {
                        card = card.child(dispatch);
                    }
                }
                for note in self.snapshot.notes.iter().filter(|n| {
                    n["run_id"] == run.id
                        && matches!(n["kind"].as_str(), Some("handoff" | "review"))
                }) {
                    if let Some(body) = note["body"].as_str() {
                        card = card.child(field(
                            if note["kind"] == "review" {
                                "审查交接"
                            } else {
                                "实现交接"
                            },
                            body,
                        ));
                    }
                }
            }
            let detail = self
                .workspace
                .as_ref()
                .and_then(|ws| relay::implementation_failure(ws, run));
            let error = detail.as_deref().unwrap_or_else(|| run.text("error"));
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
            return panel().child(self.relay_model_input(cx)).child(
                button("retry-relay-first", "重试第一轮", true).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.advance_relay(id.clone(), Action::RetryFirst, cx);
                    },
                )),
            );
        }
        if relay::passed(relay.run(Round::Second, &self.snapshot)) {
            let Some(run) = relay
                .latest(Round::Second)
                .map(|attempt| attempt.id.clone())
            else {
                return div();
            };
            return panel().child(heading("最终人工审查")).child(
                button("open-human-review", "查看 Diff 与创建 PR", true).on_click(cx.listener(
                    move |this, _, window, cx| {
                        this.open_human_review(id.clone(), run.clone(), window, cx)
                    },
                )),
            );
        }
        let questions = relay.questions(&self.snapshot);
        let decisions = relay.decisions(&self.snapshot);
        let mut card = panel().child(heading("交接决策"));
        if self
            .lifecycle
            .as_ref()
            .is_some_and(|session| !session.real_task)
            && questions.is_empty()
        {
            return card.child(t(
                "第一轮没有写入未决问题。可查看本轮证据，或从原仓库启动新的隔离测试。",
                RED,
            ));
        }
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
            card = card.child(self.relay_model_input(cx));
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
                                this.advance_relay(id.clone(), Action::Second { decision }, cx);
                            }),
                        ),
                    ),
            );
        }
        card
    }
}
