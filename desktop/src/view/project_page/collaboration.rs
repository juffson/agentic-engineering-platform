use super::*;
use crate::{https_pilot::Connection, https_queue::Site};
use aep::project::RecordKind;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum InfraView {
    Workers,
    Rotation,
}

pub(super) struct SiteEditor {
    original: Option<Connection>,
    project: String,
    repository: PathBuf,
    fields: SiteFields,
}

pub(in crate::view) struct SiteFields {
    endpoint: Entity<InputState>,
    ca: Entity<InputState>,
    operator: Entity<InputState>,
}

impl SiteFields {
    pub(in crate::view) fn new(
        site: Option<&Connection>,
        w: &mut Window,
        cx: &mut Context<Studio>,
    ) -> Self {
        let mut input = |value: Option<String>, env, placeholder| {
            let value = value.unwrap_or_else(|| std::env::var(env).unwrap_or_default());
            cx.new(|cx| {
                let mut s = InputState::new(w, cx).placeholder(placeholder);
                s.set_value(value, w, cx);
                s
            })
        };
        Self {
            endpoint: input(
                site.map(|s| s.endpoint.clone()),
                "AEP_STUDIO_CONTROL_URL",
                "https://control.example.org",
            ),
            ca: input(
                site.map(|s| {
                    s.ca.as_ref()
                        .map_or(String::new(), |p| p.display().to_string())
                }),
                "AEP_STUDIO_CONTROL_CA",
                "系统信任的证书可留空",
            ),
            operator: input(
                site.map(|s| s.operator_file.display().to_string()),
                "AEP_STUDIO_CONTROL_OPERATOR_FILE",
                "操作员令牌文件的绝对路径",
            ),
        }
    }

    pub(in crate::view) fn read(&self, cx: &App) -> Result<Connection> {
        let ca = self.ca.read(cx).value().trim().to_owned();
        let site = Connection {
            endpoint: self.endpoint.read(cx).value().trim().into(),
            ca: (!ca.is_empty()).then(|| ca.into()),
            operator_file: self.operator.read(cx).value().trim().into(),
        };
        site.validate()?;
        Ok(site)
    }

    pub(in crate::view) fn render(&self) -> Div {
        let field = |id, label, state| {
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(caption(label))
                .child(
                    div()
                        .id(id)
                        .test_support()
                        .child(Input::new(state).aria_label(label)),
                )
        };
        v_flex()
            .gap_2()
            .child(field("site-endpoint", "HTTPS 控制面", &self.endpoint))
            .child(
                h_flex()
                    .gap_2()
                    .child(field("site-ca", "CA 证书路径", &self.ca))
                    .child(field("site-operator", "操作员令牌文件路径", &self.operator)),
            )
    }
}

pub(super) struct WorkDesk {
    pub(super) work: String,
    recipient: Option<String>,
    note: Entity<TextareaState>,
    sent: Option<String>,
}

impl Studio {
    pub(in crate::view) fn in_settings(&self) -> bool {
        self.board.section == Section::Settings && self.board.library.project().is_some()
    }
    pub(in crate::view) fn execution_project_id(&self) -> Option<String> {
        if self.in_settings() || self.context_work().is_some() {
            self.board.library.project().map(|p| p.id.clone())
        } else {
            None
        }
    }
    pub(in crate::view) fn project_site(&self, repo: &Path) -> Option<&Connection> {
        if !matches!(self.board.section, Section::Settings | Section::Work) {
            return None;
        }
        self.board.library.project()?.https_sites.get(repo)
    }
    pub(super) fn edit_site(&mut self, path: &Path, w: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.board.library.project() else {
            return;
        };
        let site = p.https_sites.get(path).cloned().or_else(|| {
            Workspace::open(path)
                .ok()
                .and_then(|ws| Site::restore(&ws))
                .map(|s| s.connection)
        });
        self.board.site_editor = Some(SiteEditor {
            original: p.https_sites.get(path).cloned(),
            project: p.id.clone(),
            repository: path.into(),
            fields: SiteFields::new(site.as_ref(), w, cx),
        });
        self.board.error = None;
        cx.notify();
    }

