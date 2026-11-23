use super::*;
use aep::project::{
    archive::Delivery,
    merge::{self as local, Branch, MergeStatus, Preview, Recovery},
};

pub(super) struct State {
    delivery: Delivery,
    request: uuid::Uuid,
    busy: bool,
    branches: Vec<Branch>,
    selected_target: Option<String>,
    preview: Option<Preview>,
    error: Option<String>,
    confirming: bool,
    internal: bool,
    expanded: std::collections::BTreeSet<usize>,
}
impl State {
    fn pending(&self) -> Option<&local::MergeReceipt> {
        self.delivery
            .merge
            .as_ref()
            .filter(|r| r.status == MergeStatus::Pending)
    }
    fn target(&self, requested: Option<String>) -> Option<String> {
        self.pending()
            .map(|r| r.target_branch.clone())
            .or(requested)
            .or_else(|| self.selected_target.clone())
    }
    fn can_confirm(&self) -> bool {
        if self.busy {
            return false;
        }
        let Some(p) = &self.preview else {
            return false;
        };
        match self.pending() {
            Some(r) if r.target_branch != p.branch.name => false,
            Some(r) => match r.recovery(&p.branch.tip) {
                Recovery::Record => true,
                Recovery::Retry => p.branch.writable(),
                Recovery::Changed => false,
            },
            None => p.branch.writable(),
        }
    }
}
impl Studio {
    pub(super) fn open_work_merge(&mut self, delivery: Delivery, cx: &mut Context<Self>) {
        if let Some(t) = self.board.timeline.as_mut() {
            t.merge = Some(State {
                delivery,
                request: uuid::Uuid::new_v4(),
                busy: false,
                branches: vec![],
                selected_target: None,
                preview: None,
                error: None,
                confirming: false,
                internal: false,
                expanded: Default::default(),
            });
        }
        self.preview_work_merge(None, cx);
    }
    fn preview_work_merge(&mut self, target: Option<String>, cx: &mut Context<Self>) {
        let Some(home) = self.board.dir.clone() else {
            return;
        };
        let Some(s) = self.board.timeline.as_mut().and_then(|t| t.merge.as_mut()) else {
            return;
        };
        let target = s.target(target);
        s.selected_target = target.clone();
        s.request = uuid::Uuid::new_v4();
        s.busy = true;
        s.confirming = false;
        s.error = None;
        s.preview = None;
        s.expanded.clear();
        let request = s.request;
        let d = s.delivery.clone();
        let job = cx.background_executor().spawn(async move {
            (|| -> Result<(Vec<Branch>, Preview)> {
                let candidate = local::candidate(&home, &d)?;
                let branches = local::branches(&candidate)?;
                let default = candidate.default_target;
                let target = target
                    .or_else(|| d.merge.as_ref().map(|r| r.target_branch.clone()))
                    .or_else(|| default.filter(|n| branches.iter().any(|b| &b.name == n)))
                    .or_else(|| {
                        branches
                            .iter()
                            .find(|b| !b.internal)
                            .map(|b| b.name.clone())
                    })
                    .context("没有本地目标分支")?;
                let p = local::preview(&home, &d, &target)?;
                Ok((branches, p))
            })()
            .map_err(|e| format!("{e:#}"))
        });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |s, cx| {
                if let Some(state) = s
                    .board
                    .timeline
                    .as_mut()
                    .and_then(|t| t.merge.as_mut())
                    .filter(|s| s.request == request)
                {
                    state.busy = false;
                    match result {
                        Ok((b, p)) => {
                            state.branches = b;
                            state.selected_target = Some(p.branch.name.clone());
                            state.preview = Some(p);
                        }
                        Err(e) => state.error = Some(e),
                    }
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }
    fn confirm_work_merge(&mut self, abandon: bool, cx: &mut Context<Self>) {
        let Some(home) = self.board.dir.clone() else {
            return;
        };
        let Some(s) = self.board.timeline.as_mut().and_then(|t| t.merge.as_mut()) else {
            return;
        };
        if s.busy || (!abandon && !s.can_confirm()) {
            return;
        }
        let target = s.target(None);
        let Some(target) = target else { return };
        let digest = s
            .delivery
            .merge
            .as_ref()
            .filter(|r| r.status == MergeStatus::Pending)
            .map(|r| r.diff_sha256.clone())
            .or_else(|| s.preview.as_ref().map(|p| p.diff_sha256.clone()))
            .unwrap_or_default();
        let d = s.delivery.clone();
        let request = s.request;
        let run = d.run.clone();
        s.busy = true;
        s.error = None;
        let job = cx.background_executor().spawn(async move {
            (|| -> Result<Library> {
                let mut library = Library::open(&home)?;
                if abandon {
                    local::abandon(
                        &home,
                        &mut library,
                        &d.project,
                        &d.work,
                        &d.run,
                        "操作者在 Studio 放弃未完成意图，重新预览",
                    )?;
                } else {
                    local::merge(
                        &home,
                        &mut library,
                        &d.project,
                        &d.work,
                        &d.run,
                        &target,
                        &digest,
                    )?;
                }
                Library::open(&home)
            })()
            .map_err(|e| format!("{e:#}"))
        });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |s, cx| {
                let valid = s
                    .board
                    .timeline
                    .as_ref()
                    .and_then(|t| t.merge.as_ref())
                    .is_some_and(|t| t.request == request);
                if !valid {
                    return;
                }
                match result {
                    Ok(lib) => {
                        s.board.library = lib;
                        s.load_timeline(cx);
                    }
                    Err(e) => {
                        if let Some(state) =
                            s.board.timeline.as_mut().and_then(|t| t.merge.as_mut())
                        {
                            state.busy = false;
                            state.error = Some(e);
                            state.confirming = false;
                        }
                        // Read back a persisted intent after a partial failure.
                        if let Some(home) = &s.board.dir
                            && let Ok(lib) = Library::open(home)
                        {
                            if let Some(d) = lib
                                .projects
                                .iter()
                                .flat_map(|p| &p.work)
                                .flat_map(|w| &w.deliveries)
                                .find(|v| v.run == run)
                                && let Some(state) =
                                    s.board.timeline.as_mut().and_then(|t| t.merge.as_mut())
                            {
                                state.delivery = d.clone();
                            }
                            s.board.library = lib;
                        }
                        cx.notify();
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }
    pub(super) fn work_merge(&self, state: &State, cx: &Context<Self>) -> Div {
        let mut body = v_flex().gap_3().child(
            h_flex()
                .justify_between()
                .child(heading("归档交付 · 本地合并"))
                .child(
                    button("work-merge-back", "返回时间线", false).on_click(cx.listener(
                        |s, _, _, cx| {
                            if let Some(t) = s.board.timeline.as_mut() {
                                t.merge = None;
                            }
                            cx.notify();
                        },
                    )),
                ),
        );
        body = body.child(caption("只更新本地分支，不会推送"));
        if let Some(e) = &state.error {
            body = body.child(t(e.clone(), AMBER));
        }
        if state.busy {
            return body.child(caption("正在校验交付与本地分支…"));
        }
        if let Some(r) = state.pending() {
            body = body.child(field("待处理意图", &format!("{} · {} → {}", r.target_branch, r.before_sha, r.candidate_sha)))
                .child(caption("分支已到候选时可恢复补记；仍在原顶端时可重试；变为其他提交时须放弃意图并重新预览。"))
                .child(button("work-merge-abandon", "放弃未完成意图并保留记录", false).on_click(cx.listener(|s, _, _, cx| s.confirm_work_merge(true, cx))));
        }
        body = body.child(
            button("work-merge-refresh", "重新查看 Diff", false)
                .on_click(cx.listener(|s, _, _, cx| s.preview_work_merge(None, cx))),
        );
        let Some(p) = &state.preview else {
            return body;
        };
        if let Some(r) = state.pending() {
            body = body.child(detail_text(
                "work-merge-recovery",
                r.recovery(&p.branch.tip).message(),
            ));
        }
        body = body
            .child(field("仓库", &p.candidate.repository.display().to_string()))
            .child(field("已验收提交", &p.candidate.sha))
            .child(field("来源接力执行", &p.candidate.transfer_run));
        for (i, b) in state
            .branches
            .iter()
            .enumerate()
            .filter(|(_, b)| state.internal || !b.internal)
        {
            let target = b.name.clone();
            let mut row = h_flex().gap_2();
            if state.pending().is_some() {
                row = row.child(caption(b.name.clone()));
            } else {
                row = row.child(
                    button(
                        format!("work-merge-target-{i}"),
                        b.name.clone(),
                        b.name == p.branch.name,
                    )
                    .on_click(cx.listener(move |s, _, _, cx| {
                        s.preview_work_merge(Some(target.clone()), cx)
                    })),
                );
            }
            row = row.child(caption(format!(
                "{}{}{}{}",
                b.reason,
                b.checked_out
                    .as_ref()
                    .map(|p| format!(" · 检出于 {}", p.display()))
                    .unwrap_or_default(),
                if b.internal {
                    " · AEP 内部分支"
                } else {
                    ""
                },
                b.merge_base
                    .as_ref()
                    .map(|s| format!(" · 分叉点 {s}"))
                    .unwrap_or_default()
            )));
            body = body.child(row);
        }
        body = body
            .child(
                button(
                    "work-merge-internal",
                    if state.internal {
                        "折叠 AEP 内部分支"
                    } else {
                        "展开 AEP 内部分支"
                    },
                    false,
                )
                .on_click(cx.listener(|s, _, _, cx| {
                    if let Some(s) = s.board.timeline.as_mut().and_then(|t| t.merge.as_mut()) {
                        s.internal = !s.internal;
                    }
                    cx.notify();
                })),
            )
            .child(
                field("目标分支", &p.branch.name)
                    .id("work-merge-selected-target")
                    .test_support()
                    .role(Role::Label)
                    .aria_label(p.branch.name.clone()),
            )
            .child(field("合并前顶端", &p.branch.tip))
            .child(field("Diff 摘要", &p.diff_sha256))
            .child(field(
                "变更统计",
                &format!(
                    "{} 个文件 · +{} / -{}",
                    p.files_changed, p.insertions, p.deletions
                ),
            ))
            .child(detail_text("work-merge-stat", &p.stat))
            .child(caption(if p.same_as_accepted {
                "最终 Diff 与归档已验收 Diff 相同"
            } else {
                "目标分支已包含部分变更或已前进；请核对最终 Diff"
            }));
        if p.truncated {
            body = body.child(t(
                "Diff 超过 96 KiB，当前显示已截断。可打开完整 patch。",
                AMBER,
            ));
        }
        let d = state.delivery.clone();
        let target = p.branch.name.clone();
        body = body.child(
            button("work-merge-patch", "打开完整 patch", false).on_click(cx.listener(
                move |s, _, _, cx| {
                    let result = (|| -> Result<PathBuf> {
                        let dir = tempfile::tempdir()?.keep();
                        let path = dir.join("changes.patch");
                        local::export_patch(
                            s.board.dir.as_deref().context("项目库")?,
                            &d,
                            &target,
                            &path,
                        )?;
                        Ok(path)
                    })();
                    match result {
                        Ok(path) => cx.reveal_path(&path),
                        Err(e) => s.board.error = Some(format!("{e:#}")),
                    }
                    cx.notify();
                },
            )),
        );
        for (i, section) in p
            .diff
            .split("diff --git ")
            .filter(|s| !s.is_empty())
            .enumerate()
        {
            let title = section.lines().next().unwrap_or("文件变更").to_owned();
            body = body.child(
                button(
                    format!("work-merge-file-{i}"),
                    title,
                    state.expanded.contains(&i),
                )
                .on_click(cx.listener(move |s, _, _, cx| {
                    if let Some(s) = s.board.timeline.as_mut().and_then(|t| t.merge.as_mut())
                        && !s.expanded.remove(&i)
                    {
                        s.expanded.insert(i);
                    }
                    cx.notify();
                })),
            );
            if state.expanded.contains(&i) {
                body = body.child(detail_text(
                    SharedString::from(format!("work-merge-diff-{i}")),
                    if section.contains("\nBinary files ") {
                        "二进制变更"
                    } else {
                        section
                    },
                ));
            }
        }
        if state.can_confirm() {
            if state.confirming {
                body = body
                    .child(t(
                        format!(
                            "确认将 {} 合入本地 {}（当前 {}），共 {} 个文件，+{} / -{}？",
                            p.candidate.sha,
                            p.branch.name,
                            p.branch.tip,
                            p.files_changed,
                            p.insertions,
                            p.deletions
                        ),
                        AMBER,
                    ))
                    .child(
                        primary("work-merge-confirm", "确认本地合并")
                            .on_click(cx.listener(|s, _, _, cx| s.confirm_work_merge(false, cx))),
                    );
            } else {
                body = body.child(primary("work-merge-review", "核对并确认合并").on_click(
                    cx.listener(|s, _, _, cx| {
                        if let Some(s) = s.board.timeline.as_mut().and_then(|t| t.merge.as_mut()) {
                            s.confirming = true;
                        }
                        cx.notify();
                    }),
                ));
            }
        } else if state
            .pending()
            .is_none_or(|r| r.recovery(&p.branch.tip) != Recovery::Changed)
        {
            body = body.child(t(p.branch.reason.clone(), AMBER));
        }
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    mod fixture {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../tests/support/work_item_merge.rs"
        ));
    }

    fn state(f: &fixture::Fixture) -> Result<State> {
        let preview = local::preview(&f.home, &f.delivery, "target")?;
        assert_eq!(preview.branch.tip, f.base);
        assert_eq!(preview.candidate.repository, f.repo.canonicalize()?);
        Ok(State {
            delivery: f.delivery.clone(),
            request: uuid::Uuid::new_v4(),
            busy: false,
            branches: vec![],
            selected_target: Some("target".into()),
            preview: Some(preview),
            error: None,
            confirming: false,
            internal: false,
            expanded: Default::default(),
        })
    }
    fn pending(s: &mut State) {
        let p = s.preview.as_ref().unwrap();
        s.delivery.merge = Some(local::MergeReceipt {
            status: MergeStatus::Pending,
            delivery_run: s.delivery.run.clone(),
            repository: p.candidate.repository.clone(),
            target_branch: p.branch.name.clone(),
            before_sha: p.branch.tip.clone(),
            candidate_sha: p.candidate.sha.clone(),
            merged_sha: None,
            mode: "update-ref".into(),
            diff_sha256: p.diff_sha256.clone(),
            files_changed: p.files_changed,
            insertions: p.insertions,
            deletions: p.deletions,
            intent_at: "now".into(),
            merged_at: None,
            operator: "test".into(),
            recovered: false,
        });
    }
    #[test]
    fn refresh_preserves_selection_even_after_preview_failure() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let f = fixture::create(tmp.path())?;
        let mut s = state(&f)?;
        assert_eq!(s.target(None).as_deref(), Some("target"));
        assert_eq!(s.target(Some("main".into())).as_deref(), Some("main"));
        s.preview = None;
        s.error = Some("preview failed".into());
        assert_eq!(s.target(None).as_deref(), Some("target"));
        assert!(!s.can_confirm());
        Ok(())
    }
    #[test]
    fn pending_locks_target_and_blocks_changed_tip_before_confirmation() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let f = fixture::create(tmp.path())?;
        let mut s = state(&f)?;
        pending(&mut s);
        assert_eq!(s.target(Some("main".into())).as_deref(), Some("target"));
        assert!(s.can_confirm());
        s.preview.as_mut().unwrap().branch.name = "main".into();
        assert!(!s.can_confirm());
        s.preview.as_mut().unwrap().branch.name = "target".into();
        s.preview.as_mut().unwrap().branch.tip = "other-tip".into();
        assert!(!s.can_confirm());
        assert_eq!(
            s.pending().unwrap().recovery("other-tip").message(),
            "目标分支在合并过程中被修改，请放弃意图后重新预览"
        );
        Ok(())
    }
    #[test]
    fn recovery_records_moved_candidate_but_retry_requires_writable_target() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let f = fixture::create(tmp.path())?;
        let mut s = state(&f)?;
        assert!(s.can_confirm());
        pending(&mut s);
        s.preview.as_mut().unwrap().branch.status = "unwritable".into();
        assert!(!s.can_confirm());
        s.preview.as_mut().unwrap().branch.tip = f.candidate;
        assert!(s.can_confirm());
        s.busy = true;
        assert!(!s.can_confirm());
        s.busy = false;
        s.delivery.merge.as_mut().unwrap().mode = "already_contained".into();
        assert!(!s.can_confirm());
        Ok(())
    }
}
