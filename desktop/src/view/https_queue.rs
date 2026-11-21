use super::*;
use crate::{
    https_pilot::Connection,
    https_queue::{self as queue, Action, KeyAction, KeyCommand, Run, Site},
};
use std::time::Instant;

impl Studio {
    pub(super) fn restore_https_queue(&mut self, source: &Workspace, cx: &mut Context<Self>) {
        let connection = self
            .project_site(&source.root)
            .cloned()
            .or_else(|| Site::restore(source).map(|s| s.connection));
        let current = self
            .https_queue
            .as_ref()
            .map(|s| (&s.source.root, &s.connection));
        if current != connection.as_ref().map(|c| (&source.root, c)) {
            self.https_queue = connection.map(|c| Site::new(source.clone(), c));
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
        if !self.in_settings() && self.context_work().is_none() {
            site.save()?;
        }
        self.https_queue = Some(site);
        self.poll_https_queue(cx);
        Ok(())
    }

    pub(super) fn poll_https_queue(&mut self, cx: &mut Context<Self>) {
        if self.context_work().is_none()
            && self.https_queue.as_ref().is_some_and(|site| {
                site.checked
                    .is_none_or(|checked| checked.elapsed() >= Duration::from_secs(2))
            })
        {
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
            site.pending_revoke = None;
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

    fn https_deploy_keys(
        &self,
        site: &Site,
        snapshot: &queue::Snapshot,
        cx: &Context<Self>,
    ) -> Div {
        let mut section = v_flex().gap_2().child(heading("Worker Deploy Key"));
        let current = snapshot
            .key_rotation
            .as_ref()
            .and_then(|plan| plan.current());
        let editable = !site.busy && site.error.is_none();
        if let Some(plan) = &snapshot.key_rotation {
            let text = if let Some(worker) = current {
                format!(
                    "正在轮换 {}/{} · {}",
                    plan.next + 1,
                    plan.workers.len(),
                    worker.worker
                )
            } else if plan.stopped {
                format!("轮换已停止 · {}/{} 已完成", plan.next, plan.workers.len())
            } else {
                format!("滚动轮换已完成 · {} 台 Worker", plan.workers.len())
            };
            section = section.child(detail_text("https-key-roll-state", &text));
            if let Some(worker) = current
                && editable
                && snapshot
                    .deploy_keys
                    .get(&worker.worker)
                    .is_some_and(|entry| entry.request.is_none())
            {
                let id = plan.id.clone();
                section = section.child(
                    button("https-key-roll-stop", "停止滚动轮换", false).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.https_queue_action(Action::StopRotation(id.clone()), cx)
                        },
                    )),
                );
            }
        }
        if current.is_none()
            && editable
            && snapshot.deploy_keys.len() >= 2
            && snapshot.deploy_keys.iter().all(|(worker, entry)| {
                entry.inventory.active.is_some()
                    && entry.inventory.staged.is_none()
                    && entry.inventory.retiring.is_none()
                    && entry.request.is_none()
                    && snapshot
                        .workers
                        .get(worker)
                        .is_some_and(|worker| worker.online)
            })
        {
            section = section.child(
                button("https-key-roll-start", "开始滚动轮换", true).on_click(cx.listener(
                    move |this, _, _, cx| this.https_queue_action(Action::StartRotation, cx),
                )),
            );
        }
        for (worker, entry) in &snapshot.deploy_keys {
            let ready = editable
                && current.is_none_or(|current| current.worker == *worker)
                && entry.request.is_none()
                && snapshot
                    .workers
                    .get(worker)
                    .is_some_and(|worker| worker.online);
            let mut card = v_flex().gap_1().child(detail_text(
                format!("https-key-scope-{worker}"),
                &format!("{} · {worker}", entry.repository),
            ));
            if let Some(plan) = &snapshot.key_rotation
                && let Some((index, _)) = plan
                    .workers
                    .iter()
                    .enumerate()
                    .find(|(_, item)| item.worker == *worker)
            {
                let label = if index < plan.next {
                    "轮换已完成"
                } else if plan.stopped {
                    "轮换已停止"
                } else if index == plan.next {
                    "当前轮换 · 新任务交给其他 Worker"
                } else {
                    "等待轮换"
                };
                card = card.child(detail_text(
                    format!("https-key-roll-worker-{worker}"),
                    label,
                ));
            }
            for (slot, label, key) in [
                ("active", "生效密钥", entry.inventory.active.as_ref()),
                ("staged", "待切换密钥", entry.inventory.staged.as_ref()),
                (
                    "retiring",
                    "待撤销旧密钥",
                    entry.inventory.retiring.as_ref(),
                ),
            ] {
                if let Some(key) = key {
                    card = card.child(detail_text(
                        format!("https-key-{slot}-{worker}"),
                        &format!("{label} · {}", key.fingerprint),
                    ));
                }
            }
            let status = if entry.request.is_some() {
                "等待 Worker 空闲后执行凭证操作"
            } else if current.is_some_and(|current| current.worker == *worker)
                && entry.inventory.staged.is_some()
                && entry
                    .last
                    .as_ref()
                    .is_some_and(|receipt| receipt.error.is_some())
            {
                "切换失败 · 保留当前密钥"
            } else if entry.verified_at.is_some() {
                "上次验证通过 · GitHub 只读 · Worker 可读取"
            } else if entry.inventory.active.is_some() {
                "等待验证生效密钥的只读权限"
            } else {
                "等待配置 Deploy Key"
            };
            card = card.child(detail_text(format!("https-key-status-{worker}"), status));
            if let Some(error) = entry
                .last
                .as_ref()
                .and_then(|receipt| receipt.error.as_ref())
            {
                card = card.child(
                    detail_text(format!("https-key-error-{worker}"), error).text_color(rgb(RED)),
                );
            }
            if ready {
                let command = |action| KeyCommand {
                    worker: worker.clone(),
                    installation: entry.installation.clone(),
                    rotation: current
                        .and_then(|_| snapshot.key_rotation.as_ref().map(|plan| plan.id.clone())),
                    action,
                };
                let mut buttons = h_flex().gap_2().flex_wrap();
                let control = |id: &str, label: &str, action| {
                    let action = Action::DeployKey(command(action));
                    button(format!("https-key-{id}-{worker}"), label.to_owned(), false).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.https_queue_action(action.clone(), cx)
                        }),
                    )
                };
                if let Some(key) = &entry.inventory.active {
                    buttons = buttons.child(control(
                        "verify",
                        "验证只读",
                        KeyAction::Verify(key.fingerprint.clone()),
                    ));
                }
                if let Some(staged) = &entry.inventory.staged {
                    buttons = buttons.child(control(
                        "install",
                        "注册只读并切换",
                        KeyAction::Install(staged.fingerprint.clone()),
                    ));
                } else if let Some(retiring) = &entry.inventory.retiring {
                    if entry.verified_at.is_some() {
                        let revoke = command(KeyAction::Revoke(retiring.fingerprint.clone()));
                        if site.pending_revoke.as_ref() == Some(&revoke) {
                            card = card.child(caption(format!(
                                "从 {} 撤销 {}",
                                entry.repository, retiring.fingerprint
                            )));
                            buttons = buttons.child(control(
                                "confirm-revoke",
                                "确认撤销旧密钥",
                                revoke.action,
                            ));
                        } else {
                            buttons = buttons.child(
                                button(
                                    format!("https-key-review-revoke-{worker}"),
                                    "撤销旧密钥",
                                    false,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        if let Some(site) = &mut this.https_queue {
                                            site.pending_revoke = Some(revoke.clone());
                                            cx.notify();
                                        }
                                    },
                                )),
                            );
                        }
                    }
                } else {
                    buttons = buttons.child(control(
                        "prepare",
                        if entry.inventory.active.is_some() {
                            "准备轮换"
                        } else {
                            "生成密钥"
                        },
                        KeyAction::Prepare,
                    ));
                }
                card = card.child(buttons);
            }
            section = section.child(card);
        }
        section
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
        if !snapshot.deploy_keys.is_empty() {
            panel = panel.child(self.https_deploy_keys(site, snapshot, cx));
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
