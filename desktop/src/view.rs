pub(crate) mod editor;
pub(crate) mod github_page;
pub(crate) mod human_review;
mod lifecycle_page;
mod relay_page;
mod repository_tasks;
pub(crate) mod task_authoring;
use super::*;
use aep::studio::Run;
use editor::Start;
use gpui_kit::base::ObservedElement;
use gpui_kit::component::{Icon, IconName, Sizable, StyledExt, h_flex, input::Input, v_flex};
use gpui_kit::prelude::FluentBuilder;

const CANVAS: u32 = 0x121820;
const RAIL: u32 = 0x10151c;
const PANEL: u32 = 0x19212b;
const SELECTED: u32 = 0x213038;
const LINE: u32 = 0x303b46;
const FG: u32 = 0xe6edf0;
const MUTED: u32 = 0xa1b0ba;
const MINT: u32 = 0x95dfc4;
const AMBER: u32 = 0xe8c080;
const RED: u32 = 0xf29b9c;
const BLUE: u32 = 0x9bbff4;
const AGENT_ROLES: [&str; 2] = ["implementer", "reviewer"];

fn t(s: impl Into<SharedString>, color: u32) -> Div {
    div().text_sm().text_color(rgb(color)).child(s.into())
}
fn detail_text(id: impl Into<ElementId>, text: &str) -> ObservedElement<Stateful<Div>> {
    let text = SharedString::from(text.to_owned());
    t(text.clone(), FG)
        .id(id)
        .role(Role::Label)
        .aria_label(text)
        .test_support()
}
fn caption(s: impl Into<SharedString>) -> Div {
    t(s, MUTED).text_xs()
}
fn heading(s: impl Into<SharedString>) -> Div {
    t(s, FG).font_semibold().text_lg()
}
fn icon(name: IconName) -> Icon {
    Icon::new(name).small()
}
fn status(s: &str) -> (&str, u32) {
    match s {
        "awaiting_human_review" => ("待人工审查", AMBER),
        "failed" => ("失败", RED),
        "needs_reconciliation" => ("需核对", RED),
        "unstarted" => ("未启动", AMBER),
        "running" => ("运行中", MINT),
        "initializing" => ("初始化", BLUE),
        "" => ("未知", MUTED),
        other => (other, MUTED),
    }
}
fn author(s: &str) -> &str {
    match s {
        "implementer" => "实现 Agent",
        "reviewer" => "审查 Agent",
        "manual" => "人工",
        s => s,
    }
}
fn kind(s: &str) -> &str {
    match s {
        "handoff" => "交接",
        "review" => "审查",
        "open_question" => "未决问题",
        "decision" => "决策",
        s => s,
    }
}
fn scope(s: &str) -> &str {
    match s {
        "project" => "仓库",
        "candidate" => "候选提交",
        "task" => "任务",
        s => s,
    }
}
fn date(s: &str) -> String {
    if let Some((date, time)) = s.split_once('T') {
        format!("{} {}", date, time.get(..8).unwrap_or(time))
    } else {
        s.to_owned()
    }
}
fn chip(s: impl Into<SharedString>, tone: u32) -> Div {
    h_flex()
        .flex_none()
        .gap_1p5()
        .child(div().size(px(6.)).rounded_full().bg(rgb(tone)))
        .child(t(s, tone).text_xs())
}
fn panel() -> Div {
    v_flex()
        .gap_3()
        .bg(rgb(PANEL))
        .border_1()
        .border_color(rgb(LINE))
        .rounded_lg()
        .p_4()
}
fn field(label: &'static str, value: &str) -> Div {
    h_flex()
        .items_start()
        .gap_3()
        .child(caption(label).w(px(74.)).flex_none())
        .child(t(value.to_string(), FG).min_w_0().flex_1())
}
fn button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    active: bool,
) -> ObservedElement<Stateful<Div>> {
    let label = label.into();
    div()
        .id(id.into())
        .test_support()
        .role(Role::Button)
        .aria_label(label.clone())
        .tab_stop(true)
        .px_2p5()
        .py_1()
        .rounded_md()
        .text_xs()
        .cursor_pointer()
        .text_color(rgb(if active { MINT } else { MUTED }))
        .bg(rgb(if active { SELECTED } else { PANEL }))
        .hover(|s| s.bg(rgb(LINE)))
        .child(label)
}
fn item(id: impl Into<SharedString>, active: bool) -> ObservedElement<Stateful<Div>> {
    let id = id.into();
    v_flex()
        .id(id.clone())
        .test_support()
        .role(Role::Button)
        .aria_label(id)
        .tab_stop(true)
        .flex_none()
        .gap_2()
        .px_3()
        .py_3()
        .border_l_2()
        .border_color(rgb(if active { MINT } else { PANEL }))
        .rounded_md()
        .bg(rgb(if active { SELECTED } else { PANEL }))
        .hover(|s| s.bg(rgb(0x26333d)))
        .cursor_pointer()
}
fn metric(label: &'static str, count: usize, tone: u32) -> Div {
    h_flex()
        .flex_1()
        .gap_3()
        .items_baseline()
        .px_4()
        .py_2()
        .border_l_1()
        .border_color(rgb(LINE))
        .child(t(count.to_string(), tone).font_semibold().text_2xl())
        .child(caption(label))
}
fn contains(hay: &str, q: &str) -> bool {
    q.is_empty() || hay.to_lowercase().contains(q)
}

