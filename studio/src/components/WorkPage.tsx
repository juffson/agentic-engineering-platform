import { useCallback, useEffect, useRef, useState } from "react";
import { AlertTriangle, Check, CornerUpLeft, GitBranch, Square, X, Zap } from "lucide-react";
import type { AttemptView, ItemHandoff, WorkDetail } from "../api/types";
import { api, errorMessage } from "../api/client";
import { useStudio, usePoll } from "../state/store";
import { clientLabel, isLive, modelLabel, stageVisual } from "../lib/work";
import { sessionLabel, stageSession } from "../lib/format";
import { VIEW_ATTEMPT } from "./Transcript";
import { Session } from "./Session";
import { DecisionPanel } from "./DecisionPanel";
import { AutopilotDrawer } from "./AutopilotDrawer";
import { HistoryView } from "./History";
import { Button, Spinner } from "./ui";

type Tab = "stages" | "history";

export function WorkPage({ project, work }: { project: string; work: string }) {
  const { revision, notify } = useStudio();
  const [detail, setDetail] = useState<WorkDetail | null>(null);
  const [views, setViews] = useState<Record<string, AttemptView>>({});
  const [viewed, setViewed] = useState<number | null>(null);
  const [drawer, setDrawer] = useState(false);
  const [tab, setTab] = useState<Tab>("stages");
  const [handoffs, setHandoffs] = useState<ItemHandoff[] | null>(null);
  const [handoffRev, setHandoffRev] = useState(0);

  // Hand-offs change rarely: fetch on open and after mutations, not every second.
  useEffect(() => {
    let alive = true;
    api
      .handoffs({ project, work })
      .then((h) => alive && setHandoffs(h))
      .catch(() => alive && setHandoffs([]));
    return () => {
      alive = false;
    };
  }, [project, work, revision, handoffRev]);
  const viewsRef = useRef(views);
  viewsRef.current = views;
  const viewedRef = useRef(viewed);
  viewedRef.current = viewed;
  const lastError = useRef<string | null>(null);

  const poll = useCallback(async () => {
    let d: WorkDetail;
    try {
      d = await api.workDetail({ project, work });
      lastError.current = null;
    } catch (e) {
      const text = errorMessage(e);
      if (lastError.current !== text) notify(`读取事项失败：${text}`);
      lastError.current = text;
      return;
    }
    setDetail(d);
    const index = Math.min(viewedRef.current ?? d.stage, d.stages.length - 1);
    const stage = d.stages[index];
    if (!stage) return;
    const cache = viewsRef.current;
    const wanted = stage.attempts.filter((a) => isLive(a.status) || a.id === d.running || cache[a.id]?.status !== a.status);
    if (wanted.length === 0) return;
    const results = await Promise.allSettled(wanted.map((a) => api.attemptView({ project, work, attempt: a.id })));
    setViews((prev) => {
      const next = { ...prev };
      results.forEach((r, i) => {
        if (r.status === "fulfilled") {
          // Keep the summary's status as the cache key so finished runs are fetched once more.
          next[wanted[i].id] = { ...r.value, status: isLive(r.value.status) ? r.value.status : wanted[i].status };
        }
      });
      return next;
    });
  }, [project, work, notify]);

  usePoll(poll, 1000, [poll, revision, viewed]);

  // "复用自「…」" on a turn: show that stage and bring the reused turn into view.
  const detailRef = useRef(detail);
  detailRef.current = detail;
  useEffect(() => {
    let timer = 0;
    const onView = (e: Event) => {
      const { stage, attempt } = (e as CustomEvent<{ stage: number; attempt?: string }>).detail;
      setTab("stages");
      setViewed(stage === detailRef.current?.stage ? null : stage);
      if (!attempt) return;
      let tries = 0;
      window.clearInterval(timer);
      timer = window.setInterval(() => {
        const el = document.querySelector<HTMLElement>(`.turn[data-attempt="${CSS.escape(attempt)}"]`);
        if (el || ++tries > 40) window.clearInterval(timer);
        if (!el) return;
        el.scrollIntoView({ block: "start", behavior: "smooth" });
        el.classList.add("turn-flash");
        window.setTimeout(() => el.classList.remove("turn-flash"), 1600);
      }, 100);
    };
    window.addEventListener(VIEW_ATTEMPT, onView);
    return () => {
      window.removeEventListener(VIEW_ATTEMPT, onView);
      window.clearInterval(timer);
    };
  }, []);

  // Follow the current stage when it moves (advance / send back) unless the user pinned another stage.
  const lastStage = useRef<number | null>(null);
  useEffect(() => {
    if (!detail) return;
    if (lastStage.current !== null && lastStage.current !== detail.stage) setViewed(null);
    lastStage.current = detail.stage;
  }, [detail?.stage]); // eslint-disable-line react-hooks/exhaustive-deps

  if (!detail) {
    return (
      <div className="page-loading">
        <Spinner size={16} />
      </div>
    );
  }

  const currentIndex = Math.min(detail.stage, detail.stages.length - 1);
  const viewIndex = viewed ?? currentIndex;
  const stage = detail.stages[viewIndex];
  const isCurrent = viewIndex === detail.stage;
  const current = detail.stages[detail.stage];

  return (
    <div className="work-page">
      <WorkHeader detail={detail} onAutopilot={() => setDrawer(true)} tab={tab} onTab={setTab} received={handoffs?.length ?? 0} />
      {!detail.flow_error && (
        <Pipeline
          detail={detail}
          viewIndex={tab === "stages" ? viewIndex : -1}
          onView={(i) => {
            setTab("stages");
            setViewed(i === detail.stage ? null : i);
          }}
        />
      )}
      {tab === "history" ? (
        <HistoryView detail={detail} handoffs={handoffs} onHandoffSent={() => setHandoffRev((r) => r + 1)} />
      ) : detail.flow_error ? (
        <section className="card work-flow-error" role="alert">
          <AlertTriangle size={16} />
          <div>
            <p className="card-title">这个事项的流程数据无法读取：{detail.flow_error}</p>
            <p className="muted">事项的执行记录和工作区不受影响。</p>
          </div>
        </section>
      ) : (
        <>
          {!isCurrent && current && stage && !detail.delivered && (
            <div className="view-banner">
              <span>
                正在查看{stage.state === "done" ? "已完成的" : stage.state === "skipped" ? "已跳过的" : "后续的"}「{stage.name}」
              </span>
              <button type="button" className="link-btn" onClick={() => setViewed(null)}>
                回到当前阶段「{current.name}」
              </button>
            </div>
          )}
          <div className={`work-body${detail.delivered && !detail.shelved ? " wide" : ""}`}>
            {stage ? <Session detail={detail} stage={stage} views={views} isCurrent={isCurrent} /> : <div />}
            <DecisionPanel detail={detail} viewed={stage} />
          </div>
        </>
      )}
      {drawer && !detail.flow_error && <AutopilotDrawer detail={detail} onClose={() => setDrawer(false)} />}
    </div>
  );
}

