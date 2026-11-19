use super::*;
use crate::{
    github_pr::{self, Check, Outcome, Snapshot},
    publish::Publication,
};
use std::sync::Arc;

struct Diagnostic {
    id: String,
    text: Option<String>,
}

pub(crate) struct State {
    reader: uuid::Uuid,
    publication: Publication,
    snapshot: Option<Arc<Snapshot>>,
    refresh: bool,
    merging: bool,
    confirmed: bool,
    discussion: bool,
    diagnostic: Option<Diagnostic>,
    error: Option<String>,
    merge_error: Option<String>,
}

impl Studio {
    pub(super) fn open_github_pr(
        &mut self,
        publication: Publication,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if publication.pr.is_none() {
            return;
        }
        if let Some(state) = self.pull_request.as_ref()
            && state.publication.target == publication.target
            && (state.merging || state.refresh)
        {
            self.navigate(Page::PullRequest, window, cx);
            return;
        }
        let reader = uuid::Uuid::new_v4();
        self.pull_request = Some(State {
            reader,
            publication,
            snapshot: None,
            refresh: false,
            merging: false,
            confirmed: false,
            discussion: false,
            diagnostic: None,
            error: None,
            merge_error: None,
        });
        self.navigate(Page::PullRequest, window, cx);
        self.refresh_github(cx);
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            loop {
                executor.timer(Duration::from_secs(12)).await;
                let active = this
                    .update(cx, |this, cx| {
                        let active = this.page == Page::PullRequest
                            && this.pull_request.as_ref().is_some_and(|state| {
                                state.reader == reader
                                    && state.snapshot.as_ref().is_none_or(|pr| pr.state == "OPEN")
                            });
                        if active {
                            this.refresh_github(cx);
                        }
                        active
                    })
                    .unwrap_or(false);
                if !active {
                    break;
                }
            }
        })
        .detach();
    }

    fn refresh_github(&mut self, cx: &mut Context<Self>) {
        let (Some(ws), Some(state)) = (self.workspace.clone(), self.pull_request.as_mut()) else {
            return;
        };
        if state.refresh || state.merging {
            return;
        }
        let Some(pr) = &state.publication.pr else {
            return;
        };
        let number = pr.number;
        let target = state.publication.target.clone();
        let runner = self.runner.clone();
        let reader = state.reader;
        state.refresh = true;
        let task = cx
            .background_executor()
            .spawn(async move { github_pr::load(&ws, &runner, &target, number) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                let Some(state) = this
                    .pull_request
                    .as_mut()
                    .filter(|state| state.reader == reader)
                else {
                    return;
                };
                state.refresh = false;
                match result {
                    Ok(snapshot) => {
                        if !state
                            .snapshot
                            .as_ref()
                            .is_some_and(|previous| snapshot.matches_confirmation(previous))
                        {
                            state.confirmed = false;
                        }
                        state.snapshot = Some(Arc::new(snapshot));
                        state.error = None;
                    }
                    Err(error) => {
                        state.error = Some(format!("读取 GitHub 失败：{error:#}"));
                        state.confirmed = false;
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn show_check(&mut self, check: Check, cx: &mut Context<Self>) {
        let (Some(ws), Some(state)) = (self.workspace.clone(), self.pull_request.as_mut()) else {
            return;
        };
        if state
            .diagnostic
            .as_ref()
            .is_some_and(|current| current.id == check.id)
        {
            state.diagnostic = None;
            cx.notify();
            return;
        }
        let id = check.id.clone();
        let reader = state.reader;
        let target = state.publication.target.clone();
        let runner = self.runner.clone();
        state.diagnostic = Some(Diagnostic {
            id: id.clone(),
            text: None,
        });
        let work = cx
            .background_executor()
            .spawn(async move { github_pr::check_details(&ws, &runner, &target, &check) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(state) = this
                    .pull_request
                    .as_mut()
                    .filter(|state| state.reader == reader)
                    && let Some(diagnostic) = state
                        .diagnostic
                        .as_mut()
                        .filter(|diagnostic| diagnostic.id == id)
                {
                    diagnostic.text = Some(match result {
                        Ok(text) if text.trim().is_empty() => {
                            "GitHub 未提供详细原因。可打开该检查查看完整日志。".into()
                        }
                        Ok(text) => text,
                        Err(error) => format!("读取详情失败：{error:#}"),
                    });
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn merge_github_pr(&mut self, cx: &mut Context<Self>) {
        let (Some(ws), Some(state)) = (self.workspace.clone(), self.pull_request.as_mut()) else {
            return;
        };
        let Some(snapshot) = state.snapshot.as_ref() else {
            return;
        };
        if state.merging
            || state.refresh
            || !state.confirmed
            || state.error.is_some()
            || state.merge_error.is_some()
            || snapshot.blocker(&state.publication.target).is_some()
        {
            return;
        }
        state.merging = true;
        state.confirmed = false;
        let reader = state.reader;
        let runner = self.runner.clone();
        let publication = state.publication.clone();
        let snapshot = Arc::clone(snapshot);
        let task = cx
            .background_executor()
            .spawn(async move { github_pr::merge(&ws, &runner, &publication, &snapshot) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                let Some(state) = this
                    .pull_request
                    .as_mut()
                    .filter(|state| state.reader == reader)
                else {
                    return;
                };
                state.merging = false;
                match result {
                    Ok(snapshot) => {
                        state.snapshot = Some(Arc::new(snapshot));
                        this.reload(cx);
                    }
                    Err(error) => state.merge_error = Some(format!("合并失败：{error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn github_pr_page(&self, cx: &Context<Self>) -> Div {
        let mut main = v_flex().size_full().min_h_0().bg(rgb(CANVAS));
        let Some(state) = &self.pull_request else {
            return main;
        };
        let publication = &state.publication;
        let Some(pr) = &publication.pr else {
            return main;
        };
        let number = pr.number;
        let url = pr.url(&publication.target);
        main = main.child(
            h_flex()
                .justify_between()
                .px_5()
                .py_3()
                .gap_2()
                .border_b_1()
                .border_color(rgb(LINE))
                .child(heading(format!("GitHub PR #{number}")).text_2xl())
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            button(
                                "refresh-github-pr",
                                if state.refresh { "更新中" } else { "刷新" },
                                false,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(state) = &mut this.pull_request {
                                    state.merge_error = None;
                                }
                                this.refresh_github(cx);
                            })),
                        )
                        .child(
                            button("open-github-pr", "在 GitHub 打开 ↗", false)
                                .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url))),
                        )
                        .child(button("github-back-to-relay", "返回接力", false).on_click(
                            cx.listener(|this, _, window, cx| {
                                this.navigate(Page::Relay, window, cx)
                            }),
                        )),
                ),
        );
        if let Some(error) = state.merge_error.as_ref().or(state.error.as_ref()) {
            main = main.child(
                detail_text("github-pr-error", error)
                    .text_color(rgb(RED))
                    .px_5()
                    .py_2(),
            );
        }
        let Some(snapshot) = state.snapshot.as_deref() else {
            return main.child(
                caption(if state.refresh {
                    "正在读取检查和审查"
                } else {
                    "暂无 GitHub 数据"
                })
                .p_5(),
            );
        };
        let mut tabs = h_flex().gap_2();
        for (discussion, id, label) in [
            (false, "github-ci-tab", "CI 检查"),
            (true, "github-review-tab", "审查意见"),
        ] {
            tabs = tabs.child(button(id, label, state.discussion == discussion).on_click(
                cx.listener(move |this, _, _, cx| {
                    if let Some(state) = &mut this.pull_request {
                        state.discussion = discussion;
                        cx.notify();
                    }
                }),
            ));
        }
        let contents = if state.discussion {
            github_notes(snapshot, cx)
        } else {
            self.github_checks(state, snapshot, cx)
        };
        main.child(
            h_flex()
                .flex_1()
                .min_h_0()
                .items_start()
                .child(
                    v_flex()
                        .h_full()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .p_3()
                        .gap_2()
                        .child(heading(snapshot.title.clone()).text_lg())
                        .child(tabs)
                        .child(contents),
                )
                .child(self.github_merge_card(state, snapshot, cx)),
        )
    }

    fn github_checks(&self, state: &State, snapshot: &Snapshot, cx: &Context<Self>) -> AnyElement {
        let mut list = v_flex()
            .id("github-check-list")
            .test_support()
            .overflow_y_scroll()
            .flex_1()
            .min_h_0()
            .gap_2();
        if snapshot.checks.is_empty() {
            list = list.child(panel().child(caption("GitHub 尚未报告检查。")));
        }
        for (index, check) in snapshot.checks.iter().enumerate() {
            let (label, color) = match check.outcome {
                Outcome::Pending => (
                    if check.status == "IN_PROGRESS" {
                        "运行中"
                    } else {
                        "排队中"
                    },
                    AMBER,
                ),
                Outcome::Passed => ("已通过", MINT),
                Outcome::Skipped => ("已跳过 / 中立", MUTED),
                Outcome::Failed => ("失败", RED),
            };
            let mut row = panel().gap_2().child(
                h_flex()
                    .justify_between()
                    .items_start()
                    .gap_2()
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(t(check.name.clone(), FG))
                            .when(!check.workflow.is_empty(), |header| {
                                header.child(caption(check.workflow.clone()))
                            }),
                    )
                    .child(chip(label, color)),
            );
            if check.outcome == Outcome::Failed {
                let item = check.clone();
                row = row.child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .child(caption(check.conclusion.clone()))
                        .child(
                            button(format!("github-failure-{index}"), "查看失败原因", false)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.show_check(item.clone(), cx)
                                })),
                        ),
                );
            }
            if !check.description.is_empty() {
                row = row.child(caption(check.description.clone()));
            }
            if let Some(diagnostic) = state
                .diagnostic
                .as_ref()
                .filter(|diagnostic| diagnostic.id == check.id)
            {
                row = row.child(
                    detail_text(
                        "github-check-diagnostic",
                        diagnostic.text.as_deref().unwrap_or("正在读取检查详情"),
                    )
                    .font_family("Menlo")
                    .text_xs(),
                );
            }
            if check.url.starts_with("https://") {
                let url = check.url.clone();
                row = row.child(
                    h_flex().child(
                        button(format!("open-check-{index}"), "打开检查 ↗", false)
                            .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url))),
                    ),
                );
            }
            list = list.child(row);
        }
        list.into_any_element()
    }

    fn github_merge_card(
        &self,
        state: &State,
        snapshot: &Snapshot,
        cx: &Context<Self>,
    ) -> AnyElement {
        let target = &state.publication.target;
        let mut side = v_flex()
            .id("github-merge-card")
            .test_support()
            .overflow_y_scroll()
            .w(px(294.))
            .h_full()
            .min_h_0()
            .flex_none()
            .p_3()
            .gap_3()
            .border_l_1()
            .border_color(rgb(LINE))
            .child(heading("合并"))
            .child(field("目标分支", &snapshot.base_ref))
            .child(field("目标 SHA", &snapshot.base))
            .child(field("PR head", &snapshot.head))
            .child(field("GitHub 状态", &snapshot.merge_state));
        if !snapshot.review_decision.is_empty() {
            side = side.child(field("审查结果", review_label(&snapshot.review_decision)));
        }
        if snapshot.state == "MERGED" {
            side = side
                .child(detail_text("github-merged", "GitHub 已合并").text_color(rgb(MINT)))
                .child(field("合并提交", &snapshot.merged_sha))
                .child(field("合并账号", &snapshot.merged_by))
                .child(field("合并时间", &snapshot.merged_at));
            if let Some(receipt) = &snapshot.receipt {
                side = side.child(caption(format!(
                    "Studio 确认账号 · {}",
                    receipt.confirmed_by
                )));
            }
        } else if state.merging {
            side = side.child(chip("正在提交合并请求", MINT));
        } else if let Some(reason) = snapshot.blocker(target) {
            side = side.child(detail_text("github-merge-blocker", reason).text_color(rgb(AMBER)));
        } else if state.error.is_none() && state.merge_error.is_none() {
            side = side
                .child(field(
                    "合并方式",
                    match snapshot.method {
                        Some("squash") => "Squash",
                        Some("rebase") => "Rebase",
                        _ => "Merge commit",
                    },
                ))
                .child(
                    button(
                        "github-confirm-merge",
                        if state.confirmed {
                            "☑ 已核对当前检查和审查意见"
                        } else {
                            "☐ 我已核对当前检查和审查意见"
                        },
                        state.confirmed,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(state) = &mut this.pull_request
                            && !state.refresh
                        {
                            state.confirmed = !state.confirmed;
                            cx.notify();
                        }
                    })),
                );
            if state.confirmed && !state.refresh {
                side = side.child(
                    button("github-merge-now", "确认合并", true)
                        .on_click(cx.listener(|this, _, _, cx| this.merge_github_pr(cx))),
                );
            }
        }
        side.into_any_element()
    }
}

