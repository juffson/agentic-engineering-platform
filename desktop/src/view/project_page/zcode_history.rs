use super::*;

pub(super) struct History {
    pub(super) project: String,
    pub(super) repo: PathBuf,
    rows: Vec<Value>,
    pending: Option<uuid::Uuid>,
    message: String,
}

fn command(
    program: &Path,
    repo: &Path,
    home: &Path,
    project: &str,
    args: &[&str],
) -> Result<Vec<Value>> {
    let output = std::process::Command::new(program)
        .arg("--repo")
        .arg(repo)
        .arg("zcode-group")
        .arg("--project-home")
        .arg(home)
        .arg("--project-id")
        .arg(project)
        .args(args)
        .output()?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

impl Studio {
    pub(super) fn show_zcode_history(&mut self, repo: &Path, cx: &mut Context<Self>) {
        let Some(project) = self.board.library.project() else {
            return;
        };
        self.board.zcode_history = Some(History {
            project: project.id.clone(),
            repo: repo.into(),
            rows: vec![],
            pending: None,
            message: String::new(),
        });
        self.load_zcode_history(false, cx);
    }

    fn load_zcode_history(&mut self, sync: bool, cx: &mut Context<Self>) {
        let Some(history) = self.board.zcode_history.as_mut() else {
            return;
        };
        if history.pending.is_some() {
            return;
        }
        let id = uuid::Uuid::new_v4();
        history.pending = Some(id);
        history.message = if sync {
            "正在同步并核对空项目…"
        } else {
            "正在读取任务记录…"
        }
        .into();
        let repo = history.repo.clone();
        let home = self.board.dir.clone();
        let project = history.project.clone();
        let program = self.runner.binary();
        let job = cx.background_executor().spawn(async move {
            let program = program?;
            let home = home.context("找不到项目库目录")?;
            let outcome = sync.then(|| command(&program, &repo, &home, &project, &[]));
            let rows = command(&program, &repo, &home, &project, &["--list"])?;
            let retired: std::collections::HashSet<_> = rows
                .iter()
                .filter(|r| r["cleaned"] == true)
                .filter_map(|r| r["source_project"].as_str())
                .collect();
            let message = match outcome {
                Some(Ok(changes)) => format!(
                    "同步完成 · 本次归组 {} 个会话 · 累计确认清理 {} 个空项目",
                    changes.iter().filter(|r| r["moved"] == true).count(),
                    retired.len()
                ),
                Some(Err(e)) => format!("同步未全部完成 · {e:#}"),
                None => String::new(),
            };
            anyhow::Ok((rows, message))
        });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(h) = this.board.zcode_history.as_mut()
                    && h.pending == Some(id)
                {
                    h.pending = None;
                    match result {
                        Ok((rows, msg)) => {
                            h.rows = rows;
                            h.message = msg
                        }
                        Err(e) => h.message = format!("读取失败 · {e:#}"),
                    };
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn zcode_history_panel(&self, history: &History, cx: &Context<Self>) -> Div {
        let mut actions = h_flex().gap_2().flex_wrap().child(
            button("zcode-history-back", "返回项目设置", false)
                .on_click(cx.listener(|s, _, w, cx| s.section(Section::Settings, w, cx))),
        );
        if history.pending.is_none() {
            actions = actions
                .child(
                    button("zcode-history-sync", "同步历史任务并清理空项目", true)
                        .on_click(cx.listener(|s, _, _, cx| s.load_zcode_history(true, cx))),
                )
                .child(
                    button("zcode-history-refresh", "刷新列表", false)
                        .on_click(cx.listener(|s, _, _, cx| s.load_zcode_history(false, cx))),
                );
        }
        let count = history
            .rows
            .iter()
            .filter(|r| r["session"].is_string())
            .count();
        let grouped = history.rows.iter().filter(|r| r["grouped"] == true).count();
        let mut body = v_flex()
            .gap_3()
            .child(heading("ZCode 任务归组"))
            .child(caption(history.repo.display().to_string()))
            .child(actions);
        if !history.message.is_empty() {
            body = body.child(detail_text("zcode-history-status", &history.message));
        }
        if !history.rows.is_empty() {
            body = body.child(detail_text(
                "zcode-history-count",
                &format!("{count} 个会话 · {grouped} 个已归入仓库项目"),
            ));
        } else if history.pending.is_none() && history.message.is_empty() {
            body = body.child(caption("这个仓库还没有 ZCode 桌面任务记录。"));
        }
        for (i, r) in history.rows.iter().enumerate() {
            if let Some(error) = r["error"].as_str() {
                body = body.child(
                    panel()
                        .child(caption(string(r, "dir").to_owned()))
                        .child(t(error.to_owned(), AMBER)),
                );
                continue;
            }
            let dir = PathBuf::from(string(r, "dir"));
            let mut top = h_flex().justify_between().gap_2().child(
                heading(
                    r["title"]
                        .as_str()
                        .unwrap_or_else(|| string(r, "run"))
                        .to_owned(),
                )
                .min_w_0()
                .flex_1(),
            );
            if r["can_open"] == true {
                let path = dir.clone();
                top = top.child(
                    button(format!("zcode-history-open-{i}"), "打开原会话", false).on_click(
                        cx.listener(move |s, _, _, cx| s.open_zcode_task(path.clone(), cx)),
                    ),
                );
            }
            let mut card = panel()
                .flex_none()
                .child(top)
                .child(caption(format!(
                    "{} · {}",
                    string(r, "role"),
                    string(r, "run")
                )))
                .child(field(
                    "归属项目",
                    r["project_name"].as_str().unwrap_or("无可核对的桌面归属"),
                ))
                .child(field(
                    if r["isolated"] == true {
                        "隔离工作区"
                    } else {
                        "工作目录"
                    },
                    string(r, "workspace"),
                ))
                .child(detail_text(
                    SharedString::from(format!("zcode-history-cleanup-{i}")),
                    string(r, "cleanup"),
                ));
            if r["workspace_exists"] == false {
                card = card.child(caption("工作目录已不存在"));
            }
            if let Some((_, msg)) = self.zcode_navigation.as_ref().filter(|(p, _)| p == &dir) {
                card = card.child(detail_text("zcode-history-open-status", msg));
            }
            body = body.child(card);
        }
        body
    }
}