function flowTitle(detail: WorkDetail): string | undefined {
  const { flow } = detail;
  if (!flow) return undefined;
  const count = `${detail.stages.length} 个阶段`;
  if (flow.builtin) return `内置流程 · ${count}`;
  if (flow.source_state === "changed" || flow.source_state === "deleted") {
    const change = flow.source_state === "changed" ? "之后已修改" : "已删除";
    return `按创建时的流程推进（${count}）；项目里的「${flow.label}」${change}`;
  }
  return `本项目流程 · ${count}`;
}

function WorkHeader({
  detail,
  onAutopilot,
  tab,
  onTab,
  received,
}: {
  detail: WorkDetail;
  onAutopilot(): void;
  tab: Tab;
  onTab(tab: Tab): void;
  received: number;
}) {
  const { act } = useStudio();
  const ap = detail.autopilot;
  const reveal = () => {
    const path = detail.worktrees[0];
    if (path) void act(() => api.reveal({ path }));
  };
  return (
    <header className="work-header">
      <div className="work-title">
        <h1 title={detail.name}>{detail.name}</h1>
        <div className="work-meta">
          <span className="tag">{detail.kind === "Release" ? "发布" : "功能"}</span>
          <span className="meta-text" title={flowTitle(detail)}>{detail.flow?.label ?? detail.template_label}</span>
          {detail.branch && (
            <button
              type="button"
              className="branch-chip"
              onClick={reveal}
              disabled={detail.worktrees.length === 0}
              title={detail.worktrees[0] ? `在访达中显示 ${detail.worktrees[0]}` : "工作区已移除"}
            >
              <GitBranch size={12} />
              {detail.branch.replace(/^(aep-stage\/[0-9a-f]{8})[0-9a-f]+$/, "$1")}
            </button>
          )}
          {detail.delivered && !detail.shelved && <span className="tag tag-ok">已交付</span>}
          {detail.shelved && <span className="tag">已归档</span>}
        </div>
      </div>
      <div className="work-actions">
        <div className="view-tabs" role="tablist" aria-label="视图">
          <button type="button" role="tab" aria-selected={tab === "stages"} className={tab === "stages" ? "on" : ""} onClick={() => onTab("stages")}>
            阶段
          </button>
          <button type="button" role="tab" aria-selected={tab === "history"} className={tab === "history" ? "on" : ""} onClick={() => onTab("history")}>
            历史
            {received > 0 && (
              <span className="tab-badge" aria-label={`收到 ${received} 条交接`}>
                {received}
              </span>
            )}
          </button>
        </div>
        {!detail.delivered &&
          (ap.running ? (
            <div className="ap-live" data-testid="autopilot-live">
              <Spinner size={13} />
              <span className="ap-live-text" title={ap.message ?? ""}>
                {ap.message ?? "自动运行中"}
              </span>
              <Button variant="secondary" size="sm" icon={<Square size={11} />} onClick={() => act(() => api.autopilotStop({ project: detail.project, work: detail.id }))}>
                停止
              </Button>
            </div>
          ) : (
            <Button variant="secondary" icon={<Zap size={14} />} onClick={onAutopilot} disabled={!!detail.flow_error}>
              自动运行…
            </Button>
          ))}
      </div>
    </header>
  );
}

