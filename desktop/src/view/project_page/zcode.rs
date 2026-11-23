use super::*;
use std::collections::BTreeSet;

#[derive(Clone, Copy)]
enum Target {
    Project,
    Review,
    Round,
}

impl Target {
    fn prefix(self) -> &'static str {
        match self {
            Self::Project => "project-zcode",
            Self::Review => "review-zcode",
            Self::Round => "round-zcode",
        }
    }
}

impl Studio {
    pub(in crate::view) fn desktop_dispatch(
        &self,
        dir: &std::path::Path,
        role: &str,
        cx: &Context<Self>,
    ) -> Option<Div> {
        let job = aep::workspace::read(&dir.join("desktop-request.json")).ok()?;
        if let Ok(task) = aep::workspace::read(&dir.join("desktop-task.json")) {
            let session = task["session"].as_str()?;
            let workspace = task["workspace"].as_str()?;
            let path = dir.join("desktop-approvals.json");
            let observed = aep::workspace::read(&path)
                .ok()
                .filter(|v| v["session"] == session && v["workspace"] == workspace);
            let fresh = path
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age < Duration::from_secs(15));
            let state = observed.as_ref().map(|v| &v["state"]);
            let mut panel = v_flex().gap_1();
            if fresh && state.is_some_and(|s| s["waiting"] == true) {
                panel = panel
                    .child(
                        detail_text(
                            SharedString::from(format!("zcode-wait-{session}")),
                            &format!("{role} · 等待工具审批"),
                        )
                        .text_color(rgb(AMBER)),
                    )
                    .child(field("工作区", workspace));
                for tool in state
                    .and_then(|s| s["tools"].as_array())
                    .into_iter()
                    .flatten()
                {
                    let name = tool["tool"].as_str().unwrap_or("工具");
                    let detail = tool["detail"].as_str().unwrap_or("");
                    panel = panel.child(detail_text(
                        SharedString::from(format!(
                            "zcode-tool-{session}-{}",
                            tool["id"].as_str().unwrap_or("")
                        )),
                        &format!("{name}  {detail}"),
                    ));
                }
            } else if let Some(error) = state.and_then(|s| s["error"].as_str()) {
                panel = panel.child(caption(format!("审批状态不可用 · {error}")));
            } else if observed.is_some() && !fresh {
                panel = panel.child(caption("正在重新连接 ZCode 审批状态"));
            } else {
                panel = panel.child(caption(format!("ZCode 桌面任务 {session} · 等待交接")));
            }
            if dir.join("desktop-create.json").is_file() {
                let folder = dir.to_owned();
                panel = panel.child(
                    button(
                        format!("zcode-open-{session}"),
                        "打开对应 ZCode 任务",
                        false,
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.open_zcode_task(folder.clone(), cx)),
                    ),
                );
            }
            if let Some((path, msg)) = &self.zcode_navigation
                && path == dir
            {
                panel = panel.child(caption(msg.clone()));
            }
            return Some(panel);
        }
        let model = &job["model"];
        let level = model
            .pointer("/options/reasoningLevel")
            .and_then(Value::as_str)
            .unwrap_or("默认");
        let provider = model["providerId"].as_str()?;
        let plan = aep::zcode_catalog::desktop_plan(provider).unwrap_or(provider);
        Some(caption(format!(
            "正在 ZCode 桌面创建{role}任务 · {plan} / {} / {level}",
            model["modelId"].as_str()?
        )))
    }

    pub(super) fn open_zcode_task(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        let program = self.runner.binary();
        self.zcode_navigation = Some((dir.clone(), "正在打开…".into()));
        let folder = dir.clone();
        let job = cx.background_executor().spawn(async move {
            let output = std::process::Command::new(program?)
                .arg("zcode-open")
                .arg("--dir")
                .arg(folder)
                .output()?;
            anyhow::ensure!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            anyhow::Ok(())
        });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |this, cx| {
                if this
                    .zcode_navigation
                    .as_ref()
                    .is_some_and(|(path, _)| path == &dir)
                {
                    this.zcode_navigation = Some((
                        dir,
                        result.map_or_else(
                            |e| format!("打开失败 · {e:#}"),
                            |_| "已打开 ZCode 任务".into(),
                        ),
                    ));
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn relay_project_default(&self) -> String {
        let Some(project) = self.board.library.project() else {
            return String::new();
        };
        let Some(ws) = self.workspace.as_ref() else {
            return String::new();
        };
        let source = self
            .lifecycle
            .as_ref()
            .and_then(|f| Workspace::open(&f.source).ok());
        let ws = source.as_ref().unwrap_or(ws);
        if !project.includes_workspace(ws) {
            use aep::project::directory::Binding;
            let bound =
                Binding::file(&ws.root).and_then(|file| Binding::read(&file).ok().flatten());
            if !bound.is_some_and(|b| {
                b.project == project.id && self.board.dir.as_ref() == Some(&b.home)
            }) {
                return String::new();
            }
        }
        // Another Studio or CLI may have saved a new default while round one was running.
        if let Some(dir) = self.board.dir.as_deref()
            && let Ok(latest) = Library::open(dir)
        {
            return latest
                .projects
                .iter()
                .find(|p| p.id == project.id)
                .map_or_else(String::new, |p| p.zcode_default().to_owned());
        }
        project.zcode_default().to_owned()
    }

    pub(crate) fn ensure_zcode_catalog(&mut self, cx: &mut Context<Self>) {
        if self.board.zcode_catalog.is_none() {
            self.load_zcode_catalog(cx);
        }
    }

    fn load_zcode_catalog(&mut self, cx: &mut Context<Self>) {
        if self.board.zcode_loading {
            return;
        }
        self.board.zcode_loading = true;
        let path = self.runner.path();
        let job = cx.background_executor().spawn(async move {
            path.and_then(|path| aep::zcode_catalog::discover(&path))
                .map_err(|e| format!("{e:#}"))
        });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |s, cx| {
                s.board.zcode_catalog = Some(result);
                s.board.zcode_loading = false;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn choose_zcode(
        &mut self,
        target: Target,
        value: String,
        w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let value = aep::zcode_catalog::effective_selection(&value).to_owned();
        match target {
            Target::Review => self
                .relay_reviewer_model
                .update(cx, |s, cx| s.set_value(value, w, cx)),
            Target::Round => self
                .relay_zcode_model
                .update(cx, |s, cx| s.set_value(value, w, cx)),
            Target::Project => {
                if let Some(p) = self.board.library.project() {
                    self.board.zcode_draft = Some((p.id.clone(), value));
                }
            }
        }
        cx.notify();
    }

    fn zcode_choice(
        &self,
        target: Target,
        key: String,
        label: String,
        value: String,
        selected: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        button(format!("{}-{key}", target.prefix()), label, selected)
            .on_click(cx.listener(move |s, _, w, cx| s.choose_zcode(target, value.clone(), w, cx)))
    }

    fn zcode_picker(&self, target: Target, value: &str, cx: &Context<Self>) -> Div {
        let mut panel = v_flex().gap_2();
        let refresh = button(
            format!("{}-refresh", target.prefix()),
            if self.board.zcode_loading {
                "正在读取…"
            } else {
                "刷新本机模型"
            },
            false,
        )
        .on_click(cx.listener(|s, _, _, cx| s.load_zcode_catalog(cx)));
        panel = panel.child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .child(caption(if value.is_empty() {
                    "ZCode 客户端默认"
                } else {
                    value
                }))
                .child(refresh),
        );
        let models = match self.board.zcode_catalog.as_ref() {
            Some(Ok(rows)) => rows,
            Some(Err(e)) => return panel.child(caption(e.clone()).text_color(rgb(AMBER))),
            None => return panel,
        };
        let (provider, rest) = value.split_once('/').unwrap_or(("", ""));
        let (name, level) = rest
            .rsplit_once('@')
            .map_or((rest, None), |(m, l)| (m, Some(l)));
        let mut providers = BTreeSet::new();
        let mut row = h_flex().flex_wrap().gap_2().child(caption("Provider"));
        for (i, m) in models.iter().enumerate() {
            if providers.insert(&m.id.provider_id) {
                row = row.child(
                    self.zcode_choice(
                        target,
                        format!("provider-{i}"),
                        m.provider_label
                            .as_ref()
                            .unwrap_or(&m.id.provider_id)
                            .clone(),
                        m.default_selection(),
                        provider == m.id.provider_id,
                        cx,
                    ),
                );
            }
        }
        panel = panel.child(row);
        let current: Vec<_> = models
            .iter()
            .enumerate()
            .filter(|(_, m)| m.id.provider_id == provider)
            .collect();
        if !current.is_empty() {
            let mut row = h_flex().flex_wrap().gap_2().child(caption("模型"));
            for (i, m) in &current {
                row = row.child(self.zcode_choice(
                    target,
                    format!("model-{i}"),
                    m.label.clone(),
                    m.default_selection(),
                    m.id.model_id == name,
                    cx,
                ));
            }
            panel = panel.child(row);
            if aep::zcode_catalog::desktop_plan(provider).is_some() {
                panel = panel.child(caption("推理档位使用模型默认值，完成后显示实际档位。"));
            } else if let Some((_, m)) = current.into_iter().find(|(_, m)| m.id.model_id == name)
                && !m.reasoning.levels.is_empty()
            {
                let mut row = h_flex().flex_wrap().gap_2().child(caption("推理档位"));
                for (i, l) in m.reasoning.levels.iter().enumerate() {
                    row = row.child(self.zcode_choice(
                        target,
                        format!("level-{i}"),
                        l.label.clone(),
                        m.selection(Some(&l.value)),
                        level == Some(l.value.as_str()),
                        cx,
                    ));
                }
                panel = panel.child(row);
            }
        }
        if !value.is_empty() && !models.iter().any(|m| m.accepts(value)) {
            panel = panel
                .child(caption("当前配置未出现在本机目录中，可重新选择。").text_color(rgb(AMBER)));
        }
        panel
    }

    pub(in crate::view) fn reviewer_zcode_picker(&self, cx: &Context<Self>) -> Div {
        self.zcode_picker(
            Target::Review,
            &self.relay_reviewer_model.read(cx).value(),
            cx,
        )
    }
    pub(in crate::view) fn relay_zcode_picker(&self, cx: &Context<Self>) -> Div {
        let value = self.relay_zcode_model.read(cx).value();
        self.zcode_picker(Target::Round, &value, cx)
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(caption("仅用于本轮"))
                    .child(
                        button("round-zcode-native", "使用客户端默认", false).on_click(
                            cx.listener(|s, _, w, cx| {
                                s.choose_zcode(Target::Round, String::new(), w, cx)
                            }),
                        ),
                    )
                    .child(
                        button("round-zcode-inherit", "恢复项目默认", false).on_click(cx.listener(
                            move |s, _, w, cx| {
                                s.choose_zcode(Target::Round, s.relay_project_default(), w, cx)
                            },
                        )),
                    ),
            )
            .child(
                Input::new(&self.relay_zcode_model)
                    .id("relay-zcode-model")
                    .aria_label("ZCode 模型"),
            )
    }

    pub(super) fn project_zcode_settings(&self, p: &Project, cx: &Context<Self>) -> Div {
        let saved = aep::zcode_catalog::effective_selection(p.zcode_default());
        let value = self
            .board
            .zcode_draft
            .as_ref()
            .filter(|(id, _)| id == &p.id)
            .map_or(saved, |(_, s)| s.as_str());
        let settings = panel()
            .flex_none()
            .child(heading("ZCode 项目默认模型"))
            .child(self.zcode_picker(Target::Project, value, cx));
        let mut row = h_flex().gap_2().child(
            button("project-zcode-clear", "使用客户端默认", false).on_click(
                cx.listener(|s, _, w, cx| s.choose_zcode(Target::Project, String::new(), w, cx)),
            ),
        );
        if saved != value
            && (value.is_empty()
                || self
                    .board
                    .zcode_catalog
                    .as_ref()
                    .and_then(|c| c.as_ref().ok())
                    .is_some_and(|rows| rows.iter().any(|m| m.accepts(value))))
        {
            let model = value.to_owned();
            let project = p.id.clone();
            row = row.child(
                primary("project-zcode-save", "保存项目默认").on_click(cx.listener(
                    move |s, _, _, cx| {
                        if s.change_board(cx, |lib| {
                            let p = lib.project_mut()?;
                            ensure!(p.id == project, "项目已切换");
                            p.set_zcode_default(model.clone());
                            Ok(())
                        }) {
                            s.board.zcode_draft = None;
                        }
                    },
                )),
            );
        }
        settings.child(row).child(caption(format!(
            "已保存 · {}",
            if saved.is_empty() {
                "ZCode 客户端默认"
            } else {
                saved
            }
        )))
    }
}
