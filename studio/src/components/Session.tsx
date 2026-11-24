import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { ArrowDown, ArrowUp, Play, RotateCcw, Square } from "lucide-react";
import type { AttemptView, StageDetail, WorkDetail } from "../api/types";
import { api } from "../api/client";
import { useStudio } from "../state/store";
import { clientLabel, isLive, needsRecheck } from "../lib/work";
import { Turn } from "./Transcript";
import { Button } from "./ui";

export function Session({
  detail,
  stage,
  views,
  isCurrent,
}: {
  detail: WorkDetail;
  stage: StageDetail;
  views: Record<string, AttemptView>;
  isCurrent: boolean;
}) {
  const { snapshot } = useStudio();
  const scrollRef = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const [showJump, setShowJump] = useState(false);
  const running = stage.attempts.find((a) => a.id === detail.running) ?? null;
  const live = !!running || stage.attempts.some((a) => isLive(a.status));

  // A fingerprint of everything that changes the scroll height.
  const fingerprint = stage.attempts
    .map((a) => `${a.id}:${a.status}:${views[a.id]?.entries.length ?? -1}`)
    .join("|");

  useEffect(() => {
    stick.current = true;
    setShowJump(false);
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [detail.id, stage.index]);

  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [fingerprint]);

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
    stick.current = atBottom;
    setShowJump(!atBottom && live);
  };

  const jump = () => {
    const el = scrollRef.current;
    if (!el) return;
    stick.current = true;
    el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
    setShowJump(false);
  };

  const showComposer = isCurrent && !detail.delivered && !detail.shelved && stage.state !== "skipped";

  return (
    <section className="session" aria-label="阶段会话">
      <div className="session-scroll" ref={scrollRef} onScroll={onScroll}>
        <div className="session-inner">
          <header className="stage-intro">
            <div className="stage-intro-title">
              <span className="stage-intro-index">阶段 {stage.index + 1}</span>
              <h2>{stage.name}</h2>
            </div>
            <p>{stage.brief}</p>
          </header>
          {stage.state === "skipped" ? (
            <div className="session-empty">
              <p className="session-empty-title">此阶段已跳过</p>
              <p>{stage.skipped_reason || "未填写原因"}</p>
            </div>
          ) : stage.attempts.length === 0 ? (
            <div className="session-empty">
              <p className="session-empty-title">{isCurrent ? "还没有运行" : "此阶段尚未开始"}</p>
              <p>
                {isCurrent
                  ? "Agent 会在共享工作区里按阶段职责执行。可以在下方补充说明，或直接运行。"
                  : "前面的阶段完成后才会轮到它。"}
              </p>
            </div>
          ) : (
            stage.attempts.map((a) => (
              <Turn key={a.id} attempt={a} view={views[a.id]} clientName={clientLabel(snapshot, a.client)} stageName={stage.name} />
            ))
          )}
        </div>
      </div>
      {showJump && (
        <button type="button" className="jump-latest" onClick={jump}>
          <ArrowDown size={13} />
          跳到最新输出
        </button>
      )}
      {showComposer && <Composer key={`${detail.id}:${stage.index}`} detail={detail} stage={stage} runningId={running?.id ?? detail.running} />}
    </section>
  );
}

function Composer({ detail, stage, runningId }: { detail: WorkDetail; stage: StageDetail; runningId: string | null }) {
  const { act } = useStudio();
  const fresh = stage.attempts.length === 0;
  const recheck = needsRecheck(stage);
  const [text, setText] = useState(fresh ? stage.note : "");
  const [busy, setBusy] = useState(false);
  const ref = useRef<HTMLTextAreaElement>(null);
  const running = !!runningId;

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 220)}px`;
  }, [text]);

  const submit = async () => {
    if (running || busy) return;
    const project = detail.project;
    const work = detail.id;
    setBusy(true);
    if (fresh) {
      const ok = await act(async () => {
        if (text !== stage.note) await api.updateStage({ project, work, stage: stage.index, note: text });
        return api.runStage({ project, work, stage: stage.index });
      });
      if (ok !== undefined) setText("");
    } else if (recheck) {
      const message = text.trim() || undefined;
      const ok = await act(() => api.runStage({ project, work, stage: stage.index, message }));
      if (ok !== undefined) setText("");
    } else {
      if (!text.trim()) {
        setBusy(false);
        ref.current?.focus();
        return;
      }
      const ok = await act(() => api.followUp({ project, work, stage: stage.index, message: text.trim() }));
      if (ok !== undefined) setText("");
    }
    setBusy(false);
  };

  const stop = () => {
    if (runningId) void act(() => api.cancel({ project: detail.project, work: detail.id, attempt: runningId }));
  };

  return (
    <div className={`composer${running ? " is-running" : ""}`}>
      <textarea
        ref={ref}
        value={text}
        rows={fresh ? 3 : 2}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
            e.preventDefault();
            void submit();
          }
        }}
        disabled={running}
        aria-label={fresh ? "阶段说明" : "继续对话"}
        placeholder={
          running
            ? "Agent 正在执行，完成后可以继续对话…"
            : fresh
              ? "给这个阶段的说明（可留空，按阶段职责执行）"
              : recheck
                ? `前序阶段已修改。可补充复核要点（可留空），「${stage.name}」会在原会话里复核上次的问题`
                : `继续和「${stage.name}」的 Agent 对话，例如：补充单元测试，或解释某处改动`
        }
      />
      <div className="composer-bar">
        <span className="composer-hint">{running ? "正在执行" : "⌘ / Ctrl + Enter 发送"}</span>
        {running ? (
          <Button variant="secondary" icon={<Square size={12} />} onClick={stop}>
            停止
          </Button>
        ) : fresh ? (
          <Button variant="primary" icon={<Play size={13} />} onClick={submit} busy={busy}>
            运行「{stage.name}」
          </Button>
        ) : recheck ? (
          <Button variant="primary" icon={<RotateCcw size={13} />} onClick={submit} busy={busy}>
            复核「{stage.name}」
          </Button>
        ) : (
          <Button variant="primary" icon={<ArrowUp size={14} />} onClick={submit} busy={busy} disabled={!text.trim()}>
            继续对话
          </Button>
        )}
      </div>
    </div>
  );
}
