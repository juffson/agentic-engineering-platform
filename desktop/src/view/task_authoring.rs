use super::*;
use crate::lifecycle;
use aep::task::TaskManifest;
use anyhow::Context as _;
use gpui_kit::component::input::{Textarea, TextareaState};

pub(crate) struct Authoring {
    branch: lifecycle::Branch,
    task: Entity<InputState>,
    title: Entity<InputState>,
    spec: Entity<TextareaState>,
    paths: Entity<TextareaState>,
    check: Entity<TextareaState>,
}

impl Authoring {
    fn draft(&self, cx: &App) -> Result<lifecycle::Draft> {
        let check = serde_json::from_str::<Vec<String>>(&self.check.read(cx).value())
            .context("验收命令必须是 JSON 字符串数组，例如 [\"cargo\",\"test\",\"--locked\"]")?;
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
            },
        };
        draft.validate()?;
        Ok(draft)
    }

    pub(super) fn render(&self, ws: &Workspace, preparing: bool, cx: &Context<Studio>) -> Div {
        let text_field = |label, field, height| {
            v_flex().gap_1().child(caption(label)).child(
                Textarea::new(field)
                    .aria_label(label)
                    .h(px(height))
                    .disabled(preparing),
            )
        };
        let mut form = panel()
            .child(field("仓库", &ws.root.display().to_string()))
            .child(field("目标分支", &self.branch.name))
            .child(field("目标提交", &self.branch.sha))
            .child(h_flex().gap_3()
                .child(v_flex().gap_1().flex_1().min_w_0()
                    .child(caption("任务 ID"))
                    .child(Input::new(&self.task).aria_label("任务 ID").disabled(preparing)))
                .child(v_flex().gap_1().flex_1().min_w_0()
                    .child(caption("标题（可选）"))
                    .child(Input::new(&self.title).aria_label("任务标题").disabled(preparing))))
            .child(text_field("任务 Spec", &self.spec, 155.))
            .child(text_field("允许修改的文件", &self.paths, 68.))
            .child(text_field("验收命令", &self.check, 64.))
            .child(caption("验收命令用 JSON 数组填写程序名和各个参数，例如 [\"cargo\", \"test\", \"--locked\"]。"));
        if preparing {
            return form.child(caption("正在准备指定提交上的任务…"));
        }
        form =
            form.child(
                h_flex()
                    .gap_2()
                    .child(
                        button("prepare-task-draft", "准备派发", true)
                            .on_click(cx.listener(|this, _, w, cx| this.prepare_task_draft(w, cx))),
                    )
                    .child(button("cancel-task-draft", "返回任务列表", false).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.task_authoring = None;
                            this.lifecycle_error = None;
                            cx.notify();
                        }),
                    )),
            );
        form
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
        let task = cx.new(|cx| InputState::new(window, cx).placeholder("如 improve-task-errors"));
        task.focus_handle(cx).focus(window, cx);
        self.task_authoring = Some(Authoring {
            branch,
            task,
            title: cx.new(|cx| InputState::new(window, cx)),
            spec: cx
                .new(|cx| TextareaState::new(window, cx).placeholder("描述范围、要求和验收标准")),
            paths: cx
                .new(|cx| TextareaState::new(window, cx).placeholder("每行一个仓库相对文件路径")),
            check: cx.new(|cx| {
                TextareaState::new(window, cx).placeholder("[\"cargo\",\"test\",\"--locked\"]")
            }),
        });
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
