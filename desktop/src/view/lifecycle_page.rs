use super::*;
use crate::lifecycle;
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::Path,
};

impl Studio {
    pub(crate) fn load_lifecycle_choices(&mut self, cx: &mut Context<Self>) {
        if self.lifecycle.is_some() || self.lifecycle_loading || self.lifecycle_preparing {
            return;
        }
        let Some(ws) = self.workspace.clone() else {
            return;
        };
        let runner = self.runner.clone();
        let generation = self.workspace_generation;
        self.lifecycle_loading = true;
        self.lifecycle_error = None;
        self.lifecycle_choices = None;
        let job = cx
            .background_executor()
            .spawn(async move { lifecycle::choices(&ws, &runner) });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |this, cx| {
                if generation != this.workspace_generation {
                    return;
                }
                this.lifecycle_loading = false;
                match result {
                    Ok(choices) => this.lifecycle_choices = Some(choices),
                    Err(error) => {
                        this.lifecycle_error =
                            Some(format!("读取 GitHub 任务与分支失败：{error:#}"))
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn start_lifecycle(
        &mut self,
        plan: lifecycle::Plan,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.lifecycle_preparing || self.lifecycle.is_some() {
            return;
        }
        let Some(source) = self.workspace.clone() else {
            return;
        };
        let runner = self.runner.clone();
        let generation = self.workspace_generation;
        self.lifecycle_preparing = true;
        self.lifecycle_error = None;
        let job = cx.background_executor().spawn(async move {
            let sessions = source.data.join("lifecycle");
            fs::create_dir_all(&sessions)?;
            let root = tempfile::Builder::new()
                .prefix("studio-")
                .tempdir_in(sessions)?
                .keep();
            let out = root.join("evidence");
            fs::create_dir(&out)?;
            let lock = File::create(root.join("prepare.lock"))?;
            lock.try_lock()?;
            lifecycle::save(&root, "request.json", &plan)?;
            let result =
                lifecycle::Fixture::prepare_studio(&out, &root, &source.root, &runner, plan);
            if let Err(error) = &result {
                fs::write(out.join("failure.txt"), format!("{error:#}\n"))?;
            }
            result
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if generation != this.workspace_generation {
                    return;
                }
                this.lifecycle_preparing = false;
                match result {
                    Ok((fixture, ws)) => {
                        if fixture.authored {
                            this.set_workspace(ws, Page::Lifecycle, window, cx);
                        } else {
                            this.set_workspace(ws, Page::Relay, window, cx);
                            this.relay_task = Some(fixture.task.clone());
                            this.relay_tasks = vec![relay::Task {
                                name: fixture.task,
                                sha: fixture.spec,
                            }];
                        }
                    }
                    Err(error) => this.lifecycle_error = Some(format!("准备失败：{error:#}")),
                }
                this.load_task_sessions(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn dispatch_and_start(&mut self, cx: &mut Context<Self>) {
        if self.lifecycle_preparing || self.relay_launching || !self.relays.is_empty() {
            return;
        }
        if let Some(fixture) = self.lifecycle.as_ref().filter(|fixture| fixture.authored) {
            self.enqueue_round(
                fixture.initial_relay_id(),
                crate::task_queue::Action::Start,
                cx,
            );
        }
    }

    pub(super) fn checkpoint(
        &mut self,
        name: &'static str,
        w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.lifecycle.is_none() {
            return;
        }
        let Some(out) = self.workspace.as_ref().and_then(lifecycle::output) else {
            return;
        };
        let path = out.join(format!("{name}.png"));
        if path.exists() || !self.lifecycle_shots.insert(name) {
            return;
        }
        let entity = cx.weak_entity();
        let generation = self.workspace_generation;
        w.on_next_frame(move |w, app| {
            let _ = entity.update(app, |this, cx| {
                if this.workspace_generation != generation {
                    return;
                }
                if let Err(e) = w.render_to_image().and_then(|image| Ok(image.save(&path)?)) {
                    this.lifecycle_error = Some(format!("保存 Studio 画面失败：{e:#}"));
                }
                if name == "14-completed" {
                    this.pack_lifecycle(cx);
                }
            });
        });
    }

    pub(super) fn lifecycle_frame(&mut self, w: &mut Window, cx: &mut Context<Self>) {
        if self.lifecycle.is_none() {
            return;
        }
        let picked = self
            .relays
            .iter()
            .find(|relay| Some(&relay.id) == self.relay_id.as_ref());
        let active =
            picked.is_some_and(|relay| relay.active(&self.snapshot)) || self.relay_launching;
        let first_passed = picked
            .is_some_and(|relay| relay::passed(relay.run(relay::Round::First, &self.snapshot)));
        let decision = picked.is_some_and(|relay| !relay.decisions(&self.snapshot).is_empty());
        let second_passed = picked
            .is_some_and(|relay| relay::passed(relay.run(relay::Round::Second, &self.snapshot)));
        let step = if self.editor.is_some() {
            Some("03-revision")
        } else {
            match self.page {
                Page::Relay if self.relay_task.is_some() => Some("01-task"),
                Page::Relay if !active && second_passed => Some("05-round-two"),
                Page::Relay if !active && decision => Some("04-decision"),
                Page::Relay if !active && first_passed => Some("02-round-one"),
                Page::Review => self
                    .review
                    .as_ref()
                    .and_then(human_review::State::checkpoint),
                Page::PullRequest => self
                    .pull_request
                    .as_ref()
                    .and_then(github_page::State::checkpoint),
                Page::Lifecycle if self.verified() => Some("14-completed"),
                Page::Lifecycle
                    if self.lifecycle.as_ref().is_some_and(|run| run.authored)
                        && self.relays.is_empty()
                        && !self.lifecycle_preparing =>
                {
                    Some("00-task-prepared")
                }
                _ => None,
            }
        };
        if let Some(step) = step {
            self.checkpoint(step, w, cx);
        }
    }

    fn verified(&self) -> bool {
        self.workspace
            .as_ref()
            .and_then(lifecycle::output)
            .is_some_and(|p| p.join("verification.json").is_file())
    }

    pub(super) fn pack_lifecycle(&mut self, cx: &mut Context<Self>) {
        if self.lifecycle_packaging {
            return;
        }
        let Some(ws) = &self.workspace else { return };
        let Some(root) = lifecycle::root(ws).map(Path::to_path_buf) else {
            return;
        };
        self.lifecycle_packaging = true;
        let generation = self.workspace_generation;
        let ws = ws.clone();
        let job = cx.background_executor().spawn(async move {
            let result = lifecycle::archive(&root, &root.join("evidence"));
            lifecycle::record_failure(&ws, "archive-error.txt", result)
        });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |this, cx| {
                if this.workspace_generation == generation {
                    this.lifecycle_packaging = false;
                    if let Err(error) = result {
                        this.lifecycle_error = Some(format!("归档失败：{error:#}"));
                    }
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn lifecycle_page(&self, cx: &Context<Self>) -> Div {
        let mut body = v_flex()
            .flex_1()
            .h_full()
            .min_w_0()
            .gap_3()
            .p_4()
            .id("lifecycle-results")
            .test_support()
            .overflow_y_scroll()
            .child(heading("隔离全流程").text_2xl());
        if let Some(error) = &self.lifecycle_error {
            body = body.child(detail_text("lifecycle-error", error).text_color(rgb(RED)));
        }
        if let Some(error) = &self.queue_error {
            body = body.child(detail_text("task-queue-error", error).text_color(rgb(RED)));
        }
        if let Some(error) = &self.repo_error {
            body = body.child(detail_text("lifecycle-repo-error", error).text_color(rgb(RED)));
        }
        if let (Some(authoring), Some(ws)) = (&self.task_authoring, &self.workspace) {
            body = body.child(authoring.render(ws, self.lifecycle_preparing, cx));
        } else if let Some(test) = &self.lifecycle {
            let ws = self.workspace.as_ref().unwrap();
            let root = lifecycle::root(ws).unwrap();
            let out = root.join("evidence");
            let archive = root.join("evidence.tar.gz");
            let complete = self.verified();
            let relay_id = self.relays.first().map(|relay| relay.id.clone());
            let pending = test.authored && self.relays.is_empty() && !self.relay_launching;
            let dispatched = out.join("dispatch.json").is_file();
            let job = self.current_queue_job();
            let active = job.is_some_and(|job| job.state.active());
            let cancelled = job.is_some_and(|job| job.state == crate::task_queue::State::Cancelled);
            let label = if complete {
                "全流程已核验".to_owned()
            } else if let Some(job) = job.filter(|job| job.state.active() || cancelled) {
                self.job_label(job)
            } else if pending && dispatched {
                "已派发，等待启动第一轮".to_owned()
            } else if pending {
                "已准备 Spec，待派发".to_owned()
            } else {
                "进行中".to_owned()
            };
            body = body.child(
                panel()
                    .child(field("仓库", &test.repo))
                    .child(field("任务", &test.task))
                    .child(field("目标分支", &test.base))
                    .when(pending, |card| {
                        card.child(field("目标提交", &test.base_sha))
                            .child(field("Spec SHA", &test.spec))
                    })
                    .child(detail_text("lifecycle-status", &label))
                    .when_some(job.filter(|job| job.state.active()), |card, job| {
                        card.child(self.queue_controls(job, cx))
                    })
                    .when_some(
                        job.and_then(|job| match &job.state {
                            crate::task_queue::State::Failed(error) => Some(error),
                            _ => None,
                        }),
                        |card, error| {
                            card.child(
                                detail_text("lifecycle-queue-failed", error).text_color(rgb(RED)),
                            )
                        },
                    )
                    .when(pending && !active && !self.lifecycle_preparing, |card| {
                        card.child(self.relay_model_input()).child(
                            button("dispatch-task-and-start", "派发并加入队列", true).on_click(
                                cx.listener(|this, _, _, cx| this.dispatch_and_start(cx)),
                            ),
                        )
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .when(!pending, |row| {
                                row.child(
                                    button("lifecycle-open-relay", "查看接力与日志", true)
                                        .on_click(cx.listener(move |this, _, w, cx| {
                                            this.relay_id = relay_id.clone();
                                            if this.relay_id.is_none() {
                                                this.relay_task = this
                                                    .lifecycle
                                                    .as_ref()
                                                    .map(|test| test.task.clone());
                                            }
                                            this.navigate(Page::Relay, w, cx);
                                        })),
                                )
                            })
                            .child(button("lifecycle-return", "返回原仓库", false).on_click(
                                cx.listener(|this, _, w, cx| {
                                    if this.lifecycle_packaging || this.lifecycle_preparing {
                                        return;
                                    }
                                    if let Some(test) = &this.lifecycle {
                                        match Workspace::open(&test.source) {
                                            Ok(ws) => {
                                                this.set_workspace(ws, Page::Lifecycle, w, cx)
                                            }
                                            Err(e) => {
                                                this.lifecycle_error = Some(format!("{e:#}"));
                                                cx.notify();
                                            }
                                        }
                                    }
                                }),
                            ))
                            .when(archive.is_file() && !self.lifecycle_packaging, |row| {
                                row.child(
                                    button("lifecycle-archive", "在 Finder 中显示归档", false)
                                        .on_click(
                                            cx.listener(move |_, _, _, cx| {
                                                cx.reveal_path(&archive)
                                            }),
                                        ),
                                )
                            })
                            .when(complete && !self.lifecycle_packaging, |row| {
                                row.child(button("lifecycle-repack", "更新归档", false).on_click(
                                    cx.listener(|this, _, _, cx| this.pack_lifecycle(cx)),
                                ))
                            })
                            .when(cancelled && !self.lifecycle_packaging, |row| {
                                row.child(
                                    button("lifecycle-archive-cancelled", "归档取消证据", true)
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.pack_lifecycle(cx)),
                                        ),
                                )
                            }),
                    ),
            );
            if self.lifecycle_packaging {
                body = body.child(caption("正在归档证据…"));
            }
            let mut files = Vec::new();
            list_evidence(&out, &mut files);
            files.sort();
            let list = v_flex()
                .id("lifecycle-files")
                .test_support()
                .overflow_y_scroll()
                .h(px(420.))
                .w(px(245.))
                .flex_none()
                .gap_1()
                .children(files.into_iter().map(|path| {
                    let relative = path
                        .strip_prefix(&out)
                        .unwrap_or(&path)
                        .display()
                        .to_string();
                    let id = format!("lifecycle-file-{relative}");
                    let label = evidence_label(&relative);
                    button(id, label, self.lifecycle_doc.as_ref() == Some(&path)).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.lifecycle_doc = Some(path.clone());
                            cx.notify();
                        }),
                    )
                }));
            let mut document = v_flex()
                .id("lifecycle-document")
                .overflow_y_scroll()
                .h(px(420.))
                .flex_1()
                .min_w_0();
            if let Some(path) = &self.lifecycle_doc {
                if path.extension().is_some_and(|ext| ext == "png") {
                    document = document.child(
                        img(path.clone())
                            .w_full()
                            .h(px(420.))
                            .object_fit(ObjectFit::Contain),
                    );
                } else {
                    let text = fs::File::open(path)
                        .and_then(|mut file| {
                            const LIMIT: u64 = 96 * 1024;
                            let len = file.metadata()?.len();
                            file.seek(SeekFrom::Start(len.saturating_sub(LIMIT)))?;
                            let mut data = Vec::new();
                            file.take(LIMIT).read_to_end(&mut data)?;
                            let prefix = if len > LIMIT {
                                "仅显示末尾 96 KiB\n\n"
                            } else {
                                ""
                            };
                            Ok(format!("{prefix}{}", String::from_utf8_lossy(&data)))
                        })
                        .unwrap_or_else(|e| format!("读取失败：{e}"));
                    document = document.child(
                        panel().child(
                            detail_text("lifecycle-preview", &text)
                                .font_family("Menlo")
                                .text_xs(),
                        ),
                    );
                }
            } else {
                document = document.child(caption("选择一个文件来预览日志、截图或核验结果。"));
            }
            body = body.child(h_flex().items_start().gap_3().child(list).child(document));
        } else if let Some(ws) = &self.workspace {
            body = body
                .child(self.queue_settings(cx))
                .child(self.repository_tasks(cx));
            body = body.child(self.lifecycle_setup(ws, cx));
        }
        h_flex()
            .size_full()
            .bg(rgb(CANVAS))
            .child(self.rail(cx))
            .child(body)
    }
}

fn list_evidence(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            list_evidence(&path, files);
        } else if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| matches!(ext, "png" | "json" | "log" | "txt" | "diff" | "md"))
        {
            files.push(path);
        }
    }
}

fn evidence_label(relative: &str) -> String {
    let Some((run, file)) = relative
        .strip_prefix("runs/")
        .and_then(|s| s.split_once('/'))
    else {
        return relative.to_owned();
    };
    let label = if run.contains("-r1-") {
        "第一轮"
    } else {
        "第二轮"
    };
    let attempt = run.rsplit('-').next().unwrap_or_default();
    format!("{label} #{attempt} · {file}")
}

impl Studio {
    fn lifecycle_setup(&self, ws: &Workspace, cx: &Context<Self>) -> Div {
        let mut form = panel()
            .child(heading("真实任务"))
            .child(caption(ws.root.display().to_string()))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        button("lifecycle-choose-repo", "选择仓库…", false)
                            .on_click(cx.listener(|this, _, w, cx| this.choose_repo(w, cx))),
                    )
                    .child(
                        button(
                            "lifecycle-load-choices",
                            if self.lifecycle_loading {
                                "正在读取…"
                            } else {
                                "刷新任务与分支"
                            },
                            false,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.load_lifecycle_choices(cx))),
                    ),
            );
        if let Some(choices) = &self.lifecycle_choices {
            let tasks = v_flex()
                .id("lifecycle-task-choices")
                .test_support()
                .overflow_y_scroll()
                .h(px(145.))
                .gap_1()
                .children(choices.tasks.iter().map(|task| {
                    let current = choices
                        .task
                        .as_ref()
                        .is_some_and(|chosen| chosen.name == task.name);
                    let task = task.clone();
                    button(
                        format!("lifecycle-task-{}", task.name),
                        task.name.clone(),
                        current,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(choices) = this.lifecycle_choices.as_mut() {
                            choices.task = Some(task.clone());
                        }
                        cx.notify();
                    }))
                }));
            let branches = v_flex()
                .id("lifecycle-branch-choices")
                .test_support()
                .overflow_y_scroll()
                .h(px(145.))
                .gap_1()
                .children(choices.branches.iter().map(|branch| {
                    let current = choices
                        .branch
                        .as_ref()
                        .is_some_and(|chosen| chosen.name == branch.name);
                    let branch = branch.clone();
                    button(
                        format!("lifecycle-branch-{}", branch.name),
                        branch.name.clone(),
                        current,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(choices) = this.lifecycle_choices.as_mut() {
                            choices.branch = Some(branch.clone());
                        }
                        cx.notify();
                    }))
                }));
            form = form.child(
                h_flex()
                    .gap_4()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(caption("已派发任务"))
                            .child(tasks),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(caption("GitHub 目标分支"))
                            .child(branches),
                    ),
            );
            if let (Some(task), Some(branch)) = (&choices.task, &choices.branch) {
                let selected = (task.clone(), branch.clone());
                form = form
                    .child(detail_text(
                        "lifecycle-selection",
                        &format!("{} → {}", task.name, branch.name),
                    ))
                    .when(!self.lifecycle_preparing, |form| {
                        form.child(
                            button("lifecycle-start-task", "启动真实任务", true).on_click(
                                cx.listener(move |this, _, w, cx| {
                                    this.start_lifecycle(
                                        lifecycle::Plan::Existing(
                                            selected.0.clone(),
                                            selected.1.clone(),
                                        ),
                                        w,
                                        cx,
                                    )
                                }),
                            ),
                        )
                    });
            }
            if choices.tasks.is_empty() {
                form = form.child(caption("仓库暂无已派发任务。"));
            }
            if let Some(branch) = &choices.branch
                && !self.lifecycle_preparing
            {
                let branch = branch.clone();
                form = form.child(
                    button("start-task-authoring", "创建并派发任务…", true).on_click(cx.listener(
                        move |this, _, w, cx| this.open_task_authoring(branch.clone(), w, cx),
                    )),
                );
            }
        }
        if self.lifecycle_preparing {
            return form.child(caption("正在准备隔离工作区…"));
        }
        form.child(button("lifecycle-start", "启动合成验证", false).on_click(
            cx.listener(|this, _, w, cx| this.start_lifecycle(lifecycle::Plan::Synthetic, w, cx)),
        ))
    }
}