impl Studio {
    fn nav(
        &self,
        page: Page,
        label: &'static str,
        name: IconName,
        n: usize,
        cx: &Context<Self>,
    ) -> ObservedElement<Stateful<Div>> {
        let on = page == self.page;
        h_flex()
            .id(label)
            .test_support()
            .role(Role::Button)
            .aria_label(label)
            .tab_stop(true)
            .px_3()
            .py_2()
            .gap_2()
            .rounded_md()
            .text_color(rgb(if on { MINT } else { MUTED }))
            .cursor_pointer()
            .when(on, |s| s.bg(rgb(SELECTED)))
            .hover(|s| s.bg(rgb(SELECTED)))
            .child(icon(name))
            .child(div().flex_1().text_sm().child(label))
            .when(page != Page::Lifecycle, |row| {
                row.child(div().text_xs().child(n.to_string()))
            })
            .on_click(cx.listener(move |this, _, w, cx| this.navigate(page, w, cx)))
    }

    fn rail(&self, cx: &Context<Self>) -> Div {
        let repo = self
            .workspace
            .as_ref()
            .and_then(|ws| ws.root.file_name())
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        v_flex()
            .w(px(180.))
            .flex_none()
            .h_full()
            .p_3()
            .gap_1()
            .bg(rgb(RAIL))
            .border_r_1()
            .border_color(rgb(LINE))
            .child(
                h_flex()
                    .px_2()
                    .py_4()
                    .gap_2()
                    .child(
                        div()
                            .size(px(27.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(rgb(MINT))
                            .rounded_md()
                            .child(div().text_color(rgb(RAIL)).font_bold().child("A")),
                    )
                    .child(heading("AEP Studio")),
            )
            .child(self.nav(
                Page::Runs,
                "运行记录",
                IconName::LayoutDashboard,
                self.snapshot.runs.len(),
                cx,
            ))
            .child(self.nav(
                Page::Memory,
                "共享记忆",
                IconName::BookOpen,
                self.snapshot.notes.len(),
                cx,
            ))
            .child(self.nav(
                Page::Relay,
                "两轮接力",
                IconName::Play,
                self.relays.len(),
                cx,
            ))
            .child(self.nav(Page::Lifecycle, "隔离全流程", IconName::Play, 0, cx))
            .child(div().flex_1())
            .child(
                v_flex()
                    .gap_2()
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .p_2()
                    .child(caption("本地仓库"))
                    .child(t(repo, FG).truncate())
                    .child(
                        button("change-repo", "打开仓库…", false)
                            .on_click(cx.listener(|this, _, w, cx| this.choose_repo(w, cx))),
                    ),
            )
    }

    fn run_link(&self, id: &str, cx: &Context<Self>) -> AnyElement {
        if !self.snapshot.runs.iter().any(|r| r.id == id) {
            return t(format!("{id} · 运行记录不可用"), MUTED).into_any_element();
        }
        self.open_run_button("origin-run".into(), id, cx)
    }

    fn open_run_button(&self, button_id: SharedString, id: &str, cx: &Context<Self>) -> AnyElement {
        let target = id.to_string();
        button(button_id, format!("{id}  ↗"), true)
            .on_click(cx.listener(move |this, _, w, cx| {
                this.run = Some(target.clone());
                this.navigate(Page::Runs, w, cx);
            }))
            .into_any_element()
    }
    fn note_link(&self, id: i64, key: &str, cx: &Context<Self>) -> AnyElement {
        if !self
            .snapshot
            .notes
            .iter()
            .any(|n| n["id"].as_i64() == Some(id))
        {
            return caption(format!("#{id} · 记录不可用")).into_any_element();
        }
        button(format!("{key}-{id}"), format!("#{id}  ↗"), true)
            .on_click(cx.listener(move |this, _, w, cx| {
                this.note = Some(id);
                this.navigate(Page::Memory, w, cx);
            }))
            .into_any_element()
    }
    fn run_detail(&self, r: &Run, cx: &Context<Self>) -> Div {
        let (label, tone) = status(r.text("status"));
        let mut evidence = panel().child(heading("执行证据"));
        for (label, key) in [
            ("任务", "task"),
            ("Spec SHA", "spec_sha"),
            ("候选 SHA", "candidate_sha"),
            ("实现来源", "implementation_source"),
            ("审查方式", "reviewer"),
            ("审查状态", "review_status"),
            ("错误", "error"),
            ("恢复目录", "recovery_worktree"),
            ("清理错误", "cleanup_error"),
        ] {
            let value = r.text(key);
            if !value.is_empty() {
                evidence = evidence.child(field(label, value));
            }
        }
        for (label, key, sep) in [
            ("验收命令", "check", " "),
            ("变更文件", "changed_files", "\n"),
        ] {
            if let Some(items) = r.record[key].as_array() {
                let s = items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(sep);
                if !s.is_empty() {
                    evidence = evidence.child(field(label, &s));
                }
            }
        }
        if let Some(code) = r.record["check_exit_code"].as_i64() {
            evidence = evidence.child(field("退出码", &code.to_string()));
        }
        for (label, key) in [("开始时间", "started_at"), ("结束时间", "finished_at")] {
            if !r.text(key).is_empty() {
                evidence = evidence.child(field(label, r.text(key)));
            }
        }
        let mut header = h_flex().justify_between().child(heading("Agent 记忆交接"));
        if !r.text("task").is_empty() && !r.text("spec_sha").is_empty() {
            let id = r.id.clone();
            header = header.child(
                button("compose-run-note", "为此任务写记忆", false).on_click(cx.listener(
                    move |this, _, w, cx| this.compose(Start::FromRun(id.clone()), w, cx),
                )),
            );
        }
        let mut handoff = panel().child(header);
        let mut groups = 0;
        for role in AGENT_ROLES {
            for (key, verb) in [("memory_inputs", "读取"), ("memory_outputs", "写入")] {
                if let Some(ids) = r.record[key][role].as_array() {
                    groups += 1;
                    let mut row = h_flex()
                        .gap_2()
                        .flex_wrap()
                        .child(caption(format!("{} · {}", author(role), verb)).w(px(110.)));
                    if ids.is_empty() {
                        row = row.child(caption("0 条"));
                    }
                    for id in ids.iter().filter_map(Value::as_i64) {
                        row = row.child(self.note_link(id, &format!("{key}-{role}"), cx));
                    }
                    handoff = handoff.child(row);
                }
            }
        }
        if groups == 0 {
            handoff = handoff.child(caption("此次运行没有记录 Agent 的记忆读写。"));
        }
        let mut buttons = h_flex().gap_2();
        let mut available = vec![
            (Artifact::Checks, "验收日志"),
            (Artifact::Review, "审查报告"),
        ];
        if attention(r.text("status")) {
            available.extend([
                (Artifact::Implementation, "实现日志"),
                (Artifact::ReviewDiagnostics, "审查诊断"),
            ]);
        }
        for (kind, title) in available {
            let id = r.id.clone();
            let active = self
                .artifact
                .as_ref()
                .is_some_and(|(run, k, _)| run == &id && *k == kind);
            buttons = buttons
                .child(button(kind.filename(), title, active).on_click(
                    cx.listener(move |this, _, _, cx| this.show_artifact(&id, kind, cx)),
                ));
        }
        let mut artifacts = panel().child(heading("运行产物")).child(buttons);
        if let Some((id, kind, text)) = &self.artifact
            && id == &r.id
        {
            artifacts = artifacts.child(caption(kind.filename())).child(
                detail_text("artifact-body", text)
                    .font_family("Menlo")
                    .text_xs(),
            );
        }
        v_flex()
            .gap_3()
            .child(chip(label, tone))
            .child(heading(r.id.clone()).text_2xl())
            .child(handoff)
            .child(evidence)
            .child(artifacts)
    }

    fn memory_read_history(&self, id: i64, cx: &Context<Self>) -> Div {
        let mut history = panel().child(heading("读取记录"));
        let mut count = 0;
        for run in &self.snapshot.runs {
            let readers = AGENT_ROLES
                .into_iter()
                .filter(|role| {
                    run.record["memory_inputs"][role]
                        .as_array()
                        .is_some_and(|ids| ids.iter().any(|value| value.as_i64() == Some(id)))
                })
                .map(author)
                .collect::<Vec<_>>();
            if readers.is_empty() {
                continue;
            }
            count += 1;
            history = history.child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(self.open_run_button(format!("read-run-{}", run.id).into(), &run.id, cx))
                    .child(
                        detail_text(
                            SharedString::from(format!("read-roles-{}", run.id)),
                            &readers.join("、"),
                        )
                        .text_xs()
                        .text_color(rgb(MUTED)),
                    ),
            );
        }
        history.when(count == 0, |panel| {
            panel.child(caption("此版本暂无读取记录。"))
        })
    }

    fn memory_detail(&self, n: &Value, cx: &Context<Self>) -> Div {
        let id = n["id"].as_i64().unwrap_or_default();
        let mut meta = panel()
            .child(heading("来源与版本"))
            .child(field("作者", author(string(n, "author"))))
            .child(field("范围", scope(string(n, "scope"))))
            .child(field("记录时间", string(n, "created_at")));
        for (label, key) in [
            ("任务", "task"),
            ("Spec SHA", "spec_sha"),
            ("候选 SHA", "candidate_sha"),
            ("来源 SHA", "origin_sha"),
        ] {
            let s = string(n, key);
            if !s.is_empty() {
                meta = meta.child(field(label, s));
            }
        }
        if let Some(run) = n["run_id"].as_str() {
            meta = meta.child(
                h_flex()
                    .gap_3()
                    .child(caption("来源 Run").w(px(74.)))
                    .child(self.run_link(run, cx)),
            );
        }
        for (label, key) in [("上一版本", "replaces"), ("下一版本", "successor")] {
            if let Some(id) = n[key].as_i64() {
                meta = meta.child(
                    h_flex()
                        .gap_3()
                        .child(caption(label).w(px(74.)))
                        .child(self.note_link(id, key, cx)),
                );
            }
        }
        let version = if n["successor"].is_null() {
            "当前版本"
        } else {
            "历史版本"
        };
        v_flex()
            .gap_3()
            .child(chip(
                version,
                if n["successor"].is_null() {
                    MINT
                } else {
                    AMBER
                },
            ))
            .child(heading(format!("#{id}  {}", kind(string(n, "kind")))).text_2xl())
            .child(
                panel()
                    .child(h_flex().justify_between().child(heading("记忆正文")).when(
                        n["successor"].is_null(),
                        |h| {
                            h.child(button("revise-note", "修订", false).on_click(cx.listener(
                                move |this, _, w, cx| this.compose(Start::Revise(id), w, cx),
                            )))
                        },
                    ))
                    .child(detail_text("note-body", string(n, "body"))),
            )
            .child(meta)
            .child(self.memory_read_history(id, cx))
    }
}

impl Render for Studio {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.lifecycle_frame(window, cx);
        if self.workspace.is_none() {
            return v_flex()
                .size_full()
                .justify_center()
                .items_center()
                .gap_4()
                .bg(rgb(CANVAS))
                .child(heading("AEP Studio").text_2xl())
                .child(t("选择要查看运行记录和共享记忆的 Git 仓库。", MUTED))
                .child(
                    button("open-repo", "打开仓库…", true)
                        .on_click(cx.listener(|this, _, w, cx| this.choose_repo(w, cx))),
                )
                .children(self.repo_error.iter().map(|e| t(e.clone(), AMBER)));
        }
        if let (Some(editor), Some(ws)) = (&self.editor, &self.workspace) {
            return editor.render(ws, cx);
        }
        if self.page == Page::Relay {
            return self.relay_page(cx);
        }
        if self.page == Page::Review {
            return self.human_review_page(cx);
        }
        if self.page == Page::PullRequest {
            return self.github_pr_page(cx);
        }
        if self.page == Page::Lifecycle {
            return self.lifecycle_page(cx);
        }
        let runs = &self.snapshot.runs;
        let notes = &self.snapshot.notes;
        let q = self.search.read(cx).value().trim().to_lowercase();
        let runs_page = self.page == Page::Runs;
        let title = if runs_page {
            "运行记录"
        } else {
            "共享记忆"
        };
        let filters = if runs_page {
            [
                (Filter::All, "全部"),
                (Filter::Pending, "待审查"),
                (Filter::Attention, "需处理"),
            ]
        } else {
            [
                (Filter::All, "全部"),
                (Filter::Current, "当前"),
                (Filter::History, "历史"),
            ]
        };
        let stats = if runs_page {
            [
                ("总运行", runs.len(), FG),
                (
                    "待人工审查",
                    runs.iter().filter(|r| pending(r.text("status"))).count(),
                    AMBER,
                ),
                (
                    "需处理",
                    runs.iter().filter(|r| attention(r.text("status"))).count(),
                    RED,
                ),
            ]
        } else {
            [
                ("总记忆", notes.len(), FG),
                (
                    "当前版本",
                    notes.iter().filter(|n| n["successor"].is_null()).count(),
                    MINT,
                ),
                (
                    "当前未决问题",
                    notes
                        .iter()
                        .filter(|n| n["kind"] == "open_question" && n["successor"].is_null())
                        .count(),
                    AMBER,
                ),
            ]
        };
        let mut chips = h_flex().gap_1();
        for (filter, label) in filters {
            chips = chips.child(
                button(format!("filter-{label}"), label, filter == self.filter).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.filter = filter;
                        cx.notify();
                    }),
                ),
            );
        }

        let mut list = v_flex()
            .id(("list", usize::from(!runs_page)))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_2()
            .gap_1();
        let detail;
        let count;
        let mut detail_id = String::from(title);
        if runs_page {
            let hits: Vec<_> = runs
                .iter()
                .filter(|r| {
                    (match self.filter {
                        Filter::Pending => pending(r.text("status")),
                        Filter::Attention => attention(r.text("status")),
                        _ => true,
                    }) && [
                        r.id.as_str(),
                        r.text("task"),
                        r.text("candidate_sha"),
                        r.text("spec_sha"),
                        r.text("error"),
                    ]
                    .iter()
                    .any(|s| contains(s, &q))
                })
                .collect();
            count = hits.len();
            let current = hits
                .iter()
                .copied()
                .find(|r| Some(&r.id) == self.run.as_ref())
                .or_else(|| hits.first().copied());
            if let Some(r) = current {
                detail_id = r.id.clone();
            }
            detail = current.map(|r| self.run_detail(r, cx));
            for r in hits {
                let id = r.id.clone();
                let (label, tone) = status(r.text("status"));
                list = list.child(
                    item(
                        format!("run-{}", r.id),
                        current.is_some_and(|c| c.id == r.id),
                    )
                    .child(
                        h_flex()
                            .justify_between()
                            .child(chip(label, tone))
                            .child(caption(date(r.text("started_at")))),
                    )
                    .child(t(id.clone(), FG).font_semibold().truncate())
                    .when(!r.text("task").is_empty(), |el| {
                        el.child(caption(r.text("task").to_owned()).truncate())
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.run = Some(id.clone());
                        this.artifact = None;
                        cx.notify();
                    })),
                );
            }
        } else {
            let hits: Vec<_> = notes
                .iter()
                .filter(|n| {
                    (match self.filter {
                        Filter::Current => n["successor"].is_null(),
                        Filter::History => !n["successor"].is_null(),
                        _ => true,
                    }) && (["task", "body", "author", "kind", "scope", "run_id"]
                        .iter()
                        .any(|k| contains(string(n, k), &q))
                        || contains(&format!("#{}", n["id"]), &q)
                        || contains(author(string(n, "author")), &q)
                        || contains(kind(string(n, "kind")), &q))
                })
                .collect();
            count = hits.len();
            let current = hits
                .iter()
                .copied()
                .find(|n| n["id"].as_i64() == self.note)
                .or_else(|| hits.first().copied());
            if let Some(n) = current {
                detail_id = format!("memory-{}", n["id"]);
            }
            detail = current.map(|n| self.memory_detail(n, cx));
            for n in hits {
                let id = n["id"].as_i64().unwrap_or_default();
                list = list.child(
                    item(format!("note-{id}"), current.is_some_and(|c| c["id"] == id))
                        .child(
                            h_flex()
                                .justify_between()
                                .child(
                                    t(format!("#{id}  {}", kind(string(n, "kind"))), MINT)
                                        .text_xs()
                                        .font_semibold(),
                                )
                                .child(caption(author(string(n, "author")).to_owned())),
                        )
                        .child(t(string(n, "body").to_owned(), FG).truncate())
                        .child(caption(
                            if n["successor"].is_null() {
                                scope(string(n, "scope"))
                            } else {
                                "历史版本"
                            }
                            .to_owned(),
                        ))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.note = Some(id);
                            cx.notify();
                        })),
                );
            }
        }
        let empty = if self.loading {
            "正在读取本地数据…"
        } else if !q.is_empty() || self.filter != Filter::All {
            "没有符合筛选条件的记录"
        } else if runs_page {
            "该仓库还没有运行记录"
        } else {
            "该仓库还没有共享记忆"
        };
        if count == 0 {
            list = list.child(caption(empty).p_2());
        }
        let detail = match detail {
            Some(content) => content,
            None => v_flex()
                .items_center()
                .justify_center()
                .h_full()
                .gap_2()
                .child(heading(empty))
                .child(caption(if self.loading {
                    ""
                } else if !q.is_empty() || self.filter != Filter::All {
                    "可调整关键词或切换为全部。"
                } else if runs_page {
                    "在这个仓库执行 aep run 后刷新。"
                } else {
                    "点击「新建记忆」写入第一条工程记录。"
                })),
        };
        let mut main = v_flex().flex_1().min_w_0().h_full();
        let mut controls = h_flex().gap_2();
        if !runs_page {
            controls = controls.child(
                button("new-note", "新建记忆", true)
                    .on_click(cx.listener(|this, _, w, cx| this.compose(Start::Blank, w, cx))),
            );
        }
        let bar = h_flex()
            .flex_none()
            .px_5()
            .py_3()
            .justify_between()
            .border_b_1()
            .border_color(rgb(LINE))
            .child(heading(title).text_2xl())
            .child(
                controls.child(
                    button(
                        "refresh",
                        if self.loading {
                            "读取中…"
                        } else {
                            "刷新"
                        },
                        false,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                ),
            );
        let stats_bar = h_flex()
            .flex_none()
            .bg(rgb(PANEL))
            .py_2()
            .border_b_1()
            .border_color(rgb(LINE))
            .children(stats.map(|(label, count, tone)| metric(label, count, tone)));
        main = main.child(bar).child(stats_bar);
        if let Some(error) = &self.repo_error {
            main = main.child(t(error.clone(), AMBER).px_4());
        }
        if !self.snapshot.issues.is_empty() {
            // Keep diagnostics scrollable so a damaged store cannot cover the entire browser.
            main = main.child(
                v_flex()
                    .id("read-errors")
                    .overflow_y_scroll()
                    .max_h(px(84.))
                    .flex_none()
                    .px_4()
                    .py_2()
                    .bg(rgb(PANEL))
                    .children(
                        self.snapshot
                            .issues
                            .iter()
                            .map(|e| t(e.clone(), AMBER).text_xs()),
                    ),
            );
        }
        let index = v_flex()
            .w(px(295.))
            .h_full()
            .flex_none()
            .border_r_1()
            .border_color(rgb(LINE))
            .child(
                v_flex()
                    .flex_none()
                    .gap_2()
                    .p_3()
                    .child(Input::new(&self.search).id("search"))
                    .child(
                        h_flex()
                            .justify_between()
                            .child(chips)
                            .child(caption(format!("{count} 条"))),
                    ),
            )
            .child(list);
        let body = h_flex()
            .flex_1()
            .min_h_0()
            .items_start()
            .child(index)
            .child(
                v_flex()
                    .id(SharedString::from(format!("detail-{detail_id}")))
                    .test_support()
                    .p_5()
                    .min_w_0()
                    .flex_1()
                    .h_full()
                    .overflow_y_scroll()
                    .child(detail),
            );
        h_flex()
            .size_full()
            .bg(rgb(CANVAS))
            .text_color(rgb(FG))
            .items_start()
            .child(self.rail(cx))
            .child(main.child(body))
    }
}
