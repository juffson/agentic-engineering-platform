use super::*;
use aep::memory::{Key, Kind, Memory, RevisionConflict};
use clap::ValueEnum;
use gpui_kit::component::input::{Textarea, TextareaState};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Project,
    Task,
    Candidate,
}
impl Scope {
    fn label(self) -> &'static str {
        match self {
            Self::Project => "仓库",
            Self::Task => "任务与 Spec",
            Self::Candidate => "候选提交",
        }
    }
}

pub(super) enum Start {
    Blank,
    FromRun(String),
    RelayDecision(String),
    Revise(i64),
    Resolve(i64),
}

pub(crate) struct Editor {
    run: Option<String>,
    replaces: Option<i64>,
    scope: Scope,
    kind: Kind,
    task: Entity<InputState>,
    spec: Entity<InputState>,
    candidate: Entity<InputState>,
    body: Entity<TextareaState>,
    error: Option<String>,
    saving: bool,
}

impl Studio {
    pub(super) fn compose(&mut self, start: Start, window: &mut Window, cx: &mut Context<Self>) {
        let run = match &start {
            Start::RelayDecision(id) => Some(id.clone()),
            _ => None,
        };
        let resolving = matches!(start, Start::Resolve(_));
        let (record, replaces, scope) = match start {
            Start::Blank => (&Value::Null, None, Scope::Project),
            Start::FromRun(id) | Start::RelayDecision(id) => {
                let Some(run) = self.snapshot.runs.iter().find(|r| r.id == id) else {
                    return;
                };
                (&run.record, None, Scope::Task)
            }
            Start::Revise(id) | Start::Resolve(id) => {
                let Some(note) = self
                    .snapshot
                    .notes
                    .iter()
                    .find(|n| n["id"] == id && n["successor"].is_null())
                else {
                    return;
                };
                let scope = match string(note, "scope") {
                    "candidate" => Scope::Candidate,
                    "task" => Scope::Task,
                    _ => Scope::Project,
                };
                (note, Some(id), scope)
            }
        };
        let mut input = |key: &str, placeholder: &'static str| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(string(record, key).to_string())
            })
        };
        let task = input("task", "如 market-volume-dedup-rust");
        let spec = input("spec_sha", "Spec 提交 SHA 或 Git 引用");
        let candidate = input("candidate_sha", "候选提交 SHA 或 Git 引用");
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("填写要共享的工程记录")
                .default_value(string(record, "body").to_owned())
        });
        body.focus_handle(cx).focus(window, cx);
        self.editor = Some(Editor {
            run,
            replaces,
            scope,
            kind: if resolving {
                Kind::Decision
            } else {
                Kind::from_str(string(record, "kind"), false).unwrap_or(Kind::Decision)
            },
            task,
            spec,
            candidate,
            body,
            error: None,
            saving: false,
        });
        cx.notify();
    }

    fn save_note(&mut self, window: &Window, cx: &mut Context<Self>) {
        let (Some(editor), Some(ws)) = (&mut self.editor, self.workspace.clone()) else {
            return;
        };
        if editor.saving {
            return;
        }
        let mut body = editor.body.read(cx).value().trim().to_owned();
        if self
            .lifecycle
            .as_ref()
            .is_some_and(|session| !session.real_task)
            && editor.kind == Kind::Decision
            && editor.replaces.is_some()
            && !body.is_empty()
        {
            let original = body
                .split_once(" Decision reference ")
                .map(|(original, _)| original)
                .unwrap_or(&body);
            body = format!(
                "{original} Decision reference decision-{}.",
                uuid::Uuid::new_v4().simple()
            );
        }
        if body.is_empty() || body.chars().count() > 1000 {
            editor.error = Some("正文需填写 1 至 1,000 个字符。".into());
            cx.notify();
            return;
        }
        let task = editor.task.read(cx).value().trim().to_owned();
        let spec = editor.spec.read(cx).value().trim().to_owned();
        let candidate = editor.candidate.read(cx).value().trim().to_owned();
        let scoped = editor.scope != Scope::Project;
        let pinned = editor.scope == Scope::Candidate;
        if editor.replaces.is_none()
            && ((scoped && (task.is_empty() || spec.is_empty()))
                || (pinned && candidate.is_empty()))
        {
            editor.error = Some("请填写所选范围的任务与提交。".into());
            cx.notify();
            return;
        }
        let kind = editor.kind;
        let replaces = editor.replaces;
        let run = editor.run.clone();
        editor.saving = true;
        editor.error = None;
        cx.notify();
        let save = cx.background_executor().spawn(async move {
            if let Some(id) = replaces {
                Memory::open(&ws.data)?.revise(id, kind, &body)
            } else {
                let key = Key::resolve(
                    &ws.root,
                    scoped.then_some(task.as_str()),
                    scoped.then_some(spec.as_str()),
                    pinned.then_some(candidate.as_str()),
                )?;
                Memory::open(&ws.data)?.add_manual(&key, kind.as_str(), &body, None, run.as_deref())
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = save.await;
            let _ = this.update_in(cx, |this, w, cx| match result {
                Ok(id) => {
                    this.editor = None;
                    this.note = Some(id);
                    if this.page != Page::Relay {
                        this.navigate(Page::Memory, w, cx);
                    }
                    // Supersede any snapshot that started before the transaction committed.
                    this.loading = false;
                    this.reload(cx);
                }
                Err(error) => {
                    if let Some(editor) = this.editor.as_mut() {
                        editor.saving = false;
                        editor.error = Some(if error.is::<RevisionConflict>() {
                            "这条记忆已有新版本。请先复制草稿，取消编辑后刷新，再打开当前版本修订。"
                                .to_owned()
                        } else {
                            format!("保存失败：{error:#}")
                        });
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

impl Editor {
    pub(super) fn render(&self, ws: &Workspace, cx: &Context<Studio>) -> Div {
        let title = self
            .replaces
            .map(|id| format!("修订记忆 #{id}"))
            .unwrap_or_else(|| "新建共享记忆".into());
        let mut form = panel()
            .gap_3()
            .child(heading(title).text_2xl())
            .child(caption(ws.root.display().to_string()));
        let mut scopes = h_flex().gap_2();
        for scope in [Scope::Project, Scope::Task, Scope::Candidate] {
            let active = self.scope == scope;
            if self.replaces.is_some() || self.run.is_some() {
                if active {
                    scopes = scopes.child(t(scope.label(), FG));
                }
            } else {
                scopes = scopes.child(button(scope.label(), scope.label(), active).on_click(
                    cx.listener(move |this, _, _, cx| {
                        if let Some(e) = this.editor.as_mut()
                            && !e.saving
                        {
                            e.scope = scope;
                            e.error = None;
                            cx.notify();
                        }
                    }),
                ));
            }
        }
        form = form.child(caption("共享范围")).child(scopes);
        if self.scope != Scope::Project {
            let mut pins = h_flex().items_start().gap_3();
            for (label, field) in [
                ("任务", &self.task),
                ("Spec 提交", &self.spec),
                ("候选提交", &self.candidate),
            ] {
                if label == "候选提交" && self.scope != Scope::Candidate {
                    continue;
                }
                pins = pins.child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(caption(label))
                        .child(
                            Input::new(field)
                                .aria_label(label)
                                .readonly(self.replaces.is_some() || self.run.is_some())
                                .disabled(self.saving),
                        ),
                );
            }
            form = form.child(pins);
        }
        let mut kinds = h_flex().gap_2();
        for &k in Kind::value_variants() {
            if self.run.is_some() && k != Kind::Decision {
                continue;
            }
            let label = kind(k.as_str());
            kinds = kinds.child(
                button(label.to_owned(), label.to_owned(), k == self.kind).on_click(cx.listener(
                    move |this, _, _, cx| {
                        if let Some(e) = this.editor.as_mut()
                            && !e.saving
                        {
                            e.kind = k;
                            cx.notify();
                        }
                    },
                )),
            );
        }
        form = form
            .child(caption("记忆类型"))
            .child(kinds)
            .child(caption("正文 · 最多 1,000 个字符"))
            .child(
                Textarea::new(&self.body)
                    .aria_label("记忆正文")
                    .h(px(116.))
                    .disabled(self.saving),
            );
        if let Some(err) = &self.error {
            form = form.child(
                div()
                    .id("save-error")
                    .test_support()
                    .role(Role::Alert)
                    .aria_label(err.clone())
                    .child(t(err.clone(), RED)),
            );
        }
        let controls = h_flex()
            .justify_end()
            .gap_2()
            .child(
                button("cancel-edit", "取消", false).on_click(cx.listener(|this, _, _, cx| {
                    if this.editor.as_ref().is_some_and(|e| !e.saving) {
                        this.editor = None;
                        cx.notify();
                    }
                })),
            )
            .child(
                button(
                    "save-note",
                    if self.saving {
                        "正在保存…"
                    } else {
                        "保存记忆"
                    },
                    true,
                )
                .on_click(cx.listener(|this, _, w, cx| this.save_note(w, cx))),
            );
        v_flex()
            .size_full()
            .bg(rgb(CANVAS))
            .text_color(rgb(FG))
            .child(
                v_flex()
                    .id("memory-editor")
                    .size_full()
                    .overflow_y_scroll()
                    .items_center()
                    .p_5()
                    .child(form.w_full().max_w(px(790.)).flex_none().child(controls)),
            )
    }
}
