//! The stage conversation: every attempt of a stage rendered as one terminal-style session,
//! with a composer that continues the current CLI session.
use super::*;
use aep::handoff::busy;
use aep::transcript::{Entry, Transcript};
use std::{fs, io::Read as _, io::Seek as _, rc::Rc};

/// Largest stdout tail parsed for display; older events stay in the raw log.
const TAIL_BYTES: u64 = 4 << 20;
/// Entries shown per turn before older ones collapse into a count.
const SHOWN: usize = 120;
const MONO: &str = "Menlo";

pub(super) type Cache = std::cell::RefCell<BTreeMap<PathBuf, (u64, Rc<Transcript>)>>;
/// Per-turn scroll position and the entry count it last followed.
pub(super) type Scrolls = std::cell::RefCell<BTreeMap<String, (ScrollHandle, usize)>>;

fn tail_lines(s: &str, n: usize) -> (String, usize) {
    let lines: Vec<_> = s.trim_end().lines().collect();
    let hidden = lines.len().saturating_sub(n);
    (lines[hidden..].join("\n"), hidden)
}
fn mono(s: impl Into<SharedString>, color: u32) -> Div {
    div()
        .font_family(MONO)
        .text_xs()
        .text_color(rgb(color))
        .child(s.into())
}
fn elapsed(ev: &Value) -> Option<String> {
    let start = chrono::DateTime::parse_from_rfc3339(ev["started_at"].as_str()?).ok()?;
    let end = ev["finished_at"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map_or_else(chrono::Utc::now, |t| t.with_timezone(&chrono::Utc));
    let secs = (end - start.with_timezone(&chrono::Utc))
        .num_seconds()
        .max(0);
    Some(format!("{}:{:02}", secs / 60, secs % 60))
}

impl Studio {
    fn transcript(&self, dir: &Path) -> Rc<Transcript> {
        let log = dir.join("stdout.log");
        let len = log.metadata().map(|m| m.len()).unwrap_or(0);
        if let Some((seen, t)) = self.board.transcripts.borrow().get(&log)
            && *seen == len
        {
            return t.clone();
        }
        let mut raw = String::new();
        if let Ok(mut f) = fs::File::open(&log) {
            let skip = len.saturating_sub(TAIL_BYTES);
            let _ = f.seek(std::io::SeekFrom::Start(skip));
            let mut bytes = vec![];
            let _ = f.read_to_end(&mut bytes);
            raw = String::from_utf8_lossy(&bytes).into_owned();
            if skip > 0 {
                // Drop the partial first line.
                raw = raw
                    .split_once('\n')
                    .map_or(String::new(), |(_, r)| r.to_owned());
            }
        }
        let parsed = Rc::new(aep::transcript::parse(&raw));
        self.board
            .transcripts
            .borrow_mut()
            .insert(log, (len, parsed.clone()));
        parsed
    }

    fn toggle(
        &self,
        id: String,
        cx: &Context<Self>,
    ) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, _, _, cx| {
            if !this.board.expanded.remove(&id) {
                this.board.expanded.insert(id.clone());
            }
            cx.notify();
        })
    }

    /// Standalone entries: answers, reasoning, errors and plain CLI text.
    fn entry(&self, key: &str, n: usize, e: &Entry, cx: &Context<Self>) -> AnyElement {
        match e {
            Entry::Message(s) => h_flex()
                .items_start()
                .gap_2()
                .child(t("●", MINT).text_xs().mt(px(2.)))
                .child(
                    detail_text(SharedString::from(format!("{key}-msg-{n}")), s)
                        .min_w_0()
                        .flex_1(),
                )
                .into_any_element(),
            Entry::Thinking(s) => {
                let id = format!("{key}-think-{n}");
                let open = self.board.expanded.contains(&id);
                let first = s.trim().lines().next().unwrap_or_default().to_owned();
                h_flex()
                    .id(SharedString::from(id.clone()))
                    .cursor_pointer()
                    .items_start()
                    .gap_2()
                    .child(t("∴", MUTED).text_xs())
                    .child(
                        caption(if open { s.trim().to_owned() } else { first })
                            .italic()
                            .when(!open, |c| c.truncate())
                            .min_w_0()
                            .flex_1(),
                    )
                    .on_click(self.toggle(id, cx))
                    .into_any_element()
            }
            Entry::Error(s) => h_flex()
                .gap_2()
                .items_start()
                .child(t("✗", RED).text_xs())
                .child(mono(s.clone(), RED).min_w_0().flex_1())
                .into_any_element(),
            Entry::Notice(s) => h_flex()
                .gap_2()
                .child(t("!", AMBER).text_xs())
                .child(caption(s.clone()).truncate().min_w_0().flex_1())
                .into_any_element(),
            Entry::Text(s) => {
                let (shown, hidden) = tail_lines(s, 400);
                v_flex()
                    .id(SharedString::from(format!("{key}-text-{n}")))
                    .max_h(px(260.))
                    .overflow_y_scroll()
                    .when(hidden > 0, |b| {
                        b.child(mono(format!("… 前 {hidden} 行见原始日志"), MUTED))
                    })
                    .child(mono(shown, MUTED))
                    .into_any_element()
            }
            _ => self.actions(key, n, std::slice::from_ref(e), false, cx),
        }
    }

    /// Consecutive commands and tool calls folded into one line, like a coding CLI.
    fn actions(
        &self,
        key: &str,
        start: usize,
        items: &[Entry],
        live: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let id = format!("{key}-group-{start}");
        let open = live || self.board.expanded.contains(&id);
        let (mut commands, mut reads, mut edits, mut tools, mut failed) = (0, 0, 0, 0, 0);
        for e in items {
            match e {
                Entry::Command { exit, .. } => {
                    commands += 1;
                    failed += usize::from(exit.is_some_and(|c| c != 0));
                }
                Entry::Edit(paths) => edits += paths.len(),
                Entry::Tool {
                    name, failed: f, ..
                } => {
                    failed += usize::from(*f);
                    if matches!(name.as_str(), "Read" | "Grep" | "Glob" | "LS") {
                        reads += 1;
                    } else {
                        tools += 1;
                    }
                }
                _ => {}
            }
        }
        let mut parts = vec![];
        if commands > 0 {
            parts.push(format!("运行 {commands} 条命令"));
        }
        if reads > 0 {
            parts.push(format!("读取/搜索 {reads} 次"));
        }
        if edits > 0 {
            parts.push(format!("修改 {edits} 个文件"));
        }
        if tools > 0 {
            parts.push(format!("调用 {tools} 个工具"));
        }
        let mut block = v_flex().gap_1().child(
            h_flex()
                .id(SharedString::from(id.clone()))
                .cursor_pointer()
                .gap_2()
                .rounded_sm()
                .hover(|s| s.bg(rgb(SELECTED)))
                .child(t(if open { "▾" } else { "▸" }, MUTED).text_xs())
                .child(caption(parts.join(" · ")))
                .when(failed > 0, |r| {
                    r.child(t(format!("{failed} 个失败"), RED).text_xs())
                })
                .when(live, |r| r.child(t("进行中", BLUE).text_xs()))
                .on_click(self.toggle(id, cx)),
        );
        let last = items.len().saturating_sub(1);
        for (i, e) in items.iter().enumerate() {
            let n = start + i;
            let failed_here = matches!(e, Entry::Command { exit: Some(c), .. } if *c != 0)
                || matches!(e, Entry::Tool { failed: true, .. });
            // Collapsed groups still surface failures, one line each.
            if !open && !failed_here {
                continue;
            }
            block = block.child(self.action(key, n, e, live && i == last, cx).ml_4());
        }
        block.into_any_element()
    }

    /// One action line; its output opens on click (and stays open while it runs).
    fn action(&self, key: &str, n: usize, e: &Entry, running: bool, cx: &Context<Self>) -> Div {
        let id = format!("{key}-out-{n}");
        let open = self.board.expanded.contains(&id);
        let (head, output, failed) = match e {
            Entry::Command {
                command,
                output,
                exit,
            } => {
                let (mark, tone) = match exit {
                    None => ("…".to_string(), BLUE),
                    Some(0) => ("✓".to_string(), MINT),
                    Some(c) => (format!("✗ {c}"), RED),
                };
                let lines = output.trim_end().lines().count();
                (
                    h_flex()
                        .gap_2()
                        .child(mono("$", AMBER))
                        .child(
                            mono(command.lines().next().unwrap_or_default().to_owned(), FG)
                                .truncate()
                                .min_w_0()
                                .flex_1(),
                        )
                        .when(lines > 0, |r| r.child(caption(format!("{lines} 行"))))
                        .child(mono(mark, tone).flex_none()),
                    output.clone(),
                    exit.is_some_and(|c| c != 0),
                )
            }
            Entry::Edit(paths) => (
                h_flex()
                    .gap_2()
                    .child(t("✎", AMBER).text_xs())
                    .child(mono(paths.join("  "), AMBER).truncate().min_w_0().flex_1()),
                String::new(),
                false,
            ),
            Entry::Tool {
                name,
                detail,
                output,
                failed,
            } => (
                h_flex()
                    .gap_2()
                    .child(t("⚙", MUTED).text_xs())
                    .child(mono(name.clone(), BLUE))
                    .child(mono(detail.clone(), MUTED).truncate().min_w_0().flex_1()),
                output.clone(),
                *failed,
            ),
            _ => return div(),
        };
        let has_output = !output.trim().is_empty();
        let mut row = v_flex().gap_1().child(
            div()
                .id(SharedString::from(id.clone()))
                .when(has_output, |d| {
                    d.cursor_pointer().hover(|s| s.bg(rgb(SELECTED)))
                })
                .rounded_sm()
                .child(head)
                .on_click(self.toggle(id, cx)),
        );
        if has_output && (open || running) {
            let (shown, hidden) = tail_lines(&output, if open { 400 } else { 6 });
            row = row.child(
                v_flex()
                    .id(SharedString::from(format!("{key}-outbox-{n}")))
                    .ml_4()
                    .pl_2()
                    .max_h(px(240.))
                    .overflow_y_scroll()
                    .border_l_1()
                    .border_color(rgb(if failed { RED } else { LINE }))
                    .when(hidden > 0, |b| {
                        b.child(mono(format!("… 前 {hidden} 行见原始日志"), MUTED))
                    })
                    .child(mono(shown, if failed { RED } else { MUTED })),
            );
        }
        row
    }

    /// Card 2 body: the stage's turns, oldest first, ending in the composer.
    pub(super) fn stage_console(
        &self,
        p: &Project,
        work: &Work,
        i: usize,
        cx: &Context<Self>,
    ) -> Div {
        let mut view = v_flex().gap_3();
        let Some(base) = self.board.dir.as_ref() else {
            return view;
        };
        let turns: Vec<_> = work.attempts.iter().filter(|a| a.stage == i).collect();
        let mut running = false;
        for (round, a) in turns.iter().enumerate() {
            let d = workflow::attempt_dir(base, &p.id, &work.id, &a.id);
            let ev = workflow::evidence(&d)
                .unwrap_or_else(|e| serde_json::json!({"error":e.to_string()}));
            let req: Option<aep::workflow::Request> = fs::File::open(d.join("request.json"))
                .ok()
                .and_then(|f| serde_json::from_reader(f).ok());
            let active = busy(&ev);
            running |= active;
            let current = work.current_attempt.get(&i) == Some(&a.id);
            let tone = match ev["status"].as_str() {
                Some("succeeded") => MINT,
                Some("failed") => RED,
                Some("cancelled" | "needs_reconciliation" | "unstarted") => AMBER,
                _ => BLUE,
            };
            let mut bar = h_flex()
                .gap_2()
                .flex_wrap()
                .pb_1()
                .border_b_1()
                .border_color(rgb(LINE))
                .child(
                    t(format!("第 {} 轮", round + 1), FG)
                        .text_xs()
                        .font_semibold(),
                )
                .child(chip(agents::phase(&ev), tone))
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
                )))
                .when_some(elapsed(&ev), |b, s| b.child(caption(s)))
                .when(current && !active && turns.len() > 1, |b| {
                    b.child(caption("· 当前结论"))
                })
                .child(div().flex_1());
            let expanded = self
                .board
                .log_view
                .as_ref()
                .is_some_and(|(id, _, _)| id == &a.id);
            let id = a.id.clone();
            bar = bar.child(
                button(
                    format!("workflow-inspect-{id}"),
                    if expanded {
                        "收起原始日志"
                    } else {
                        "原始日志"
                    },
                    expanded,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.board.log_view = if expanded {
                        None
                    } else {
                        Some((id.clone(), "result.txt", 0))
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
                    button(format!("workflow-files-{}", a.id), "工作区", false)
                        .on_click(cx.listener(move |_, _, _, cx| cx.reveal_path(&ws))),
                );
            }
            if active {
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
            let key = format!("turn-{}", a.id);
            let mut turn = v_flex()
                .gap_2()
                .p_3()
                .rounded_md()
                .bg(rgb(CANVAS))
                .border_1()
                .border_color(rgb(if active { BLUE } else { LINE }))
                .child(bar);
            let asked = req.as_ref().map(|r| {
                r.follow_up
                    .as_ref()
                    .map_or(r.brief.as_str(), |f| f.message.as_str())
                    .trim()
                    .to_owned()
            });
            let asked = asked.filter(|s| !s.is_empty()).unwrap_or_else(|| {
                if round == 0 {
                    "（未填写阶段说明，按阶段职责执行）".into()
                } else {
                    "（重新执行）".into()
                }
            });
            turn = turn.child(
                h_flex()
                    .items_start()
                    .gap_2()
                    .child(t("›", BLUE).font_semibold())
                    .child(
                        detail_text(SharedString::from(format!("{key}-ask")), &asked)
                            .text_color(rgb(BLUE))
                            .min_w_0()
                            .flex_1(),
                    ),
            );
            if ev["mode"] == "external" {
                turn = turn.child(caption(format!(
                    "由独立会话 {} 执行，完成后用 project submit 交回结果。",
                    ev["session"].as_str().unwrap_or("—")
                )));
            }
            let transcript = self.transcript(&d);
            let entries = &transcript.entries;
            let hidden = entries.len().saturating_sub(SHOWN);
            // The turn's output scrolls inside a bounded box and follows new output.
            let handle = {
                let mut scrolls = self.board.scrolls.borrow_mut();
                let slot = scrolls
                    .entry(key.clone())
                    .or_insert_with(|| (ScrollHandle::new(), usize::MAX));
                if slot.1 != entries.len() {
                    slot.1 = entries.len();
                    slot.0.scroll_to_bottom();
                }
                slot.0.clone()
            };
            let mut log = v_flex()
                .id(SharedString::from(format!("{key}-log")))
                .track_scroll(&handle)
                .max_h(px(460.))
                .overflow_y_scroll()
                .gap_2();
            if hidden > 0 {
                log = log.child(caption(format!("… 前 {hidden} 条输出见原始日志")));
            }
            let is_action = |e: &Entry| {
                matches!(
                    e,
                    Entry::Command { .. } | Entry::Edit(_) | Entry::Tool { .. }
                )
            };
            let mut n = hidden;
            while n < entries.len() {
                if is_action(&entries[n]) {
                    let end = (n..entries.len())
                        .find(|&j| !is_action(&entries[j]))
                        .unwrap_or(entries.len());
                    let live = active && end == entries.len();
                    log = log.child(self.actions(&key, n, &entries[n..end], live, cx));
                    n = end;
                } else {
                    log = log.child(self.entry(&key, n, &entries[n], cx));
                    n += 1;
                }
            }
            let has_message = transcript
                .entries
                .iter()
                .any(|e| matches!(e, Entry::Message(_)));
            // ZCode and custom CLIs write the answer only to result.txt.
            if !active
                && !has_message
                && ev["status"] == "succeeded"
                && let Ok(result) = fs::read_to_string(d.join("result.txt"))
                && !result.trim().is_empty()
                && !transcript
                    .entries
                    .iter()
                    .any(|e| matches!(e, Entry::Text(s) if s.contains(result.trim())))
            {
                log = log.child(self.entry(&key, usize::MAX, &Entry::Message(result), cx));
            }
            turn = turn.child(log);
            if active {
                turn = turn.child(t("▍ 正在执行…", BLUE).text_xs());
            } else if let Some(error) = ev["error"].as_str() {
                let (shown, _) = tail_lines(error, 16);
                turn = turn.child(
                    detail_text(SharedString::from(format!("{key}-error")), &shown)
                        .text_color(rgb(RED))
                        .font_family(MONO)
                        .text_xs(),
                );
            } else if ev["status"] == "succeeded" {
                let mut foot = vec!["✓ 完成".to_string()];
                if transcript.input_tokens + transcript.output_tokens > 0 {
                    foot.push(format!(
                        "输入 {} · 输出 {} tokens",
                        transcript.input_tokens, transcript.output_tokens
                    ));
                }
                if let Some(id) = ev["written_memory"].as_i64() {
                    foot.push(format!("写入项目记忆 #{id}"));
                }
                if ev["session"].is_string() {
                    foot.push("可继续对话".into());
                }
                turn = turn.child(caption(foot.join("  ·  ")));
            }
            if let Some((open, name, page)) = &self.board.log_view
                && open == &a.id
            {
                turn = turn.child(self.log_view(&d, open, name, *page, cx));
            }
            view = view.child(turn);
        }
        if i != work.stage {
            return view;
        }
        let continuable = work
            .current_attempt
            .get(&i)
            .and_then(|id| {
                let ev =
                    workflow::evidence(&workflow::attempt_dir(base, &p.id, &work.id, id)).ok()?;
                (ev["mode"] != "external" && ev["workspace"].is_string()).then_some(ev)
            })
            .is_some();
        if running {
            return view.child(caption(
                "Agent 执行期间可以关闭 Studio，重新打开后继续查看或取消。",
            ));
        }
        if continuable {
            view = view.child(
                v_flex()
                    .gap_2()
                    .child(
                        div().id("workflow-chat").test_support().child(
                            Textarea::new(&self.board.chat)
                                .aria_label("继续对话")
                                .h(px(64.)),
                        ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(primary("workflow-send", "发送").on_click(cx.listener(
                                |this, _, w, cx| {
                                    if let Err(e) = this.send_follow_up(w, cx) {
                                        this.board.error = Some(format!("{e:#}"));
                                    }
                                    cx.notify();
                                },
                            )))
                            .child(caption(
                                "在同一会话和工作区继续；Codex、Claude 直接续接原会话，其他 CLI 会带上上一轮答复。",
                            )),
                    ),
            );
        }
        view
    }

    fn send_follow_up(&mut self, w: &mut Window, cx: &mut Context<Self>) -> Result<()> {
        ensure!(!self.has_running_agent(), "此事项已有 CLI 正在运行");
        let message = self.board.chat.read(cx).value().to_string();
        let home = self.board.dir.clone().context("找不到项目库")?;
        let i = self.board.viewing;
        self.runner.binary()?;
        self.runner.path()?;
        let mut folder = None;
        if !self.change_board(cx, |lib| {
            let p = lib.project_mut()?;
            let work = p.selected().context("请选择开发事项")?.id.clone();
            folder = Some(p.reserve_follow_up(&home, &work, i, &message)?);
            Ok(())
        }) {
            return Ok(());
        }
        self.board.chat.update(cx, |s, cx| s.set_value("", w, cx));
        let d = folder.context("未创建运行记录")?;
        self.spawn_stage(&d, cx)
    }
}