function Pipeline({ detail, viewIndex, onView }: { detail: WorkDetail; viewIndex: number; onView(i: number): void }) {
  const { snapshot } = useStudio();
  return (
    <nav className="pipeline" aria-label="阶段">
      <ol>
        {detail.stages.map((s, i) => {
          const v = stageVisual(detail, s);
          // Session of the latest passed run: "↻ 复用" in the caption, the full label on hover.
          const passed = v === "skipped" ? null : stageSession(s);
          const session = sessionLabel(passed);
          const reused = passed?.kind === "reused";
          const caption = v === "skipped" ? "已跳过" : `${clientLabel(snapshot, s.profile.client)} · ${modelLabel(s.profile.model)}${reused ? " · ↻ 复用" : ""}`;
          return (
            <li key={s.index} className={`step step-${v}${i === viewIndex ? " viewed" : ""}`}>
              <button type="button" onClick={() => onView(i)} aria-current={i === viewIndex ? "step" : undefined} title={`${s.name} · ${caption}${session ? `\n${session.text}` : ""}`}>
                <span className="step-marker">
                  {v === "done" ? (
                    <Check size={12} strokeWidth={2.5} />
                  ) : v === "running" ? (
                    <span className="step-pulse" />
                  ) : v === "failed" ? (
                    <X size={12} strokeWidth={2.5} />
                  ) : (
                    i + 1
                  )}
                </span>
                <span className="step-text">
                  <span className="step-name">
                    {s.name}
                    {detail.reworks.some((r) => r.to === i) && v !== "upcoming" && <CornerUpLeft size={11} className="step-rework" aria-label="有退回" />}
                  </span>
                  {reused ? (
                    // The model name may be cut off; "↻ 复用" stays visible.
                    <span className="step-caption">
                      <span className="step-caption-text">
                        {clientLabel(snapshot, s.profile.client)} · {modelLabel(s.profile.model)}
                      </span>
                      <span className="step-reuse"> · ↻ 复用</span>
                    </span>
                  ) : (
                    <span className="step-caption">{caption}</span>
                  )}
                </span>
              </button>
            </li>
          );
        })}
      </ol>
    </nav>
  );
}
