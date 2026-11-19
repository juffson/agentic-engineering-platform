use super::*;
use crate::{
    lifecycle,
    task_queue::{self, Action, Job, State},
};

impl Studio {
    pub(crate) fn attach_task_queue(&mut self, cx: &mut Context<Self>) {
        let source = if let Some(fixture) = &self.lifecycle {
            self.queue_source
                .as_ref()
                .filter(|ws| ws.root == fixture.source)
                .cloned()
                .or_else(|| Workspace::open(&fixture.source).ok())
        } else {
            self.workspace.clone()
        };
        if self.queue_source.as_ref().map(|ws| &ws.data) != source.as_ref().map(|ws| &ws.data) {
            self.task_queue = task_queue::Queue::default();
            self.queue_driving = false;
            self.queue_error = None;
            self.queue_generation += 1;
        }
        self.queue_source = source;
        self.drive_task_queue(cx);
        if self.queue_polling {
            return;
        }
        self.queue_polling = true;
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            loop {
                executor.timer(Duration::from_millis(1400)).await;
                if this
                    .update(cx, |this, cx| this.drive_task_queue(cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    fn drive_task_queue(&mut self, cx: &mut Context<Self>) {
        if self.queue_driving {
            return;
        }
        let Some(source) = self.queue_source.clone() else {
            return;
        };
        let runner = self.runner.clone();
        let generation = self.queue_generation;
        self.queue_driving = true;
        let work = cx
            .background_executor()
            .spawn(async move { task_queue::drive(&source, &runner) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if generation != this.queue_generation {
                    return;
                }
                this.queue_driving = false;
                match result {
                    Ok(queue) => {
                        let changed = queue != this.task_queue;
                        let active = queue.running() != 0 || changed;
                        this.task_queue = queue;
                        if this.page == Page::Relay && this.relay_id.is_none() {
                            this.relay_id = this.current_queue_job().map(|job| job.relay.clone());
                        }
                        if active && this.lifecycle.is_none() && this.page == Page::Lifecycle {
                            this.load_task_sessions(cx);
                        } else if active && this.lifecycle.is_some() {
                            this.reload(cx);
                        }
                        if changed {
                            cx.notify();
                        }
                    }
                    Err(error) => {
                        this.queue_error = Some(format!("任务队列无法更新：{error:#}"));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    pub(crate) fn enqueue_round(&mut self, relay: String, action: Action, cx: &mut Context<Self>) {
        if self.relay_launching
            || self
                .current_queue_job()
                .is_some_and(|job| job.state.active())
        {
            return;
        }
        let (Some(source), Some(ws), Some(fixture)) = (
            self.queue_source.clone(),
            self.workspace.clone(),
            self.lifecycle.clone(),
        ) else {
            self.queue_error = Some("找不到任务所属的原仓库。".into());
            cx.notify();
            return;
        };
        let model = self.relay_model.read(cx).value().trim().to_owned();
        let workspace = self.workspace_generation;
        let generation = self.queue_generation;
        self.relay_launching = true;
        self.queue_error = None;
        let id = relay.clone();
        let work = cx.background_executor().spawn(async move {
            task_queue::enqueue(&source, &ws, &fixture, id, action, model)?;
            task_queue::Queue::read(&source)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if workspace == this.workspace_generation {
                    this.relay_launching = false;
                    if result.is_ok() {
                        this.relay_id = Some(relay);
                    }
                }
                if generation != this.queue_generation {
                    return;
                }
                match result {
                    Ok(queue) => this.task_queue = queue,
                    Err(error) => this.queue_error = Some(format!("加入队列失败：{error:#}")),
                }
                this.drive_task_queue(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn set_parallelism(&mut self, limit: usize, cx: &mut Context<Self>) {
        let Some(source) = self.queue_source.clone() else {
            return;
        };
        let generation = self.queue_generation;
        self.queue_error = None;
        let save = cx.background_executor().spawn(async move {
            task_queue::set_limit(&source, limit)?;
            task_queue::Queue::read(&source)
        });
        cx.spawn(async move |this, cx| {
            let result = save.await;
            let _ = this.update(cx, |this, cx| {
                if generation != this.queue_generation {
                    return;
                }
                match result {
                    Ok(queue) => this.task_queue = queue,
                    Err(error) => this.queue_error = Some(format!("保存并发数失败：{error:#}")),
                }
                this.drive_task_queue(cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn current_queue_job(&self) -> Option<&Job> {
        let session = self.workspace.as_ref().and_then(lifecycle::root)?;
        self.task_queue.latest(session)
    }

    pub(super) fn job_label(&self, job: &Job) -> String {
        let round = job.action.label();
        match job.state {
            State::Waiting => format!(
                "{round}排队中 · 第 {} 位",
                self.task_queue.position(job).unwrap_or_default()
            ),
            State::Running => format!("{round}运行中"),
            State::Passed => format!("{round}已完成"),
            State::Failed(_) => format!("{round}执行失败"),
        }
    }

    pub(super) fn queue_settings(&self, cx: &Context<Self>) -> Div {
        let mut actions = h_flex().gap_2().items_center().child(caption("最大并发"));
        for limit in 1..=task_queue::MAX_PARALLEL {
            actions = actions.child(
                button(
                    format!("queue-limit-{limit}"),
                    limit.to_string(),
                    limit == self.task_queue.limit,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set_parallelism(limit, cx))),
            );
        }
        panel()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .flex_wrap()
                    .gap_2()
                    .child(heading("执行队列"))
                    .child(actions),
            )
            .child(detail_text(
                "task-queue-summary",
                &format!(
                    "{} 个运行中 · {} 个排队",
                    self.task_queue.running(),
                    self.task_queue.pending()
                ),
            ))
    }
}