fn review_label(kind: &str) -> &str {
    match kind {
        "APPROVED" => "已同意",
        "CHANGES_REQUESTED" => "要求修改",
        "REVIEW_REQUIRED" => "等待审查",
        "DISMISSED" => "已撤销",
        "INLINE" => "行内意见",
        "RESOLVED" => "已解决的行内意见",
        "COMMENTED" => "评论",
        other => other,
    }
}

fn github_notes(snapshot: &Snapshot, cx: &Context<Studio>) -> AnyElement {
    let mut list = v_flex()
        .id("github-discussion")
        .overflow_y_scroll()
        .flex_1()
        .min_h_0()
        .gap_2();
    if snapshot.more_notes {
        list = list.child(caption("此处显示近期审查意见，完整讨论可在 GitHub 查看。"));
    }
    if snapshot.notes.is_empty() {
        list = list.child(panel().child(caption("暂无公开的审查意见。")));
    }
    for (index, note) in snapshot.notes.iter().enumerate() {
        let mut row = panel().gap_2().child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .child(t(
                    if note.author.is_empty() {
                        "GitHub 用户".into()
                    } else {
                        note.author.clone()
                    },
                    FG,
                ))
                .child(caption(review_label(&note.kind)))
                .child(caption(note.date.clone())),
        );
        if let Some(location) = &note.location {
            row = row.child(caption(location.clone()));
        }
        if !note.body.is_empty() {
            row = row.child(detail_text(
                SharedString::from(format!("github-note-{index}")),
                &note.body,
            ));
        }
        if note.url.starts_with("https://github.com/") {
            let url = note.url.clone();
            row = row.child(
                h_flex().child(
                    button(
                        format!("github-open-note-{index}"),
                        "在 GitHub 查看 ↗",
                        false,
                    )
                    .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url))),
                ),
            );
        }
        list = list.child(row);
    }
    list.into_any_element()
}
