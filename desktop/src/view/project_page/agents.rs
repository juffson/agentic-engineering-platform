use super::*;
use aep::{cancel::Cancellation, workflow::Request};
use std::{
    fs,
    os::unix::{fs::DirBuilderExt, process::CommandExt},
    process::{Command, Stdio},
};

fn busy(state: &Value) -> bool {
    matches!(state["status"].as_str(), Some("running" | "initializing"))
}
fn phase(v: &Value) -> &str {
    match v["status"].as_str() {
        Some("succeeded") => "已完成",
        Some("initializing") => "启动中",
        Some("running") => "运行中",
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
    pub(super) fn reviewed_result(&self, index: usize) -> Result<Option<String>> {
        let work = self
            .board
            .library
            .project()
            .and_then(Project::selected)
            .context("请选择开发事项")?;
        let Some(id) = work.current_attempt.get(&index) else {
            return Ok(None);
        };
        let d = self.attempt_path(id).context("找不到执行目录")?;
        let ev = workflow::evidence(&d)?;
        ensure!(
            ev["status"] == "succeeded",
            "本阶段执行{}，请先完成或重试",
            phase(&ev)
        );
        Ok(Some(fs::read_to_string(d.join("result.txt"))?))
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
        let repo = work.target(i);
        let id = uuid::Uuid::new_v4().simple().to_string();
        let d = self.attempt_path(&id).context("找不到项目库")?;
        let mut approved = vec![];
        for (j, s) in work.template.stages()[..i].iter().enumerate() {
            let mut note = work.notes[j].chars().take(4800).collect::<String>();
            if let Some(prev) = work
                .current_attempt
                .get(&j)
                .and_then(|id| self.attempt_path(id))
            {
                let path = prev.join("result.txt");
                let result = fs::read_to_string(&path)
                    .with_context(|| format!("读取前序交付物 {}", path.display()))?;
                if !work.notes[j].trim().eq(result.trim()) {
                    note.push_str(&format!(
                        "\nAgent 交付物（以人工记录为准）：{}",
                        result.chars().take(3200).collect::<String>()
                    ));
                }
                let evidence = workflow::evidence(&prev)?;
                if let Some(commands) = evidence["commands"].as_array().filter(|c| !c.is_empty()) {
                    note.push_str(&format!(
                        "\n执行器记录的命令和退出码：{}",
                        serde_json::to_string(commands)?
                    ));
                }
                note.push_str(&format!(
                    "\n完整交付物：{}\n原始日志：{}",
                    path.display(),
                    prev.join("stdout.log").display()
                ));
            }
            approved.push((s.name.to_string(), note));
        }
        let brief = self.board.note.read(cx).value().to_string();
        let req = Request {
            project: p.name.clone(),
            work: work.name.clone(),
            template: work.template,
            stage: i,
            profile: profile.clone(),
            repository: repo.clone(),
            ticket: repo.as_deref().map(|r| work.ticket(r)),
            brief: brief.clone(),
            approved,
        };
        let binary = self.runner.binary()?;
        let path = self.runner.path()?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&d)?;
        crate::lifecycle::save(&d, "request.json", &req)?;
        if !self.change_board(cx, |lib| {
            let p = lib.project_mut()?;
            Self::remember_profile(p, i, &profile)?;
            let item = p.selected_mut()?;
            item.notes[i] = brief;
            item.attempts.push(Attempt {
                id: id.clone(),
                stage: i,
                profile,
                repository: repo,
            });
            item.current_attempt.insert(i, id);
            Ok(())
        }) {
            return Ok(());
        }
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
                let id = a.id.clone();
                let data = d.parent().unwrap().parent().unwrap().to_owned();
                bar = bar.child(
                    button(format!("workflow-stop-{id}"), "取消执行", false).on_click(cx.listener(
                        move |this, _, _, cx| {
                            if let Err(e) = Cancellation::new(&data, &id).and_then(|s| s.request())
                            {
                                this.board.error = Some(e.to_string());
                            }
                            cx.notify();
                        },
                    )),
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
