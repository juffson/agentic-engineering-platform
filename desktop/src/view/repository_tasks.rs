use super::*;
use crate::{
    lifecycle::{
        self,
        catalog::{self, Stage},
    },
    relay::{Round, Task},
    task_queue::State,
};
use anyhow::Context as _;
use std::path::PathBuf;

impl Studio {
    pub(crate) fn load_task_sessions(&mut self, cx: &mut Context<Self>) {
        if self.lifecycle.is_some() || self.task_sessions_loading {
            return;
        }
        let Some(ws) = self.workspace.clone() else {
            return;
        };
        self.task_sessions_loading = true;
        self.task_sessions_error = None;
        let generation = self.workspace_generation;
        let load = cx
            .background_executor()
            .spawn(async move { catalog::load(&ws) });
        cx.spawn(async move |this, cx| {
            let result = load.await;
            let _ = this.update(cx, |this, cx| {
                if generation != this.workspace_generation {
                    return;
                }
                this.task_sessions_loading = false;
                match result {
                    Ok(entries) => this.task_sessions = entries,
                    Err(error) => {
                        this.task_sessions_error = Some(format!("读取任务记录失败：{error:#}"))
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn resume_task(
        &mut self,
        path: PathBuf,
        stage: Stage,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.lifecycle_preparing || self.lifecycle_packaging {
            return;
        }
        if stage == Stage::PrepareRetry {
            match catalog::request(&path) {
                Ok(plan) => self.start_lifecycle(plan, window, cx),
                Err(error) => {
                    self.lifecycle_error = Some(format!("读取原始任务失败：{error:#}"));
                    cx.notify();
                }
            }
            return;
        }
        let generation = self.workspace_generation;
        self.lifecycle_preparing = true;
        self.lifecycle_error = None;
        let load = cx.background_executor().spawn(async move {
            let fixture = lifecycle::load_session(&path).context("缺少有效的任务凭据")?;
            let ws = Workspace::open(&path.join("workspace/repo"))?;
            let progress = catalog::Progress::read(&path, &ws, &fixture)?;
            Ok::<_, anyhow::Error>((ws, fixture, progress))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = load.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if generation != this.workspace_generation {
                    return;
                }
                this.lifecycle_preparing = false;
                match result {
                    Ok((ws, fixture, progress)) => {
                        this.set_workspace(ws, Page::Lifecycle, window, cx);
                        match progress.stage {
                            Stage::Prepared | Stage::Dispatched => {
                                this.lifecycle_error = progress.detail
                            }
                            Stage::Archived | Stage::Archive => {
                                this.lifecycle_error = progress.detail;
                                if progress.stage == Stage::Archive
                                    && this
                                        .workspace
                                        .as_ref()
                                        .and_then(lifecycle::output)
                                        .is_some_and(|out| out.join("14-completed.png").is_file())
                                {
                                    this.pack_lifecycle(cx);
                                }
                            }
                            _ => {
                                if let Some(relay) = progress.relay {
                                    let id = relay.id;
                                    this.relay_model.update(cx, |input, cx| {
                                        input.set_value(relay.model.clone(), window, cx)
                                    });
                                    let second = relay
                                        .attempts
                                        .into_iter()
                                        .rev()
                                        .find(|attempt| attempt.round == Round::Second);
                                    this.relay_id = Some(id.clone());
                                    match progress.stage {
                                        Stage::Review | Stage::PublishRetry => {
                                            if let Some(attempt) = second {
                                                this.open_human_review(id, attempt.id, window, cx);
                                            }
                                        }
                                        Stage::Checks
                                        | Stage::ChecksFailed
                                        | Stage::Merge
                                        | Stage::Cleanup => {
                                            if let Some(publication) =
                                                second.and_then(|attempt| attempt.publication)
                                            {
                                                this.open_github_pr(publication, window, cx);
                                            }
                                        }
                                        _ => this.navigate(Page::Relay, window, cx),
                                    }
                                } else {
                                    this.navigate(Page::Relay, window, cx);
                                    this.relay_task = Some(fixture.task.clone());
                                    this.relay_tasks = vec![Task {
                                        name: fixture.task,
                                        sha: fixture.spec,
                                    }];
                                }
                            }
                        }
                    }
                    Err(error) => this.lifecycle_error = Some(format!("恢复任务失败：{error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn repository_tasks(&self, cx: &Context<Self>) -> Div {
        let mut list = panel().child(
            h_flex()
                .justify_between()
                .gap_2()
                .child(heading("仓库任务记录"))
                .child(
                    button(
                        "task-sessions-refresh",
                        if self.task_sessions_loading {
                            "正在读取…"
                        } else {
                            "刷新任务记录"
                        },
                        false,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.load_task_sessions(cx))),
                ),
        );
        if let Some(error) = &self.task_sessions_error {
            list = list.child(t(error.clone(), RED));
        }
        if self.task_sessions.is_empty() && !self.task_sessions_loading {
            list = list.child(caption("这个仓库还没有 Studio 任务记录。"));
        }
        let mut rows = v_flex()
            .id("task-sessions-list")
            .test_support()
            .overflow_y_scroll()
            .h(px((self.task_sessions.len() as f32 * 90.).min(225.)))
            .gap_1();
        for entry in &self.task_sessions {
            let stage = entry.stage;
            let id = entry.path.file_name().unwrap_or_default().to_string_lossy();
            let path = entry.path.clone();
            let job = self.task_queue.latest(&entry.path);
            let queued = job.is_some_and(|job| job.state.active());
            let failure = job.and_then(|job| match &job.state {
                State::Failed(error) => Some(error),
                _ => None,
            });
            let detail = if job.is_some_and(|job| job.state == State::Waiting) {
                None
            } else {
                entry.detail.clone().or_else(|| failure.cloned())
            };
            let label = job
                .filter(|job| job.state.active() || failure.is_some())
                .map_or_else(|| stage.label().to_owned(), |job| self.job_label(job));
            let action = match stage {
                Stage::PrepareRetry => "重试准备任务",
                Stage::Retry(_) | Stage::PublishRetry | Stage::ChecksFailed => "查看失败并重试",
                Stage::Archived => "查看归档",
                Stage::Archive => "继续归档",
                _ => "继续任务",
            };
            let color = if queued {
                BLUE
            } else if failure.is_some() {
                RED
            } else {
                match stage {
                    Stage::Archived => MINT,
                    Stage::PrepareRetry
                    | Stage::Retry(_)
                    | Stage::PublishRetry
                    | Stage::ChecksFailed
                    | Stage::Unavailable => RED,
                    _ => AMBER,
                }
            };
            let row = v_flex()
                .gap_1()
                .py_2()
                .border_t_1()
                .border_color(rgb(LINE))
                .child(
                    h_flex()
                        .gap_2()
                        .justify_between()
                        .child(
                            v_flex()
                                .gap_1()
                                .child(t(entry.task.clone(), FG))
                                .child(caption(entry.branch.clone())),
                        )
                        .child(
                            detail_text(format!("task-session-stage-{id}"), &label)
                                .text_color(rgb(color))
                                .text_xs(),
                        ),
                )
                .when_some(detail, |row, detail| {
                    row.child(
                        detail_text(format!("task-session-detail-{id}"), &detail)
                            .text_color(rgb(color)),
                    )
                })
                .when(
                    !matches!(stage, Stage::Preparing | Stage::Unavailable),
                    |row| {
                        row.child(
                            h_flex().child(
                                button(
                                    format!("lifecycle-history-{id}"),
                                    action,
                                    matches!(stage, Stage::Retry(_) | Stage::PrepareRetry),
                                )
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.resume_task(path.clone(), stage, window, cx)
                                    },
                                )),
                            ),
                        )
                    },
                );
            rows = rows.child(row);
        }
        list.when(!self.task_sessions.is_empty(), |list| list.child(rows))
    }
}