    fn save_site(&mut self, clear: bool, cx: &mut Context<Self>) {
        let Some(e) = &self.board.site_editor else {
            return;
        };
        let (pid, repo) = (e.project.clone(), e.repository.clone());
        let original = e.original.clone();
        let site = if clear {
            Ok(None)
        } else {
            e.fields.read(cx).map(Some)
        };
        if self.change_board(cx, |lib| {
            let p = lib.project_mut()?;
            ensure!(p.id == pid, "项目已切换，请重新打开设置");
            ensure!(
                p.https_sites.get(&repo) == original.as_ref(),
                "此连接已更新，请重新打开配置"
            );
            p.configure_site(&repo, site?)
        }) {
            self.board.site_editor = None;
        }
    }

    pub(super) fn project_settings(&self, p: &Project, cx: &Context<Self>) -> Div {
        let mut body = v_flex()
            .gap_3()
            .child(self.project_zcode_settings(p, cx))
            .child(heading("执行环境"))
            .child(caption(
                "每个关联仓库可指定 HTTPS 控制面。未指定时沿用这个仓库已有的站点连接。",
            ));
        if p.repositories.is_empty() {
            return body.child(
                button("settings-add-repo", "关联第一个仓库", true).on_click(
                    cx.listener(|this, _, w, cx| this.section(Section::Repositories, w, cx)),
                ),
            );
        }
        for (i, repo) in p.repositories.iter().enumerate() {
            let edit = repo.clone();
            let mut card = panel()
                .flex_none()
                .child(
                    h_flex()
                        .justify_between()
                        .gap_2()
                        .child(heading(basename(repo)))
                        .child(
                            button(format!("settings-edit-{i}"), "配置 HTTPS 连接", true).on_click(
                                cx.listener(move |this, _, w, cx| this.edit_site(&edit, w, cx)),
                            ),
                        ),
                )
                .child(caption(repo.display().to_string()));
            if let Some(e) = self
                .board
                .site_editor
                .as_ref()
                .filter(|e| e.project == p.id && e.repository == *repo)
            {
                card = card.child(e.fields.render()).child(
                    h_flex()
                        .gap_2()
                        .child(
                            button("settings-save", "保存项目连接", true)
                                .on_click(cx.listener(|this, _, _, cx| this.save_site(false, cx))),
                        )
                        .when(p.https_sites.contains_key(repo), |row| {
                            row.child(
                                button("settings-clear", "恢复仓库默认连接", false).on_click(
                                    cx.listener(|this, _, _, cx| this.save_site(true, cx)),
                                ),
                            )
                        })
                        .child(
                            button("settings-close", "关闭", false).on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.board.site_editor = None;
                                    cx.notify();
                                },
                            )),
                        ),
                );
            } else {
                card = card.child(field(
                    "控制面",
                    p.https_sites
                        .get(repo)
                        .map_or("沿用仓库连接", |s| &s.endpoint),
                ));
            }
            let mut remote = h_flex().gap_2().flex_wrap();
            for (key, label, destination) in [
                ("workers", "Worker、队列与 Deploy Key", InfraView::Workers),
                ("rotation", "滚动轮换与演练历史", InfraView::Rotation),
            ] {
                let repo = repo.clone();
                remote = remote.child(
                    button(format!("settings-{key}-{i}"), label, false).on_click(cx.listener(
                        move |this, _, w, cx| {
                            if this.open_project_repo(&repo, Page::HttpsPilot, w, cx) {
                                this.board.infra_view = Some(destination);
                                cx.notify();
                            }
                        },
                    )),
                );
            }
            let mut history = h_flex().gap_2().flex_wrap();
            for (key, label, page) in [
                ("local", "本地任务", Page::Local),
                ("relay", "两轮接力", Page::Relay),
                ("memory", "仓库共享记忆", Page::Memory),
                ("runs", "运行证据", Page::Runs),
                ("lifecycle", "隔离全流程", Page::Lifecycle),
                ("https", "HTTPS 任务与试点", Page::HttpsPilot),
            ] {
                let repo = repo.clone();
                history = history.child(
                    button(format!("settings-{key}-{i}"), label, false).on_click(cx.listener(
                        move |this, _, w, cx| {
                            this.project_tool(&repo, page, None, w, cx);
                        },
                    )),
                );
            }
            body = body.child(
                card.child(remote)
                    .child(caption("仓库工具与历史"))
                    .child(history),
            );
        }
        body
    }

    pub(in crate::view) fn infrastructure_page(&self, cx: &Context<Self>) -> Option<Div> {
        if !self.in_settings() {
            return None;
        }
        let mode = self.board.infra_view?;
        let repo = self.workspace.as_ref()?.root.clone();
        let title = match mode {
            InfraView::Workers => "Worker 与队列",
            InfraView::Rotation => "Deploy Key 滚动轮换",
        };
        let mut body = v_flex()
            .id("https-pilot-page")
            .test_support()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .gap_3()
            .p_5()
            .child(caption(format!("项目设置 / {}", basename(&repo))))
            .child(heading(title).text_2xl());
        if mode == InfraView::Workers {
            if let Some(site) = &self.https_queue {
                body = body.child(self.https_queue_panel(site, cx));
            } else {
                body = body.child(caption(
                    "配置这个仓库的 HTTPS 连接后，即可读取 Worker 和队列。",
                ));
            }
            body = body.child(h_flex().child(
                button("infra-site-edit", "编辑项目连接", false).on_click(cx.listener(
                    move |this, _, w, cx| {
                        this.section(Section::Settings, w, cx);
                        this.edit_site(&repo, w, cx);
                    },
                )),
            ));
        } else {
            if !self.rolling_busy() && self.https_rolling_form.is_none() {
                body = body.child(
                    h_flex().child(
                        button("https-rolling-mode", "配置轮换演练", true)
                            .on_click(cx.listener(|this, _, w, cx| this.open_rolling(w, cx))),
                    ),
                );
            }
            if let Some(panel) = self.rolling_panel(cx) {
                body = body.child(panel);
            } else {
                body = body.child(caption("这个仓库还没有轮换演练记录。"));
            }
        }
        Some(
            h_flex()
                .size_full()
                .bg(rgb(CANVAS))
                .child(self.rail(cx))
                .child(body),
        )
    }

    fn open_project_repo(
        &mut self,
        repo: &Path,
        page: Page,
        w: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.project_tool(repo, page, None, w, cx);
        self.page == page && self.workspace.as_ref().is_some_and(|ws| ws.root == repo)
    }
    pub(super) fn work_tabs(&self, work: &Work, open: bool, cx: &Context<Self>) -> Div {
        let id = work.id.clone();
        h_flex()
            .gap_2()
            .child(
                button("work-flow-tab", "阶段工作流", !open).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.board.desk = None;
                        cx.notify();
                    },
                )),
            )
            .child(
                button("work-collab-tab", "协作与交接", open).on_click(cx.listener(
                    move |this, _, w, cx| {
                        if this.board.desk.as_ref().is_none_or(|d| d.work != id) {
                            this.board.desk = Some(WorkDesk {
                                work: id.clone(),
                                recipient: None,
                                sent: None,
                                note: cx.new(|cx| {
                                    TextareaState::new(w, cx)
                                        .placeholder("写下结论、证据位置和下一步…")
                                }),
                            });
                        }
                        cx.notify();
                    },
                )),
            )
    }

    pub(super) fn work_collaboration(&self, p: &Project, work: &Work, cx: &Context<Self>) -> Div {
        let desk = self.board.desk.as_ref().unwrap();
        let mut workspaces = panel()
            .flex_none()
            .child(heading(work.name.clone()))
            .child(caption("事项执行记录"));
        if work.repositories.is_empty() {
            workspaces = workspaces.child(caption(
                "在阶段工作流的「事项仓库与任务」中纳入仓库后，即可关联执行记录。",
            ));
        }
        for (i, repo) in work.repositories.iter().enumerate() {
            let mut links = h_flex().gap_2().flex_wrap();
            for (key, name, page) in [
                ("relay", "两轮接力", Page::Relay),
                ("https", "HTTPS 远程任务", Page::HttpsPilot),
                ("local", "本地任务与验收", Page::Local),
            ] {
                let repo = repo.clone();
                let ticket = (page == Page::Local).then(|| work.ticket(&repo));
                links = links.child(button(format!("work-{key}-{i}"), name, false).on_click(
                    cx.listener(move |this, _, w, cx| {
                        this.project_tool(&repo, page, ticket.as_deref(), w, cx);
                    }),
                ));
            }
            let count = work
                .records
                .iter()
                .filter(|r| r.repository == *repo)
                .count();
            workspaces = workspaces.child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .justify_between()
                    .child(
                        v_flex()
                            .child(t(basename(repo), FG))
                            .child(caption(format!("已关联 {count} 条接力或远程记录"))),
                    )
                    .child(links),
            );
        }
        if !work.records.is_empty() {
            let mut saved = v_flex().gap_1().child(caption("已关联的记录"));
            for (index, r) in work.records.iter().enumerate() {
                let (repo, id, kind) = (r.repository.clone(), r.id.clone(), r.kind);
                let wid = work.id.clone();
                saved = saved.child(
                    h_flex()
                        .gap_2()
                        .justify_between()
                        .child(t(format!("{} · {}", basename(&repo), id), FG).truncate())
                        .child(
                            button(
                                format!("unlink-{}-{}", kind.key(), index),
                                "解除关联",
                                false,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.change_board(cx, |lib| {
                                        lib.project_mut()?
                                            .work_by_id_mut(&wid)?
                                            .unlink_record(&repo, kind, &id)
                                    });
                                },
                            )),
                        ),
                );
            }
            workspaces = workspaces.child(saved);
        }
        let mut incoming = panel().flex_none().child(heading("收到的事项交接"));
        if work.handoffs.is_empty() {
            incoming = incoming.child(caption("尚未收到其他事项的交接。"));
        }
        for h in work.handoffs.iter().rev() {
            incoming = incoming
                .child(caption(format!("{} · {}", h.name, h.stage)))
                .child(t(h.note.clone(), FG));
        }
        let mut outgoing = panel().flex_none().child(heading("交给其他开发事项"));
        if p.work.len() > 1 {
            let mut choices = h_flex().gap_2().flex_wrap().child(caption("接收事项"));
            for other in p.work.iter().filter(|other| other.id != work.id) {
                let id = other.id.clone();
                choices = choices.child(
                    button(
                        format!("handoff-to-{id}"),
                        other.name.clone(),
                        desk.recipient.as_ref() == Some(&id),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(d) = this.board.desk.as_mut() {
                            d.recipient = Some(id.clone());
                            d.sent = None;
                        }
                        cx.notify();
                    })),
                );
            }
            outgoing = outgoing
                .child(choices)
                .child(
                    div()
                        .id("work-handoff-note")
                        .test_support()
                        .child(Textarea::new(&desk.note).aria_label("事项交接摘要")),
                )
                .child(
                    button("work-send-handoff", "保存并交接", true)
                        .on_click(cx.listener(|this, _, w, cx| this.send_work_handoff(w, cx))),
                );
            if let Some(sent) = &desk.sent {
                outgoing = outgoing.child(caption(sent.clone()));
            }
        } else {
            outgoing = outgoing.child(caption(
                "创建第二个开发事项后，可在这里定向交接结论和证据。",
            ));
        }
        v_flex()
            .gap_3()
            .child(workspaces)
            .child(incoming)
            .child(outgoing)
    }

    fn send_work_handoff(&mut self, w: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = &self.board.desk else { return };
        let (from, to, note) = (
            d.work.clone(),
            d.recipient.clone(),
            d.note.read(cx).value().to_string(),
        );
        if self.change_board(cx, |lib| {
            let p = lib.project_mut()?;
            ensure!(p.selected.as_ref() == Some(&from), "事项已切换，请重新选择");
            p.handoff(&from, &to.context("请选择接收事项")?, &note)
        }) && let Some(d) = self.board.desk.as_mut()
        {
            d.sent = Some("交接已保存，接收事项的下次阶段执行将读到摘要。".into());
            d.note.update(cx, |s, cx| s.set_value("", w, cx));
        }
    }
    pub(in crate::view) fn record_visible(&self, kind: RecordKind, id: &str) -> bool {
        self.context_work()
            .zip(self.workspace.as_ref())
            .is_none_or(|(w, ws)| w.has_record(&ws.root, kind, id))
    }
    pub(crate) fn attach_work_record(
        &mut self,
        kind: RecordKind,
        id: &str,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let Some(work) = self.context_work() else {
            return Ok(());
        };
        let wid = work.id.clone();
        let repo = self.workspace.as_ref().unwrap().root.clone();
        let success =
            self.change_board(cx, |l| l.project_mut()?.link_record(&wid, &repo, kind, id));
        ensure!(
            success,
            "{}",
            self.board.error.as_deref().unwrap_or("无法关联记录")
        );
        Ok(())
    }
    pub(in crate::view) fn record_picker(&self, kind: RecordKind, cx: &Context<Self>) -> Div {
        let mut view = v_flex().gap_1();
        if let Some(work) = self.context_work() {
            view = view
                .child(t(
                    format!(
                        "{} · {}",
                        work.name,
                        self.workspace
                            .as_ref()
                            .map_or(String::new(), |ws| basename(&ws.root))
                    ),
                    FG,
                ))
                .child(
                    button(
                        format!("pick-{}-record", kind.key()),
                        if self.board.pick_record {
                            "收起仓库记录"
                        } else {
                            "关联已有仓库记录…"
                        },
                        self.board.pick_record,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.board.pick_record = !this.board.pick_record;
                        cx.notify();
                    })),
                );
        }
        view
    }
    pub(in crate::view) fn record_choice(
        &self,
        kind: RecordKind,
        id: &str,
        name: &str,
        cx: &Context<Self>,
    ) -> Option<Div> {
        if !self.board.pick_record {
            return None;
        }
        let repo = &self.workspace.as_ref()?.root;
        if let Some(owner) = self
            .board
            .library
            .project()?
            .work
            .iter()
            .find(|work| work.has_record(repo, kind, id))
        {
            return Some(
                v_flex()
                    .gap_1()
                    .child(caption(name.to_owned()))
                    .child(caption(format!("已关联「{}」", owner.name))),
            );
        }
        let id = id.to_owned();
        Some(
            v_flex()
                .gap_1()
                .child(caption(format!("{name} · {id}")))
                .child(
                    button(format!("link-{id}"), "关联到当前事项", false).on_click(cx.listener(
                        move |this, _, w, cx| {
                            match this.attach_work_record(kind, &id, cx) {
                                Ok(()) => {
                                    this.board.pick_record = false;
                                    if kind == RecordKind::Relay {
                                        this.select_relay_record(&id, w, cx);
                                    } else if let Some(ws) = this.https_source() {
                                        this.select_https_session(
                                            &ws,
                                            ws.data.join("https-studio").join(&id),
                                        );
                                    }
                                }
                                Err(e) => {
                                    if kind == RecordKind::Relay {
                                        this.relay_error = Some(e.to_string());
                                    } else {
                                        this.https_pilot_error = Some(e.to_string());
                                    }
                                }
                            }
                            cx.notify();
                        },
                    )),
                ),
        )
    }
}
