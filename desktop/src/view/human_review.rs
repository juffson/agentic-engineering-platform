use super::*;
use crate::publish::{self, Review};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Diff,
    Checks,
    Report,
}

fn diff_tone(line: &str) -> u32 {
    if line.starts_with("diff --git")
        || line.starts_with("@@")
        || line.starts_with("--- ")
        || line.starts_with("+++ ")
    {
        BLUE
    } else if line.starts_with('+') {
        MINT
    } else if line.starts_with('-') {
        RED
    } else {
        FG
    }
}

pub(crate) struct State {
    relay: String,
    run: String,
    review: Option<Review>,
    tab: Tab,
    page: usize,
    confirmed: bool,
    loading: bool,
    sending: bool,
    error: Option<String>,
}

impl Studio {
    pub(super) fn open_human_review(
        &mut self,
        relay: String,
        run: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .review
            .as_ref()
            .is_some_and(|state| state.run == run && (state.sending || state.loading))
        {
            self.navigate(Page::Review, window, cx);
            return;
        }
        self.review = Some(State {
            relay,
            run,
            review: None,
            tab: Tab::Diff,
            page: 0,
            confirmed: false,
            loading: false,
            sending: false,
            error: None,
        });
        self.navigate(Page::Review, window, cx);
        self.load_human_review(cx);
    }

