use super::*;
use crate::{
    https_pilot::Connection,
    https_queue::{self as queue, Action, Run, Site},
};
use std::time::Instant;

impl Studio {
    pub(super) fn restore_https_queue(&mut self, source: &Workspace, cx: &mut Context<Self>) {
        if self
            .https_queue
            .as_ref()
            .is_none_or(|site| site.source.root != source.root)
        {
            self.https_queue = Site::restore(source);
        }
        self.poll_https_queue(cx);
    }

    pub(super) fn set_https_queue(
        &mut self,
        source: Workspace,
        connection: Connection,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        if self
            .https_queue
            .as_ref()
            .is_some_and(|site| site.source.root == source.root && site.connection == connection)
        {
            return Ok(());
        }
        let site = Site::new(source, connection);
        site.save()?;
        self.https_queue = Some(site);
        self.poll_https_queue(cx);
        Ok(())
    }

    pub(super) fn poll_https_queue(&mut self, cx: &mut Context<Self>) {
        if self.https_queue.as_ref().is_some_and(|site| {
            site.checked
                .is_none_or(|checked| checked.elapsed() >= Duration::from_secs(2))
        }) {
            self.https_queue_action(Action::Status, cx);
        }
    }

    fn https_queue_action(&mut self, action: Action, cx: &mut Context<Self>) {
        let Some(site) = self.https_queue.as_mut().filter(|site| !site.busy) else {
            return;
        };
        site.busy = true;
        let status = matches!(action, Action::Status);
        let cancelled = match &action {
            Action::Cancel(run) => Some(run.task.ticket.clone()),
            _ => None,
        };
        if !status {
            site.action_error = None;
            site.pending = None;
        }
        let source = site.source.clone();
        let connection = site.connection.clone();
        let runner = self.runner.clone();
        let generation = self.workspace_generation;
        let work_source = source.clone();
        let work_connection = connection.clone();
        let work = cx
            .background_executor()
            .spawn(async move { queue::perform(&work_source, &work_connection, &runner, action) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.workspace_generation != generation {
                    return;
                }
                let Some(site) = this.https_queue.as_mut().filter(|site| {
                    site.source.root == source.root && site.connection == connection
                }) else {
                    return;
                };
                site.busy = false;
                site.checked = Some(Instant::now());
                match result {
                    Ok(snapshot) => {
                        if site.pending.as_ref().is_some_and(|pending| {
                            !snapshot
                                .attempts
                                .iter()
                                .any(|run| same_cancel_target(run, pending))
                        }) {
                            site.pending = None;
                        }
                        site.snapshot = Some(snapshot);
                        site.error = None;
                        if let Some(ticket) = cancelled.as_deref() {
                            this.follow_https_cancellation(ticket, cx);
                        }
                    }
                    Err(error) if status => site.error = Some(format!("{error:#}")),
                    Err(error) => site.action_error = Some(format!("{error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn https_queue_panel(&self, site: &Site, cx: &Context<Self>) -> Div {
        let mut panel = panel()
            .flex_none()
            .child(heading("HTTPS 任务队列"))
            .child(detail_text(
                "https-queue-endpoint",
                &site.connection.endpoint,
            ));
        if let Some(error) = site.error.as_ref().or(site.action_error.as_ref()) {
            panel = panel.child(detail_text("https-queue-error", error).text_color(rgb(RED)));
        }
        let Some(snapshot) = &site.snapshot else {
            return panel.child(caption("正在读取控制面队列"));
        };
        let Some(queue) = &snapshot.queue else {
            return panel.child(caption("此控制面版本缺少队列管理接口"));
        };
        let editable = !site.busy && site.error.is_none();
        let pause = queue.paused;
        panel = panel.child(
            h_flex()
                .gap_3()
                .child(detail_text(
                    "https-queue-state",
                    if pause {
                        "派发已暂停"
                    } else {
                        "允许派发"
                    },
                ))
                .when(editable, |row| {
                    row.child(
                        button(
                            "https-queue-toggle",
                            if pause {
                                "恢复派发"
                            } else {
                                "暂停派发"
                            },
                            pause,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.https_queue_action(Action::Pause(!pause), cx)
                        })),
                    )
                }),
        );
        if !snapshot.workers.is_empty() {
            let online = snapshot
                .workers
                .values()
                .filter(|worker| worker.online)
                .count();
            panel = panel.child(caption(format!(
                "Worker 在线 {online}/{} · 等待 {} 项",
                snapshot.workers.len(),
                queue.order.len()
            )));
            for (id, worker) in &snapshot.workers {
                let task = snapshot.current(id).map_or_else(
                    || {
                        if worker.online {
                            "空闲".into()
                        } else {
                            "未在执行".into()
                        }
                    },
                    |run| format!("{} · {}", run.task.name, run.phase_label()),
                );
                panel = panel.child(
                    h_flex().justify_between().gap_2().child(
                        v_flex()
                            .gap_1()
                            .child(detail_text(
                                format!("https-worker-{id}"),
                                &format!(
                                    "{} · {}",
                                    id,
                                    if worker.online { "在线" } else { "离线" }
                                ),
                            ))
                            .child(detail_text(format!("https-worker-task-{id}"), &task)),
                    ),
                );
            }
        }
        let runs = snapshot.visible_runs();
        if runs.is_empty() {
            return panel.child(caption("没有等待领取或正在运行的任务"));
        }
        for run in runs {
            let state = run.phase_label();
            let mut actions = h_flex().gap_2();
            if editable
                && run.stage == "queued"
                && let Some(index) = queue.order.iter().position(|id| *id == run.id)
            {
                for (earlier, slot, label) in [
                    (true, index.checked_sub(1), "上移"),
                    (false, index.checked_add(1), "下移"),
                ] {
                    if let Some(adjacent) = slot.and_then(|i| queue.order.get(i)) {
                        let action = Action::Move {
                            run: run.id.clone(),
                            adjacent: adjacent.clone(),
                            earlier,
                        };
                        actions = actions.child(
                            button(
                                format!(
                                    "https-queue-{}-{}",
                                    if earlier { "up" } else { "down" },
                                    run.id
                                ),
                                label,
                                false,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| this.https_queue_action(action.clone(), cx),
                            )),
                        );
                    }
                }
            }
            if editable && run.can_cancel() {
                if site
                    .pending
                    .as_ref()
                    .is_some_and(|pending| same_cancel_target(run, pending))
                {
                    let action = Action::Cancel(run.clone());
                    actions = actions
                        .child(
                            button(format!("https-queue-confirm-{}", run.id), "确认取消", true)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.https_queue_action(action.clone(), cx)
                                })),
                        )
                        .child(
                            button(format!("https-queue-keep-{}", run.id), "保留任务", false)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(site) = &mut this.https_queue {
                                        site.pending = None;
                                    }
                                    cx.notify();
                                })),
                        );
                } else {
                    let pending = run.clone();
                    actions = actions.child(
                        button(format!("https-queue-cancel-{}", run.id), "取消任务", false)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(site) = &mut this.https_queue {
                                    site.pending = Some(pending.clone());
                                }
                                cx.notify();
                            })),
                    );
                }
            }
            let title = match queue.order.iter().position(|id| *id == run.id) {
                Some(index) => format!("{} · {state} · 第 {} 位", run.task.name, index + 1),
                None => format!("{} · {state}", run.task.name),
            };
            panel = panel.child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(detail_text(format!("https-queue-run-{}", run.id), &title))
                            .child(caption(run.task.target.clone())),
                    )
                    .child(actions),
            );
        }
        panel
    }
}

fn same_cancel_target(left: &Run, right: &Run) -> bool {
    left.id == right.id && left.stage == right.stage
}
