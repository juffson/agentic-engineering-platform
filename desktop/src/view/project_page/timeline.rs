use super::*;
use aep::project::timeline::{Note, Timeline};
pub(super) struct State {
    pub(super) work: String,
    pub(super) project: String,
    request: uuid::Uuid,
    snapshot: Option<std::path::PathBuf>,
    data: Option<Result<Timeline, String>>,
    selected: Option<usize>,
}
fn stage_name(w: &Work, stage: Option<usize>) -> &str {
    stage
        .and_then(|i| w.template.stages().get(i))
        .map_or("阶段待关联", |s| s.name)
}
fn ids(notes: &[Note]) -> String {
    notes
        .iter()
        .map(|n| format!("{} #{}", n.store, n.id))
        .collect::<Vec<_>>()
        .join("、")
}
impl Studio {
    pub(super) fn load_timeline(&mut self, cx: &mut Context<Self>) {
        let Some(p) = self.board.library.project() else {
            return;
        };
        let Some(work) = p.selected().cloned() else {
            return;
        };
        let Some(home) = self.board.dir.clone() else {
            return;
        };
        let project = p.id.clone();
        let request = uuid::Uuid::new_v4();
        self.board.desk = None;
        self.board.timeline = Some(State {
            work: work.id.clone(),
            project: project.clone(),
            request,
            snapshot: None,
            data: None,
            selected: None,
        });
        let job = cx.background_executor().spawn(async move {
            aep::project::timeline::load(&home, &project, &work).map_err(|e| format!("{e:#}"))
        });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |s, cx| {
                if let Some(t) = s.board.timeline.as_mut()
                    && t.request == request
                {
                    t.data = Some(result);
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }
    pub(super) fn work_timeline(&self, state: &State, w: &Work, cx: &Context<Self>) -> Div {
        let mut body = v_flex().gap_3().child(
            h_flex()
                .justify_between()
                .child(heading(w.name.clone()))
                .child(
                    button("work-timeline-refresh", "刷新记录", false)
                        .on_click(cx.listener(|s, _, _, cx| s.load_timeline(cx))),
                ),
        );
        let mut links = h_flex().gap_2().flex_wrap();
        for (i, repo) in w.repositories.iter().enumerate() {
            let path = repo.clone();
            links = links.child(
                button(
                    format!("timeline-link-{i}"),
                    format!("关联历史会话 · {}", basename(repo)),
                    false,
                )
                .on_click(cx.listener(move |s, _, window, cx| {
                    s.section(Section::Settings, window, cx);
                    s.show_zcode_history(&path, cx);
                })),
            );
        }
        if state.snapshot.is_none() {
            body = body.child(links);
        }
        let history = match &state.data {
            None => return body.child(caption("正在读取执行记录…")),
            Some(Err(e)) => return body.child(t(e.clone(), AMBER)),
            Some(Ok(t)) => t,
        };
        if state.snapshot.is_some() {
            body = body.child(detail_text("delivery-snapshot-open", "归档时间线"));
        }
        for delivery in &history.deliveries {
            let mut archive = panel()
                .child(detail_text(
                    SharedString::from(format!("delivery-archive-{}", delivery.run)),
                    "交付已归档",
                ))
                .child(field("独立验收执行", &delivery.accepted_run))
                .child(field("交付执行", &delivery.run))
                .child(caption(format!(
                    "{} 次执行 · {}",
                    delivery.attempts, delivery.archived_at
                )));
            if let Some(home) = &self.board.dir {
                let dir = delivery.directory(home);
                let receipt = delivery.clone();
                let home = home.clone();
                archive = archive.child(
                    h_flex()
                        .gap_2()
                        .child(
                            button(
                                format!("delivery-snapshot-{}", delivery.run),
                                "查看归档时间线",
                                false,
                            )
                            .on_click(cx.listener(
                                move |s, _, _, cx| {
                                    if let Some(view) = s.board.timeline.as_mut() {
                                        view.request = uuid::Uuid::new_v4();
                                        view.data = Some(
                                            receipt.snapshot(&home).map_err(|e| format!("{e:#}")),
                                        );
                                        view.snapshot = Some(receipt.directory(&home));
                                        view.selected = None;
                                        cx.notify();
                                    }
                                },
                            )),
                        )
                        .child(
                            button(
                                format!("delivery-files-{}", delivery.run),
                                "在 Finder 中查看证据",
                                false,
                            )
                            .on_click(cx.listener(move |_, _, _, cx| cx.reveal_path(&dir))),
                        ),
                );
            }
            body = body.child(archive);
        }
        for e in &history.errors {
            body = body.child(t(e.clone(), AMBER));
        }
        if history.rounds.is_empty() {
            return body.child(caption("暂无执行记录。可在项目设置的「ZCode 任务归组」中关联历史会话，或在此事项中启动阶段任务。"));
        }
        for (i, r) in history.rounds.iter().enumerate() {
            let title = match r.round {
                Some(n) => format!("第 {n} 轮 · 第 {} 次执行", r.attempt),
                None => format!("{} · 第 {} 次执行", stage_name(w, r.stage), r.attempt),
            };
            let expanded = state.selected == Some(i);
            let mut card = panel()
                .flex_none()
                .child(
                    h_flex()
                        .justify_between()
                        .child(detail_text(
                            SharedString::from(format!("timeline-title-{i}")),
                            &title,
                        ))
                        .child(
                            button(
                                format!("timeline-detail-{i}"),
                                if expanded {
                                    "收起"
                                } else {
                                    "记忆与交接"
                                },
                                expanded,
                            )
                            .on_click(cx.listener(
                                move |s, _, _, cx| {
                                    if let Some(t) = s.board.timeline.as_mut() {
                                        t.selected = (!expanded).then_some(i);
                                    }
                                    cx.notify();
                                },
                            )),
                        ),
                )
                .child(caption(format!("{} · {}", r.run, status(&r.status).0)));
            if let Some(dir) = &state.snapshot {
                let folder = dir.join(aep::project::archive::run_directory(i, &r.run));
                card = card.child(
                    button(
                        format!("timeline-archived-evidence-{i}"),
                        "查看本次归档证据",
                        false,
                    )
                    .on_click(cx.listener(move |_, _, _, cx| cx.reveal_path(&folder))),
                );
            } else if r.round.is_some()
                && let Some(repo) = r.repository.clone()
            {
                let run = r.run.clone();
                card = card.child(
                    button(format!("timeline-evidence-{i}"), "查看本轮证据", false).on_click(
                        cx.listener(move |s, _, window, cx| {
                            s.project_tool(&repo, Page::Runs, None, window, cx);
                            s.run = Some(run.clone());
                            cx.notify();
                        }),
                    ),
                );
            }
            for sent in history.stage_transfers.iter().filter(|t| t.run == r.run) {
                card = card.child(
                    v_flex()
                        .child(detail_text(
                            SharedString::from(format!("timeline-transfer-source-{i}")),
                            "已确认交接",
                        ))
                        .child(caption(sent.context())),
                );
            }
            if let Some(received) = &r.read_stage_transfer {
                card = card.child(
                    v_flex()
                        .child(detail_text(
                            SharedString::from(format!("timeline-transfer-destination-{i}")),
                            "本次执行已读取阶段交接",
                        ))
                        .child(caption(received.context())),
                );
            }
            if state.snapshot.is_none()
                && let Some(i_stage) = r.stage
                && i_stage + 1 < w.template.stages().len()
                && w.stage <= i_stage
                && ((r.round == Some(2)
                    && r.status == "awaiting_human_review"
                    && !history
                        .rounds
                        .iter()
                        .skip(i + 1)
                        .any(|other| other.record == r.record))
                    || (r.status == "succeeded" && w.current_attempt.get(&i_stage) == Some(&r.run)))
            {
                let run = r.run.clone();
                let work = w.id.clone();

                card = card.child(
                    button(
                        format!("confirm-stage-transfer-{run}"),
                        if w.stage < i_stage {
                            format!(
                                "确认{}交接并将事项推进到{}",
                                w.template.stages()[i_stage].name,
                                w.template.stages()[i_stage + 1].name
                            )
                        } else {
                            format!("确认交接到{}", w.template.stages()[i_stage + 1].name)
                        },
                        true,
                    )
                    .on_click(cx.listener(move |s, _, window, cx| {
                        let Some(home) = s.board.dir.clone() else {
                            return;
                        };
                        if s.change_board(cx, |lib| {
                            let p = lib.project_mut()?;
                            ensure!(p.selected.as_ref() == Some(&work), "事项已切换");
                            p.confirm_transfer(&home, &work, i_stage, &run)?;
                            Ok(())
                        }) {
                            s.section(Section::Work, window, cx);
                        }
                    })),
                );
            }
            if let Some(decision) = r.decision {
                card = card.child(caption(format!("本轮决策 · 仓库 #{decision}")));
            }
            for (j, a) in r.agents.iter().enumerate() {
                let mut bar = h_flex().justify_between().gap_2().child(
                    t(
                        format!(
                            "{} · {} · {}",
                            a.role,
                            match a.client.as_str() {
                                "zcode" => "ZCode",
                                "claude" => "Claude",
                                "codex" => "Codex",
                                c => c,
                            },
                            stage_name(w, a.stage)
                        ),
                        FG,
                    )
                    .font_semibold(),
                );
                if let Some(dir) = a.desktop_dir.clone() {
                    bar = bar.child(
                        button(format!("timeline-open-{i}-{j}"), "打开 ZCode 原会话", false)
                            .on_click(
                                cx.listener(move |s, _, _, cx| s.open_zcode_task(dir.clone(), cx)),
                            ),
                    );
                }
                card = card.child(bar);
                if let Some((_, msg)) = self
                    .zcode_navigation
                    .as_ref()
                    .filter(|(p, _)| Some(p) == a.desktop_dir.as_ref())
                {
                    card = card.child(detail_text(
                        SharedString::from(format!("timeline-open-status-{i}-{j}")),
                        msg,
                    ));
                }
                if !a.reads.is_empty() {
                    card = card.child(detail_text(
                        SharedString::from(format!("timeline-reads-{i}-{j}")),
                        &format!("读取 · {}", ids(&a.reads)),
                    ));
                }
                if !a.writes.is_empty() {
                    card = card.child(detail_text(
                        SharedString::from(format!("timeline-writes-{i}-{j}")),
                        &format!("写入 · {}", ids(&a.writes)),
                    ));
                }
                if expanded {
                    if let Some(model) = &a.model {
                        card = card.child(field("选择的模型", model));
                    }
                    if let Some(session) = &a.session {
                        card = card.child(field("会话 ID", session));
                    }
                    if !a.handoff.is_empty() {
                        card = card.child(field(
                            if a.role == "审查" {
                                "审查报告"
                            } else {
                                "交接记录"
                            },
                            &a.handoff,
                        ));
                    }
                    for n in a.reads.iter().chain(&a.writes) {
                        card = card.child(field(
                            "共享记忆",
                            &format!(
                                "{} #{} · {}{}\n{}",
                                n.store,
                                n.id,
                                kind(&n.kind),
                                n.replaces
                                    .map_or(String::new(), |id| format!(" · 修订自 #{id}")),
                                n.body
                            ),
                        ));
                    }
                }
            }
            body = body.child(card);
        }
        body
    }
}
