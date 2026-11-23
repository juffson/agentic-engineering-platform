use super::*;
use aep::handoff::busy;
use std::{
    fs,
    os::unix::process::CommandExt,
    process::{Command, Stdio},
};

fn phase(v: &Value) -> &str {
    match v["status"].as_str() {
        Some("succeeded") => "已完成",
        Some("initializing") => "启动中",
        Some("running") => "运行中",
        Some("claimed") => "独立会话进行中",
        Some("blocked") => "阻塞",
        Some("cancelled") => "已取消",
        Some("failed") => "失败",
        Some("needs_reconciliation" | "unstarted") => "已中断",
        _ => "记录读取失败",
    }
}
impl Studio {
    fn project_memory_dir(&self) -> Result<PathBuf> {
        Ok(self
            .board
            .dir
            .as_ref()
            .context("项目库不可用")?
            .join("workflows")
            .join(&self.board.library.project().context("请选择项目")?.id))
    }
    pub(super) fn refresh_project_memory(&mut self, cx: &mut Context<Self>) {
        match self
            .project_memory_dir()
            .and_then(|dir| aep::memory::Memory::browse(&dir))
        {
            Ok(rows) => self.board.memories = rows,
            Err(e) => {
                self.board.memories.clear();
                self.board.error = Some(e.to_string());
            }
        }
        cx.notify();
    }
    pub(super) fn memory_board(&self, cx: &Context<Self>) -> Div {
        let mut page = panel()
            .flex_none()
            .child(heading(if self.board.editing_memory.is_some() {
                "修订为项目决策"
            } else {
                "记录项目决策"
            }))
            .child(
                div().id("project-memory-text").test_support().child(
                    Textarea::new(&self.board.memory_note)
                        .h(px(70.))
                        .aria_label("项目决策"),
                ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        primary("project-memory-save", "保存决策").on_click(cx.listener(
                            |this, _, w, cx| {
                                let result = (|| -> Result<()> {
                                    let mut memory =
                                        aep::memory::Memory::open(&this.project_memory_dir()?)?;
                                    let text = this.board.memory_note.read(cx).value().to_string();
                                    match this.board.editing_memory {
                                        Some(id) => {
                                            memory.revise(id, aep::memory::Kind::Decision, &text)?
                                        }
                                        None => memory.add(
                                            &aep::memory::Key::new("", "", ""),
                                            "decision",
                                            &text,
                                            None,
                                        )?,
                                    };
                                    Ok(())
                                })();
                                match result {
                                    Ok(()) => this.section(Section::Memory, w, cx),
                                    Err(e) => {
                                        this.board.error = Some(e.to_string());
                                        cx.notify();
                                    }
                                }
                            },
                        )),
                    )
                    .child(
                        button("project-memory-refresh", "刷新 / 新建", false).on_click(
                            cx.listener(|this, _, w, cx| this.section(Section::Memory, w, cx)),
                        ),
                    ),
            );
        for note in &self.board.memories {
            let id = note["id"].as_i64().unwrap_or_default();
            let body = note["body"].as_str().unwrap_or_default().to_owned();
            let mut row = h_flex().gap_2().child(caption(format!(
                "#{id} · {} · {}",
                kind(string(note, "kind")),
                author(string(note, "author"))
            )));
            if note["successor"].is_null() {
                row = row.child(
                    button(format!("project-memory-revise-{id}"), "修订", false).on_click(
                        cx.listener(move |this, _, w, cx| {
                            this.board.editing_memory = Some(id);
                            this.board
                                .memory_note
                                .update(cx, |s, cx| s.set_value(body.clone(), w, cx));
                            cx.notify();
                        }),
                    ),
                );
            } else {
                row = row.child(caption(format!("已由 #{} 修订", note["successor"])));
            }
            page = page
                .child(row)
                .child(t(string(note, "body").to_owned(), FG));
        }
        page
    }
    pub(super) fn fill_profile(
        &mut self,
        profile: Profile,
        w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.board.client = profile.client;
        self.board
            .check_command
            .update(cx, |s, cx| s.set_value(profile.check_command, w, cx));
        self.board
            .executable
            .update(cx, |s, cx| s.set_value(profile.executable, w, cx));
        self.board
            .model
            .update(cx, |s, cx| s.set_value(profile.model, w, cx));
    }
    fn chosen_profile(&self, cx: &Context<Self>) -> Profile {
        Profile {
            client: self.board.client,
            check_command: if self.board.client == Client::Claude {
                self.board.check_command.read(cx).value().trim().into()
            } else {
                String::new()
            },
            executable: self.board.executable.read(cx).value().trim().into(),
            model: self.board.model.read(cx).value().trim().into(),
        }
    }
    fn remember_profile(p: &mut Project, index: usize, profile: &Profile) -> Result<()> {
        p.cli_defaults
            .retain(|other| other.client != profile.client);
        let mut preset = profile.clone();
        preset.check_command.clear();
        p.cli_defaults.push(preset);
        p.selected_mut()?.profiles.insert(index, profile.clone());
        Ok(())
    }
    fn attempt_path(&self, id: &str) -> Option<PathBuf> {
        let p = self.board.library.project()?;
        Some(workflow::attempt_dir(
            self.board.dir.as_ref()?,
            &p.id,
            &p.selected()?.id,
            id,
        ))
    }
    pub(super) fn has_running_agent(&self) -> bool {
        self.board
            .library
            .project()
            .and_then(Project::selected)
            .and_then(|work| work.attempts.last())
            .and_then(|a| self.attempt_path(&a.id))
            .and_then(|d| workflow::evidence(&d).ok())
            .is_some_and(|v| busy(&v))
    }
    pub(super) fn accept_catalog(
        &mut self,
        latest: Library,
        w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let draft = self
            .board
            .library
            .project()
            .and_then(Project::selected)
            .is_some_and(|item| {
                let s = self.board.viewing;
                item.notes.get(s).is_some_and(|n| {
                    n != self.board.note.read(cx).value().as_str()
                        || item.profile(s) != self.chosen_profile(cx)
                })
            });
        let selected = latest.project().and_then(Project::selected).map(|i| &i.id);
        let switched = selected
            != self
                .board
                .library
                .project()
                .and_then(Project::selected)
                .map(|i| &i.id);
        let follow = switched
            || self
                .board
                .library
                .project()
                .and_then(Project::selected)
                .is_none_or(|item| {
                    self.board.viewing == item.stage.min(item.notes.len().saturating_sub(1))
                });
        self.board.library = latest;
        if self.board.timeline.is_some() {
            if switched {
                self.board.timeline = None;
            } else if self.board.section == Section::Work {
                self.load_timeline(cx);
            }
        }
        if self.board.section == Section::Work && (!draft || switched) {
            self.board.note_key = None;
            if follow {
                self.load_stage(w, cx);
            } else {
                self.show_stage(self.board.viewing, w, cx);
            }
        }
        self.watch_agents(cx);
        cx.notify();
    }
    pub(in crate::view) fn watch_project_catalog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.board.catalog_polling {
            return;
        }
        self.board.catalog_polling = true;
        let mut watched_dir = None;
        let stamp = |p: &Path| std::fs::metadata(p).ok().and_then(|m| m.modified().ok());
        let mut last = None;
        let mut last_memory = None;
        let executor = cx.background_executor().clone();
        cx.spawn_in(window, async move |this, cx| {
            loop {
                executor.timer(Duration::from_secs(2)).await;
                let active = this.update_in(cx, |this, w, cx| {
                    if this.page != Page::Project {
                        this.board.catalog_polling = false;
                        return false;
                    }
                    let Some(dir) = this.board.dir.as_ref() else {
                        return true;
                    };
                    if watched_dir.as_ref() != Some(dir) {
                        watched_dir = Some(dir.clone());
                        last = None;
                        last_memory = None;
                    }
                    let current = stamp(&dir.join("projects.json"));
                    if current != last {
                        last = current;
                        if let Ok(latest) = Library::open(dir)
                            && latest.revision != this.board.library.revision
                        {
                            this.accept_catalog(latest, w, cx);
                        }
                    }
                    if this.board.section == Section::Memory
                        && let Ok(dir) = this.project_memory_dir()
                    {
                        let current = stamp(&dir.join("memory.sqlite3"));
                        if current != last_memory {
                            last_memory = current;
                            this.refresh_project_memory(cx);
                        }
                    }
                    true
                });
                if !active.unwrap_or(false) {
                    break;
                }
            }
        })
        .detach();
    }
    pub(super) fn watch_agents(&mut self, cx: &mut Context<Self>) {
        if self.board.polling || !self.has_running_agent() {
            return;
        }
        self.board.polling = true;
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            loop {
                executor.timer(Duration::from_millis(700)).await;
                if !this
                    .update(cx, |this, cx| {
                        let active = this.page == Page::Project && this.has_running_agent();
                        this.board.polling = active;
                        cx.notify();
                        active
                    })
                    .unwrap_or(false)
                {
                    break;
                }
            }
        })
        .detach();
    }
    fn launch_agent(&mut self, cx: &mut Context<Self>) -> Result<()> {
        let p = self.board.library.project().context("请选择项目")?;
        let work = p.selected().context("请选择开发事项")?;
        let i = self.board.viewing;
        ensure!(i == work.stage, "只能执行当前阶段");
        ensure!(!self.has_running_agent(), "此事项已有 CLI 正在运行");
        let profile = self.chosen_profile(cx);
        profile.validate()?;
        let work_id = work.id.clone();
        let repository = work.target(i);
        let home = self.board.dir.clone().context("找不到项目库")?;
        let brief = self.board.note.read(cx).value().to_string();
        let binary = self.runner.binary()?;
        let path = self.runner.path()?;
        let mut folder = None;
        if !self.change_board(cx, |lib| {
            let p = lib.project_mut()?;
            Self::remember_profile(p, i, &profile)?;
            folder = Some(p.reserve(
                &home,
                &work_id,
                i,
                aep::handoff::Assignment {
                    profile,
                    brief,
                    repository,
                    external: None,
                },
            )?);
            Ok(())
        }) {
            return Ok(());
        }
        let d = folder.context("未创建运行记录")?;
        let supervisor = fs::File::create(d.join("supervisor.log"))?;
        match Command::new(binary)
            .env("PATH", path)
            .env_remove(aep::CONTROL_DEVICE_TOKEN)
            .env_remove(aep::CONTROL_OPERATOR_TOKEN)
            .arg("workflow-stage")
            .arg("--request")
            .arg(d.join("request.json"))
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(supervisor))
            .spawn()
        {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(e) => {
                crate::lifecycle::save(
                    &d,
                    "evidence.json",
                    &serde_json::json!({"status":"failed","error":e.to_string()}),
                )?;
                return Err(e.into());
            }
        }
        self.watch_agents(cx);
        Ok(())
    }
    fn log_view(
        &self,
        dir: &Path,
        id: &str,
        name: &'static str,
        page: u64,
        cx: &Context<Self>,
    ) -> Div {
        let mut tabs = h_flex().gap_1().flex_wrap();
        for (label, file) in [
            ("交付物", "result.txt"),
            ("标准输出", "stdout.log"),
            ("错误日志", "stderr.log"),
            ("输入上下文", "prompt.txt"),
            ("执行回执", "evidence.json"),
        ] {
            let key = id.to_string();
            tabs = tabs.child(
                button(format!("workflow-log-{file}"), label, file == name).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.board.log_view = Some((key.clone(), file, 0));
                        cx.notify();
                    },
                )),
            );
        }
        let mut view = v_flex()
            .bg(rgb(CANVAS))
            .rounded_md()
            .p_2()
            .gap_2()
            .child(tabs);
        match workflow::log_page(&dir.join(name), page) {
            Ok((text, total)) => {
                if total > 1 {
                    let mut pages = h_flex()
                        .gap_2()
                        .child(caption(format!("{} / {total}", page + 1)));
                    for (label, n) in [
                        ("上一页", page.checked_sub(1)),
                        ("下一页", (page + 1 < total).then_some(page + 1)),
                    ] {
                        if let Some(n) = n {
                            let id = id.to_string();
                            pages = pages.child(
                                button(format!("workflow-page-{n}"), label, false).on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        this.board.log_view = Some((id.clone(), name, n));
                                        cx.notify();
                                    }),
                                ),
                            );
                        }
                    }
                    view = view.child(pages);
                }
                view.child(detail_text(
                    "workflow-log-content",
                    if text.is_empty() {
                        "暂无输出"
                    } else {
                        &text
                    },
                ))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => view.child(caption("尚未生成")),
            Err(e) => view.child(caption(e.to_string())),
        }
    }
    pub(super) fn agent_panel(
        &self,
        p: &Project,
        work: &Work,
        i: usize,
        cx: &Context<Self>,
    ) -> Div {
        let mut clients = h_flex().gap_1().flex_wrap();
        for client in Client::ALL {
            clients = clients.child(
                button(
                    format!("workflow-client-{client:?}"),
                    client.label(),
                    client == self.board.client,
                )
                .on_click(cx.listener(move |this, _, w, cx| {
                    if this.board.client == client {
                        return;
                    }
                    let profile = this
                        .board
                        .library
                        .project()
                        .and_then(|p| {
                            p.selected()?
                                .profiles
                                .get(&i)
                                .filter(|saved| saved.client == client)
                                .or_else(|| p.preset(client))
                        })
                        .cloned()
                        .unwrap_or(Profile {
                            client,
                            model: String::new(),
                            check_command: String::new(),
                            executable: String::new(),
                        });
                    this.fill_profile(profile, w, cx);
                    cx.notify();
                })),
            );
        }
        let mut content = v_flex()
            .border_t_1()
            .border_color(rgb(LINE))
            .pt_3()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .flex_wrap()
                    .child(heading("阶段 Agent"))
                    .child(clients),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(input_field("workflow-model", &self.board.model))
                    .child(input_field("workflow-executable", &self.board.executable)),
            );
        let profile = self.chosen_profile(cx);
        let program = self.runner.path().and_then(|path| profile.program(&path));
        let mut settings = h_flex().gap_2().flex_wrap().child(caption(
            program.map_or_else(|e| e.to_string(), |p| p.display().to_string()),
        ));
        let models: &[&str] = match self.board.client {
            Client::Codex => &["gpt-6.2-astra", "gpt-6.2-sol"],
            Client::Claude => &["sonnet"],
            _ => &[],
        };
        for &name in models {
            settings = settings.child(
                button(format!("model-preset-{name}"), name, profile.model == name).on_click(
                    cx.listener(move |this, _, w, cx| {
                        this.board
                            .model
                            .update(cx, |s, cx| s.set_value(name, w, cx));
                        cx.notify();
                    }),
                ),
            );
        }
        content = content.child(settings);
        match self.board.client {
            Client::Claude => {
                content = content.child(input_field(
                    "workflow-allow-command",
                    &self.board.check_command,
                ));
            }
            Client::ZCode => {
                content = content.child(caption("留空沿用 ZCode 默认模型；指定模型使用 provider/model 或 provider/model@推理档位。"));
            }
            Client::Custom => {
                content=content.child(caption("程序在事项工作区启动，从标准输入读取任务，向标准输出写交付物。指定模型时传入 --model。"));
            }
            _ => {}
        }
        let mut repos = h_flex().flex_wrap().gap_2().child(caption("工作区"));
        let target = work.target(i);
        if work.repositories.is_empty() {
            repos = repos.child(t("项目文件", FG));
        }
        for (j, repo) in work.repositories.iter().enumerate() {
            let path = repo.clone();
            repos = repos.child(
                button(
                    format!("workflow-repo-{j}"),
                    basename(repo),
                    Some(repo) == target.as_ref(),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.change_board(cx, |lib| {
                        lib.project_mut()?
                            .selected_mut()?
                            .targets
                            .insert(i, path.clone());
                        Ok(())
                    });
                })),
            );
        }
        let mut actions = h_flex().gap_2().flex_wrap().child(
            button("workflow-save-profile", "保存 CLI 配置", false).on_click(cx.listener(
                move |this, _, _, cx| {
                    let profile = this.chosen_profile(cx);
                    this.change_board(cx, |lib| {
                        profile.validate()?;
                        Self::remember_profile(lib.project_mut()?, i, &profile)
                    });
                },
            )),
        );
        if i == work.stage && !self.has_running_agent() {
            if let Some(home) = &self.board.dir {
                let quote = |text: &str| format!("'{}'", text.replace('\'', "'\"'\"'"));
                let profile = self.chosen_profile(cx);
                let mut cmd = format!(
                    "aep project --home {} --project {} --work {} claim --stage {} --client {}",
                    quote(&home.display().to_string()),
                    p.id,
                    work.id,
                    i + 1,
                    format!("{:?}", profile.client).to_lowercase()
                );
                if !profile.model.is_empty() {
                    cmd.push_str(&format!(" --model {}", quote(&profile.model)));
                }
                cmd.push_str(r#" --session "${AEP_SESSION_ID:?请先设置本机会话名或 ID}""#);
                actions = actions.child(
                    button("workflow-claim-command", "复制独立会话领取命令", false).on_click(
                        cx.listener(move |_, _, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(cmd.clone()));
                        }),
                    ),
                );
            }
            let label = if work.current_attempt.contains_key(&i) {
                "重新执行"
            } else {
                "运行阶段 Agent"
            };
            actions = actions.child(primary("workflow-run", label).on_click(cx.listener(
                |this, _, _, cx| {
                    if let Err(e) = this.launch_agent(cx) {
                        this.board.error = Some(format!("{e:#}"));
                    }
                    cx.notify();
                },
            )));
        }
        content = content.child(repos).child(actions);
        let Some(base) = self.board.dir.as_ref() else {
            return content;
        };
        for a in work.attempts.iter().rev().filter(|a| a.stage == i) {
            let d = workflow::attempt_dir(base, &p.id, &work.id, &a.id);
            let ev = workflow::evidence(&d)
                .unwrap_or_else(|e| serde_json::json!({"error":e.to_string()}));
            let mut bar = h_flex()
                .gap_2()
                .flex_wrap()
                .child(chip(
                    phase(&ev),
                    if ev["status"] == "failed" { RED } else { MINT },
                ))
                .child(caption(format!(
                    "{} · {} · {}",
                    a.profile.client.label(),
                    if a.profile.model.is_empty() {
                        "默认模型"
                    } else {
                        &a.profile.model
                    },
                    a.repository
                        .as_deref()
                        .map(basename)
                        .unwrap_or_else(|| "项目文件".into())
                )));
            if let Some(session) = ev["session"].as_str() {
                bar = bar.child(caption(format!("独立会话 · {session}")));
            }
            if let Some(session) = ev
                .pointer("/native_session/session")
                .and_then(Value::as_str)
            {
                let source = if ev
                    .pointer("/native_session/transport")
                    .and_then(Value::as_str)
                    == Some("desktop")
                {
                    "ZCode 桌面"
                } else {
                    "ZCode app-server"
                };
                bar = bar.child(caption(format!("{source} · {session}")));
            }
            let expanded = self
                .board
                .log_view
                .as_ref()
                .is_some_and(|(id, _, _)| id == &a.id);
            let current = a.id.clone();
            bar = bar.child(
                button(
                    format!("workflow-inspect-{current}"),
                    if expanded {
                        "收起控制台"
                    } else {
                        "查看控制台"
                    },
                    expanded,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.board.log_view = if expanded {
                        None
                    } else {
                        Some((current.clone(), "result.txt", 0))
                    };
                    cx.notify();
                })),
            );
            let archive = d.clone();
            bar = bar.child(
                button(format!("workflow-evidence-{}", a.id), "执行档案", false)
                    .on_click(cx.listener(move |_, _, _, cx| cx.reveal_path(&archive))),
            );
            if let Some(ws) = ev["workspace"].as_str() {
                let ws = PathBuf::from(ws);
                bar = bar.child(
                    button(format!("workflow-files-{}", a.id), "工作区文件", false)
                        .on_click(cx.listener(move |_, _, _, cx| cx.reveal_path(&ws))),
                );
            }
            if busy(&ev) {
                if let Some(dispatch) = self.desktop_dispatch(&d, "阶段", cx) {
                    bar = bar.child(dispatch);
                }
                let id = a.id.clone();
                let attempt = d.clone();
                bar = bar.child(
                    button(
                        format!("workflow-stop-{id}"),
                        if ev["status"] == "claimed" {
                            "取消领取"
                        } else {
                            "取消执行"
                        },
                        false,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Err(e) = aep::handoff::cancel(&attempt) {
                            this.board.error = Some(e.to_string());
                        }
                        cx.notify();
                    })),
                );
            }
            content = content.child(bar);
            if let Some((key, name, page)) = &self.board.log_view
                && key == &a.id
            {
                content = content.child(self.log_view(&d, key, name, *page, cx));
                continue;
            }
            if work.current_attempt.get(&i) == Some(&a.id) {
                let memory = ev["read_memory"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|v| format!("#{v}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                content = content.child(caption(format!(
                    "项目记忆读取 {}   写入 {}",
                    if memory.is_empty() { "—" } else { &memory },
                    ev["written_memory"]
                        .as_i64()
                        .map_or("—".into(), |id| format!("#{id}"))
                )));
                let mut output = ev["error"].as_str().unwrap_or_default().to_string();
                for name in ["result.txt", "stderr.log", "stdout.log", "supervisor.log"] {
                    let log = workflow::tail(&d.join(name), 1800);
                    if !log.trim().is_empty() {
                        if !output.contains(log.trim()) {
                            output.push('\n');
                            output.push_str(log.trim());
                        }
                        break;
                    }
                }
                if !output.trim().is_empty() {
                    content = content.child(
                        detail_text("workflow-console", output.trim())
                            .bg(rgb(CANVAS))
                            .p_2()
                            .rounded_md(),
                    );
                }
            }
        }
        content
    }
}