    fn load_human_review(&mut self, cx: &mut Context<Self>) {
        let (Some(ws), Some(state)) = (self.workspace.clone(), self.review.as_mut()) else {
            return;
        };
        if state.loading || state.sending {
            return;
        }
        state.loading = true;
        state.confirmed = false;
        state.error = None;
        state.review = None;
        state.page = 0;
        state.tab = Tab::Diff;
        let relay = state.relay.clone();
        let run = state.run.clone();
        let runner = self.runner.clone();
        let generation = self.workspace_generation;
        let requested_run = run.clone();
        let task = cx
            .background_executor()
            .spawn(async move { publish::prepare(&ws, &runner, &relay, &run) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.workspace_generation != generation {
                    return;
                }
                let Some(state) = this
                    .review
                    .as_mut()
                    .filter(|state| state.run == requested_run)
                else {
                    return;
                };
                state.loading = false;
                match result {
                    Ok(review) => state.review = Some(review),
                    Err(error) => state.error = Some(format!("{error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn publish_review(&mut self, cx: &mut Context<Self>) {
        let (Some(ws), Some(state)) = (self.workspace.clone(), self.review.as_mut()) else {
            return;
        };
        let Some(review) = &state.review else { return };
        if state.sending
            || !state.confirmed
            || review
                .publication
                .as_ref()
                .is_some_and(|saved| saved.pr.is_some())
        {
            return;
        }
        state.sending = true;
        state.error = None;
        let runner = self.runner.clone();
        let target = review.target.clone();
        let requested_run = target.run.clone();
        let generation = self.workspace_generation;
        let job = cx
            .background_executor()
            .spawn(async move { publish::publish(&ws, &runner, target) });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |this, cx| {
                if this.workspace_generation != generation {
                    return;
                }
                let Some(state) = this
                    .review
                    .as_mut()
                    .filter(|state| state.run == requested_run)
                else {
                    return;
                };
                state.sending = false;
                match result {
                    Ok(publication) => {
                        if let Some(review) = &mut state.review {
                            review.publication = Some(publication);
                        }
                        this.reload(cx);
                    }
                    Err(error) => state.error = Some(format!("{error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn human_review_page(&self, cx: &Context<Self>) -> Div {
        let mut main = v_flex().size_full().min_w_0().min_h_0().bg(rgb(CANVAS));
        let header =
            h_flex()
                .justify_between()
                .gap_2()
                .px_5()
                .py_3()
                .border_b_1()
                .border_color(rgb(LINE))
                .child(heading("最终人工审查").text_2xl())
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            button("refresh-human-review", "重新加载", false)
                                .on_click(cx.listener(|this, _, _, cx| this.load_human_review(cx))),
                        )
                        .child(button("back-to-relay", "返回接力", false).on_click(
                            cx.listener(|this, _, w, cx| this.navigate(Page::Relay, w, cx)),
                        )),
                );
        main = main.child(header);
        let Some(state) = &self.review else {
            return main;
        };
        if let Some(error) = &state.error {
            main = main.child(
                detail_text("publish-error", error)
                    .px_5()
                    .py_2()
                    .text_color(rgb(RED)),
            );
        }
        let Some(review) = &state.review else {
            return main.child(
                caption(if state.loading {
                    "正在读取 GitHub 目标版本与运行证据"
                } else {
                    "审查数据尚未加载"
                })
                .px_5()
                .py_3(),
            );
        };
        let mut tab_bar = h_flex().gap_2().flex_wrap();
        for (tab, id, label) in [
            (Tab::Diff, "review-diff", "最终 Diff"),
            (Tab::Checks, "review-checks", "验收日志"),
            (Tab::Report, "review-report", "审查报告"),
        ] {
            tab_bar = tab_bar.child(button(id, label, state.tab == tab).on_click(cx.listener(
                move |this, _, _, cx| {
                    if let Some(state) = &mut this.review {
                        state.tab = tab;
                        state.page = 0;
                        cx.notify();
                    }
                },
            )));
        }
        let pages = match state.tab {
            Tab::Diff => &review.diff,
            Tab::Checks => &review.checks,
            Tab::Report => &review.report,
        };
        let mut contents = v_flex()
            .h_full()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .gap_2()
            .p_3()
            .child(tab_bar);
        if pages.len() > 1 {
            let mut page_bar = h_flex().gap_2().child(caption(format!(
                "第 {} / {} 页",
                state.page + 1,
                pages.len()
            )));
            for (id, text, page) in [
                ("review-prev", "上一页", state.page.checked_sub(1)),
                (
                    "review-next",
                    "下一页",
                    (state.page + 1 < pages.len()).then_some(state.page + 1),
                ),
            ] {
                if let Some(page) = page {
                    page_bar = page_bar.child(button(id, text, false).on_click(cx.listener(
                        move |this, _, _, cx| {
                            if let Some(state) = &mut this.review {
                                state.page = page;
                                cx.notify();
                            }
                        },
                    )));
                }
            }
            contents = contents.child(page_bar);
        }
        let body = pages
            .get(state.page)
            .filter(|text| !text.trim().is_empty())
            .map_or("文件为空", String::as_str);
        let text = if state.tab == Tab::Diff {
            v_flex()
                .id("review-content")
                .role(Role::Label)
                .aria_label(SharedString::from(body.to_owned()))
                .test_support()
                .font_family("Menlo")
                .children(body.lines().map(|line| {
                    t(
                        if line.is_empty() {
                            " ".to_owned()
                        } else {
                            line.to_owned()
                        },
                        diff_tone(line),
                    )
                    .text_xs()
                }))
                .into_any_element()
        } else {
            detail_text("review-content", body)
                .font_family("Menlo")
                .text_xs()
                .into_any_element()
        };
        contents = contents.child(
            v_flex()
                .id(("review-document", state.page * 3 + state.tab as usize))
                .overflow_y_scroll()
                .flex_1()
                .min_h_0()
                .bg(rgb(PANEL))
                .rounded_md()
                .p_3()
                .child(text),
        );

        let target = &review.target;
        let mut confirm = v_flex()
            .id("review-destination")
            .overflow_y_scroll()
            .h_full()
            .min_h_0()
            .w(px(290.))
            .flex_none()
            .border_l_1()
            .border_color(rgb(LINE))
            .p_3()
            .gap_3()
            .child(heading("发布到 GitHub"))
            .child(field("仓库", &target.repo))
            .child(field("目标分支", &target.base))
            .child(field("目标 SHA", &target.base_sha))
            .child(field("候选 SHA", &target.candidate))
            .child(field("候选分支", &target.head));
        if let Some(publication) = &review.publication {
            confirm = confirm.child(caption(format!("确认账号 · {}", publication.confirmed_by)));
            if publication.pushed {
                confirm = confirm.child(chip("候选已推送", MINT));
            }
            if let Some(error) = &publication.error {
                confirm =
                    confirm.child(detail_text("publish-receipt-error", error).text_color(rgb(RED)));
            }
        }
        if let Some(pr) = review
            .publication
            .as_ref()
            .and_then(|saved| saved.pr.as_ref())
        {
            let url = pr.url(target);
            confirm = confirm.child(
                button(
                    "open-created-pr",
                    format!("打开 GitHub PR #{} ↗", pr.number),
                    true,
                )
                .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url))),
            );
        } else if state.sending {
            confirm = confirm.child(chip("正在推送候选并创建 PR", MINT));
        } else {
            confirm = confirm.child(
                button(
                    "acknowledge-review",
                    if state.confirmed {
                        "☑ 已核对 Diff、验收和审查报告"
                    } else {
                        "☐ 我已核对 Diff、验收和审查报告"
                    },
                    state.confirmed,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(state) = &mut this.review {
                        state.confirmed = !state.confirmed;
                        cx.notify();
                    }
                })),
            );
            if state.confirmed {
                let retry = review
                    .publication
                    .as_ref()
                    .is_some_and(|saved| saved.pushed);
                confirm = confirm.child(
                    button(
                        "publish-confirmed",
                        if retry {
                            "重试创建 GitHub PR"
                        } else {
                            "确认并创建 GitHub PR"
                        },
                        true,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.publish_review(cx))),
                );
            }
        }
        main.child(
            h_flex()
                .items_start()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .child(contents)
                .child(confirm),
        )
    }
}
