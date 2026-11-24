import { useEffect, useMemo, useState } from "react";
import { AlertTriangle, ArrowDownToLine, ChevronDown, ChevronRight, GitMerge, Inbox, Send } from "lucide-react";
import type { HistoryNote, HistoryRound, ItemHandoff, WorkDetail, WorkHistory } from "../api/types";
import { api, errorMessage } from "../api/client";
import { useStudio } from "../state/store";
import { clock } from "../lib/format";
import { isLive, modelLabel } from "../lib/work";
import { Button, SectionLabel, Spinner } from "./ui";

const NOTE_KIND: Record<string, string> = {
  decision: "决策",
  handoff: "交接",
  review: "评审",
  open_question: "待定问题",
  note: "笔记",
};

const STATUS: Record<string, string> = {
  succeeded: "完成",
  failed: "未通过",
  cancelled: "已停止",
  running: "运行中",
  initializing: "启动中",
  claimed: "运行中",
};

/** Changes whenever something that affects the history changes; avoids per-second refetching. */
export function historyKey(detail: WorkDetail): string {
  return [
    detail.stage,
    detail.stages.map((s) => s.attempts.map((a) => `${a.id}:${a.status}`).join(",")).join("|"),
    detail.deliveries.map((d) => d.merged?.sha ?? "").join(","),
  ].join("#");
}

export function HistoryView({
  detail,
  handoffs,
  onHandoffSent,
}: {
  detail: WorkDetail;
  handoffs: ItemHandoff[] | null;
  onHandoffSent(): void;
}) {
  const [history, setHistory] = useState<WorkHistory | null>(null);
  const [error, setError] = useState<string | null>(null);
  const key = historyKey(detail);

  useEffect(() => {
    let alive = true;
    api
      .history({ project: detail.project, work: detail.id })
      .then((h) => {
        if (!alive) return;
        setHistory(h);
        setError(null);
      })
      .catch((e) => alive && setError(errorMessage(e)));
    return () => {
      alive = false;
    };
  }, [detail.project, detail.id, key]);

  return (
    <div className="history-scroll">
      <div className="history-inner">
        <HandoffSection detail={detail} handoffs={handoffs} onSent={onHandoffSent} />

        <SectionLabel right={history && <span className="muted">{history.rounds.length} 次运行</span>}>运行记录</SectionLabel>
        {error && <p className="inline-error">{error}</p>}
        {!history && !error && <Spinner />}
        {history && history.rounds.length === 0 && <p className="empty-text">还没有运行记录。</p>}
        {history && history.rounds.length > 0 && (
          <ol className="timeline">
            {history.rounds.map((r, i) => (
              <RoundItem key={`${r.run}-${i}`} round={r} stages={detail.stages.map((s) => s.name)} />
            ))}
          </ol>
        )}

        {history && history.merges.length > 0 && (
          <>
            <SectionLabel>合并记录</SectionLabel>
            <ul className="plain-list">
              {history.merges.map((m) => (
                <li key={m.sha} className="merge-row">
                  <GitMerge size={13} className="ok-icon" />
                  <span>
                    合并到 <code>{m.target}</code> · <code>{m.sha.slice(0, 8)}</code>
                  </span>
                  <span className="muted">{m.files_changed} 个文件</span>
                  {m.at && <span className="muted merge-at">{clock(m.at)}</span>}
                </li>
              ))}
            </ul>
          </>
        )}

        {history && history.errors.length > 0 && (
          <>
            <SectionLabel>读取问题</SectionLabel>
            <ul className="plain-list history-errors">
              {history.errors.map((e, i) => (
                <li key={i}>
                  <AlertTriangle size={12} />
                  <span>{e}</span>
                </li>
              ))}
            </ul>
          </>
        )}
      </div>
    </div>
  );
}

