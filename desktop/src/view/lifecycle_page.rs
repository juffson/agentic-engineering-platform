use super::*;
use crate::lifecycle;
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

impl Studio {
    fn start_lifecycle(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.lifecycle_preparing || self.lifecycle.is_some() {
            return;
        }
        let Some(source) = self.workspace.clone() else {
            return;
        };
        let runner = self.runner.clone();
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
            let result = lifecycle::Fixture::prepare(&out, &root, &source.root, &runner, true);
            if let Err(error) = &result {
                fs::write(out.join("failure.txt"), format!("{error:#}\n"))?;
            }
            result
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.lifecycle_preparing = false;
                match result {
                    Ok((fixture, ws)) => {
                        this.set_workspace(ws, Page::Relay, window, cx);
                        this.relay_task = Some(fixture.task.clone());
                        this.relay_tasks = vec![crate::relay::Task {
                            name: fixture.task,
                            sha: fixture.spec,
                        }];
                    }
                    Err(error) => this.lifecycle_error = Some(format!("准备失败：{error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
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

    fn pack_lifecycle(&mut self, cx: &mut Context<Self>) {
        if self.lifecycle_packaging {
            return;
        }
        let Some(ws) = &self.workspace else { return };
        let Some(root) = lifecycle::root(ws).map(Path::to_path_buf) else {
            return;
        };
        self.lifecycle_packaging = true;
        let generation = self.workspace_generation;
        let job = cx
            .background_executor()
            .spawn(async move { lifecycle::archive(&root, &root.join("evidence")) });
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
        if let Some(test) = &self.lifecycle {
            let ws = self.workspace.as_ref().unwrap();
            let root = lifecycle::root(ws).unwrap();
            let out = root.join("evidence");
            let archive = root.join("evidence.tar.gz");
            let complete = self.verified();
            let relay_id = self.relays.first().map(|relay| relay.id.clone());
            body = body.child(
                panel()
                    .child(field("测试仓库", &test.repo))
                    .child(field("隔离目标", &test.base))
                    .child(detail_text(
                        "lifecycle-status",
                        if complete {
                            "全流程已核验"
                        } else {
                            "进行中"
                        },
                    ))
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                button("lifecycle-open-relay", "查看接力与日志", true).on_click(
                                    cx.listener(move |this, _, w, cx| {
                                        this.relay_id = relay_id.clone();
                                        if this.relay_id.is_none() {
                                            this.relay_task = this
                                                .lifecycle
                                                .as_ref()
                                                .map(|test| test.task.clone());
                                        }
                                        this.navigate(Page::Relay, w, cx);
                                    }),
                                ),
                            )
                            .child(button("lifecycle-return", "返回原仓库", false).on_click(
                                cx.listener(|this, _, w, cx| {
                                    if this.lifecycle_packaging {
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
            body = body.child(panel()
                .child(caption("使用当前仓库的 GitHub origin 准备独立克隆和合成 Rust 任务。运行会使用本机 Codex、Claude 和 GitHub 账号。"))
                .child(button("lifecycle-start", if self.lifecycle_preparing { "正在创建隔离测试…" } else { "启动隔离测试" }, true)
                    .on_click(cx.listener(|this, _, w, cx| this.start_lifecycle(w, cx)))));
            let mut sessions = fs::read_dir(ws.data.join("lifecycle"))
                .into_iter()
                .flatten()
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .collect::<Vec<_>>();
            sessions.sort();
            for session in sessions.into_iter().rev() {
                let path = session.join("workspace/repo");
                if let Some(test) = lifecycle::load_session(&session) {
                    body = body.child(
                        button(format!("lifecycle-history-{}", test.task), test.base, false)
                            .on_click(cx.listener(move |this, _, w, cx| {
                                if !this.lifecycle_preparing && !this.lifecycle_packaging {
                                    match Workspace::open(&path) {
                                        Ok(ws) => this.set_workspace(ws, Page::Lifecycle, w, cx),
                                        Err(error) => {
                                            this.lifecycle_error = Some(format!("{error:#}"));
                                            cx.notify();
                                        }
                                    }
                                }
                            })),
                    );
                } else if let Ok(log) = fs::read_to_string(session.join("evidence/failure.txt")) {
                    body = body.child(caption(log));
                }
            }
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
