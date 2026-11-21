use super::*;
use crate::lifecycle;
use aep::task::{Check, TaskManifest};
use anyhow::Context as _;
use gpui_kit::component::input::{Textarea, TextareaState};

pub(crate) struct Authoring {
    pub(super) branch: lifecycle::Branch,
    fields: DraftFields,
}

pub(super) struct DraftFields {
    locked_name: bool,
    task: Entity<InputState>,
    title: Entity<InputState>,
    spec: Entity<TextareaState>,
    paths: Entity<TextareaState>,
    check: Entity<TextareaState>,
}

impl DraftFields {
    pub(super) fn new(
        initial: Option<&lifecycle::Draft>,
        window: &mut Window,
        cx: &mut Context<Studio>,
    ) -> Self {
        let mut fields = Self {
            locked_name: initial.is_some(),
            task: cx.new(|cx| InputState::new(window, cx).placeholder("如 improve-task-errors")),
            title: cx.new(|cx| InputState::new(window, cx)),
            spec: cx
                .new(|cx| TextareaState::new(window, cx).placeholder("描述范围、要求和验收标准")),
            paths: cx
                .new(|cx| TextareaState::new(window, cx).placeholder("每行一个仓库相对文件路径")),
            check: cx.new(|cx| {
                TextareaState::new(window, cx).placeholder("[\"cargo\",\"test\",\"--locked\"]")
            }),
        };
        if let Some(draft) = initial {
            fields.set_draft(draft, window, cx);
        }
        if fields.locked_name {
            fields.title.focus_handle(cx).focus(window, cx);
        } else {
            fields.task.focus_handle(cx).focus(window, cx);
        }
        fields
    }

    fn set_draft(
        &mut self,
        draft: &lifecycle::Draft,
        window: &mut Window,
        cx: &mut Context<Studio>,
    ) {
        let manifest = &draft.manifest;
        let acceptance = serde_json::json!({
            "check": manifest.check,
            "criteria": manifest.criteria,
            "checks": manifest.checks,
        });
        self.task
            .update(cx, |field, cx| field.set_value(&draft.name, window, cx));
        self.title.update(cx, |field, cx| {
            field.set_value(manifest.title.as_deref().unwrap_or(""), window, cx)
        });
        self.spec
            .update(cx, |field, cx| field.set_value(&draft.spec, window, cx));
        self.paths.update(cx, |field, cx| {
            field.set_value(manifest.allowed_paths.join("\n"), window, cx)
        });
        self.check.update(cx, |field, cx| {
            field.set_value(acceptance.to_string(), window, cx)
        });
    }

    pub(super) fn draft(&self, cx: &App) -> Result<lifecycle::Draft> {
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum Acceptance {
            Command(Vec<String>),
            Plan {
                check: Vec<String>,
                #[serde(default)]
                criteria: Vec<String>,
                #[serde(default)]
                checks: Vec<Check>,
            },
        }
        let (check, criteria, checks) =
            match serde_json::from_str::<Acceptance>(&self.check.read(cx).value())
                .context("请填写验收命令数组或包含 check、criteria 和 checks 的验收对象")?
            {
                Acceptance::Command(check) => (check, Vec::new(), Vec::new()),
                Acceptance::Plan {
                    check,
                    criteria,
                    checks,
                } => (check, criteria, checks),
            };
        let title = self.title.read(cx).value().trim().to_owned();
        let draft = lifecycle::Draft {
            name: self.task.read(cx).value().trim().to_owned(),
            spec: self.spec.read(cx).value().to_string(),
            manifest: TaskManifest {
                title: (!title.is_empty()).then_some(title),
                allowed_paths: self
                    .paths
                    .read(cx)
                    .value()
                    .lines()
                    .map(str::trim)
                    .filter(|path| !path.is_empty())
                    .map(str::to_owned)
                    .collect(),
                check,
                criteria,
                checks,
            },
        };
        draft.validate()?;
        Ok(draft)
    }

    pub(super) fn render(&self, preparing: bool, require_title: bool) -> Div {
        let text_field = |id, label, field, height| {
            v_flex().gap_1().child(caption(label)).child(
                div().id(id).test_support().child(
                    Textarea::new(field)
                        .aria_label(label)
                        .h(px(height))
                        .disabled(preparing),
                ),
            )
        };
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        v_flex()
                            .gap_1()
                            .flex_1()
                            .min_w_0()
                            .child(caption("任务 ID"))
                            .child(
                                div().id("task-author-name").test_support().child(
                                    Input::new(&self.task)
                                        .aria_label("任务 ID")
                                        .disabled(preparing || self.locked_name),
                                ),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .flex_1()
                            .min_w_0()
                            .child(caption(if require_title {
                                "标题"
                            } else {
                                "标题（可选）"
                            }))
                            .child(
                                div().id("task-author-title").test_support().child(
                                    Input::new(&self.title)
                                        .aria_label("任务标题")
                                        .disabled(preparing),
                                ),
                            ),
                    ),
            )
            .child(text_field(
                "task-author-spec",
                "任务 Spec",
                &self.spec,
                155.,
            ))
            .child(text_field(
                "task-author-paths",
                "允许修改的文件",
                &self.paths,
                68.,
            ))
            .child(text_field(
                "task-author-check",
                "验收命令",
                &self.check,
                64.,
            ))
            .child(caption(
                "填写 JSON 命令数组，或填写带验收项编号和多个检查的 JSON 对象。",
            ))
    }
}

impl Authoring {
    pub(super) fn new(
        branch: lifecycle::Branch,
        window: &mut Window,
        cx: &mut Context<Studio>,
    ) -> Self {
        Self {
            branch,
            fields: DraftFields::new(None, window, cx),
        }
    }

    pub(super) fn draft(&self, cx: &App) -> Result<lifecycle::Draft> {
        self.fields.draft(cx)
    }

    pub(super) fn fields(&self, ws: &Workspace, preparing: bool) -> Div {
        panel()
            .child(field("仓库", &ws.root.display().to_string()))
            .child(field("目标分支", &self.branch.name))
            .child(field("目标提交", &self.branch.sha))
            .child(self.fields.render(preparing, false))
    }

    pub(super) fn render(&self, ws: &Workspace, preparing: bool, cx: &Context<Studio>) -> Div {
        let form = self.fields(ws, preparing);
        if preparing {
            return form.child(caption("正在准备指定提交上的任务…"));
        }
        form.child(
            h_flex()
                .gap_2()
                .child(
                    button("prepare-task-draft", "准备派发", true)
                        .on_click(cx.listener(|this, _, w, cx| this.prepare_task_draft(w, cx))),
                )
                .child(
                    button("cancel-task-draft", "返回任务列表", false).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.task_authoring = None;
                            this.lifecycle_error = None;
                            cx.notify();
                        },
                    )),
                ),
        )
    }
}

impl Studio {
    pub(super) fn open_task_authoring(
        &mut self,
        branch: lifecycle::Branch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.lifecycle_preparing || self.lifecycle.is_some() {
            return;
        }
        self.task_authoring = Some(Authoring::new(branch, window, cx));
        self.lifecycle_error = None;
        cx.notify();
    }

    fn prepare_task_draft(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(authoring) = &self.task_authoring else {
            return;
        };
        let branch = authoring.branch.clone();
        match authoring.draft(cx) {
            Ok(draft) => self.start_lifecycle(lifecycle::Plan::New(draft, branch), window, cx),
            Err(error) => {
                self.lifecycle_error = Some(format!("{error:#}"));
                cx.notify();
            }
        }
    }
}