function RoundItem({ round, stages }: { round: HistoryRound; stages: string[] }) {
  const [open, setOpen] = useState(false);
  const live = isLive(round.status);
  const tone = live ? "running" : round.status === "succeeded" ? "ok" : round.status === "failed" ? "fail" : "muted";
  return (
    <li className={`tl-item tl-${tone}`}>
      <span className="tl-dot" aria-hidden />
      <button type="button" className="tl-head" onClick={() => setOpen((o) => !o)} aria-expanded={open}>
        {open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
        <span className="tl-stage">{round.stage_name}</span>
        <span className="tl-round">
          {round.round !== null && `第 ${round.round} 轮 · `}第 {round.attempt} 次
        </span>
        <span className="tl-agents">
          {round.agents.map((a, i) => (
            <span key={i}>
              {a.client} · {modelLabel(a.model)}
            </span>
          ))}
        </span>
        <span className={`tl-status tl-status-${tone}`}>
          {live && <Spinner size={10} />}
          {STATUS[round.status] ?? round.status}
        </span>
      </button>
      {open && (
        <div className="tl-body">
          {round.received && (
            <div className="received">
              <p className="received-head">
                <ArrowDownToLine size={12} />
                从「{stages[round.received.from_stage] ?? `阶段 ${round.received.from_stage + 1}`}」带入
              </p>
              {round.received.decision && (
                <p>
                  <span className="muted">决定 · </span>
                  {round.received.decision}
                </p>
              )}
              {round.received.acceptance && (
                <p>
                  <span className="muted">确认 · </span>
                  {round.received.acceptance}
                </p>
              )}
            </div>
          )}
          {round.agents.map((a, i) => (
            <div key={i} className="tl-agent">
              {round.agents.length > 1 && (
                <p className="tl-agent-head">
                  {a.role} · {a.client} · {modelLabel(a.model)}
                </p>
              )}
              <NoteGroup title="读取的记忆" notes={a.reads} />
              <NoteGroup title="写入的记忆" notes={a.writes} />
              {a.handoff && (
                <div className="note-group">
                  <p className="note-group-title">交接</p>
                  <p className="tl-handoff">{a.handoff}</p>
                </div>
              )}
              {a.session && <p className="tl-session">会话 {a.session} · 运行 {round.run}</p>}
            </div>
          ))}
        </div>
      )}
    </li>
  );
}

function NoteGroup({ title, notes }: { title: string; notes: HistoryNote[] }) {
  if (notes.length === 0) return null;
  return (
    <div className="note-group">
      <p className="note-group-title">
        {title} <span className="muted">{notes.length}</span>
      </p>
      <div className="note-cards">
        {notes.map((n) => (
          <NoteCard key={`${n.id}-${n.kind}`} note={n} />
        ))}
      </div>
    </div>
  );
}

function NoteCard({ note }: { note: HistoryNote }) {
  const [open, setOpen] = useState(false);
  return (
    <button type="button" className={`note-card${open ? " open" : ""}`} onClick={() => setOpen((o) => !o)} aria-expanded={open}>
      <span className="note-card-meta">
        <span className={`tag note-kind-${note.kind}`}>{NOTE_KIND[note.kind] ?? note.kind}</span>
        <span className="muted">#{note.id}</span>
      </span>
      <span className="note-card-body">{note.body}</span>
    </button>
  );
}

function HandoffSection({ detail, handoffs, onSent }: { detail: WorkDetail; handoffs: ItemHandoff[] | null; onSent(): void }) {
  const { project, act } = useStudio();
  const others = useMemo(() => (project?.work ?? []).filter((w) => w.id !== detail.id), [project, detail.id]);
  const [to, setTo] = useState("");
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const target = to || others.find((w) => w.group === "active")?.id || others[0]?.id || "";
  const over = note.length > 500;

  const send = async () => {
    if (!target) return;
    setBusy(true);
    const ok = await act(() => api.sendHandoff({ project: detail.project, work: detail.id, to: target, note: note.trim() }), "交接已发送");
    setBusy(false);
    if (ok !== undefined) {
      setNote("");
      onSent();
    }
  };

  return (
    <section className="card handoff-card" data-testid="handoffs">
      <div className="card-head">
        <span className="card-title">事项交接</span>
      </div>
      {handoffs === null ? (
        <Spinner size={12} />
      ) : handoffs.length === 0 ? (
        <p className="card-text">
          <Inbox size={13} className="inline-icon" />
          没有收到其他事项的交接。
        </p>
      ) : (
        <ul className="handoff-list">
          {handoffs.map((h) => (
            <li key={h.from} className="handoff">
              <p className="handoff-from">
                来自「{h.name}」<span className="muted"> · {h.stage}</span>
              </p>
              <p className="handoff-note">{h.note}</p>
            </li>
          ))}
        </ul>
      )}
      <div className="handoff-form">
        <div className="handoff-to">
          <span className="picker-label">交接给</span>
          <select className="input input-sm" value={target} onChange={(e) => setTo(e.target.value)} aria-label="交接给" disabled={others.length === 0}>
            {others.map((w) => (
              <option key={w.id} value={w.id}>
                {w.name} · {w.stage_name}
              </option>
            ))}
          </select>
        </div>
        <textarea
          className="input"
          rows={2}
          value={note}
          onChange={(e) => setNote(e.target.value)}
          placeholder="给另一个事项留一段简短交接，例如：共享的接口约定或需要注意的坑"
          aria-label="交接内容"
        />
        <div className="row-between">
          <span className={`count${over ? " over" : ""}`}>{note.length}/500</span>
          <Button variant="secondary" size="sm" icon={<Send size={12} />} disabled={!target || !note.trim() || over} busy={busy} onClick={send}>
            发送交接
          </Button>
        </div>
      </div>
    </section>
  );
}
