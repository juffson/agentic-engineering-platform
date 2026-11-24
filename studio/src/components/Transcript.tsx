import { memo, useEffect, useMemo, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { AlertTriangle, Check, ChevronDown, ChevronRight, CornerUpLeft, FileEdit, Square, Terminal, Wrench, X } from "lucide-react";
import type { AttemptSummary, AttemptView, Entry } from "../api/types";
import { clock, duration, parseTime, tokens } from "../lib/format";
import { isLive, modelLabel } from "../lib/work";
import { Spinner } from "./ui";

type Activity = Extract<Entry, { kind: "command" | "tool" | "edit" }>;
type Block =
  | { type: "entry"; entry: Exclude<Entry, Activity>; key: number }
  | { type: "group"; items: Activity[]; key: number };

function toBlocks(entries: Entry[]): Block[] {
  const blocks: Block[] = [];
  const notices = new Set<string>();
  entries.forEach((entry, i) => {
    if (entry.kind === "command" || entry.kind === "tool" || entry.kind === "edit") {
      const last = blocks[blocks.length - 1];
      if (last && last.type === "group") last.items.push(entry);
      else blocks.push({ type: "group", items: [entry], key: i });
      return;
    }
    if (entry.kind === "notice") {
      if (notices.has(entry.text)) return;
      notices.add(entry.text);
    }
    if ((entry.kind === "message" || entry.kind === "text" || entry.kind === "thinking") && !entry.text.trim()) return;
    blocks.push({ type: "entry", entry, key: i });
  });
  return blocks;
}

export function Turn({
  attempt,
  view,
  clientName,
  stageName,
}: {
  attempt: AttemptSummary;
  view: AttemptView | undefined;
  clientName: string;
  stageName: string;
}) {
  const live = isLive(attempt.status);
  const blocks = useMemo(() => toBlocks(view?.entries ?? []), [view?.entries]);
  return (
    <article className="turn" data-attempt={attempt.id} data-status={attempt.status}>
      <div className="turn-user">
        <div className={`bubble${attempt.prompt.trim() ? "" : " bubble-empty"}`}>
          {attempt.prompt.trim() || `按阶段职责执行「${stageName}」`}
        </div>
        <div className="turn-meta">
          {attempt.follow_up && <span>继续对话</span>}
          {attempt.subtask && <span>{attempt.subtask}</span>}
          <span>
            {clientName} · {modelLabel(view?.model ?? attempt.model)}
          </span>
          <span>{clock(attempt.started_at)}</span>
        </div>
      </div>
      <div className="turn-agent">
        {!view && <div className="entry-loading">{live ? "正在连接 Agent 输出…" : "正在读取记录…"}</div>}
        {blocks.map((b, i) =>
          b.type === "group" ? (
            <ActivityGroup key={b.key} items={b.items} live={live && i === blocks.length - 1} />
          ) : (
            <EntryBlock key={b.key} entry={b.entry} />
          ),
        )}
        <TurnStatus attempt={attempt} view={view} />
      </div>
    </article>
  );
}

const EntryBlock = memo(function EntryBlock({ entry }: { entry: Exclude<Entry, Activity> }) {
  switch (entry.kind) {
    case "message":
      return <Markdown text={entry.text} />;
    case "text":
      return <div className="entry-text">{entry.text}</div>;
    case "thinking":
      return <Thinking text={entry.text} />;
    case "error":
      return (
        <div className="entry-error" role="alert">
          <X size={14} />
          <span>{entry.text}</span>
        </div>
      );
    case "notice":
      return (
        <div className="entry-notice">
          <AlertTriangle size={12} />
          <span>{entry.text}</span>
        </div>
      );
  }
});

export const Markdown = memo(function Markdown({ text }: { text: string }) {
  return (
    <div className="md">
      <ReactMarkdown remarkPlugins={[remarkGfm]}>{text}</ReactMarkdown>
    </div>
  );
});

function Thinking({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  return (
    <button type="button" className={`entry-thinking${open ? " open" : ""}`} onClick={() => setOpen((o) => !o)} aria-expanded={open}>
      <span className="thinking-label">思考</span>
      <span className="thinking-text">{text}</span>
    </button>
  );
}

function summarize(items: Activity[]) {
  let commands = 0;
  let tools = 0;
  let failed = 0;
  const files = new Set<string>();
  for (const it of items) {
    if (it.kind === "command") {
      commands += 1;
      if (it.exit !== null && it.exit !== 0) failed += 1;
    } else if (it.kind === "tool") {
      tools += 1;
      if (it.failed) failed += 1;
    } else {
      it.files.forEach((f) => files.add(f));
    }
  }
  const parts: string[] = [];
  if (commands) parts.push(`运行了 ${commands} 条命令`);
  if (tools) parts.push(`调用 ${tools} 次工具`);
  if (files.size) parts.push(`改动 ${files.size} 个文件`);
  return { text: parts.join(" · "), failed };
}

function lastLabel(item: Activity): string {
  if (item.kind === "command") return item.command;
  if (item.kind === "tool") return `${item.name} ${item.detail}`;
  return item.files.join("  ");
}

function ActivityGroup({ items, live }: { items: Activity[]; live: boolean }) {
  const [open, setOpen] = useState(false);
  const { text, failed } = summarize(items);
  return (
    <div className={`activity${open ? " open" : ""}`}>
      <button type="button" className="activity-head" onClick={() => setOpen((o) => !o)} aria-expanded={open}>
        {open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
        <span className="activity-summary">{text}</span>
        {failed > 0 && <span className="badge badge-danger">{failed} 个失败</span>}
        {live && !open && (
          <span className="activity-live">
            <Spinner size={11} />
            <code>{lastLabel(items[items.length - 1])}</code>
          </span>
        )}
      </button>
      {open && (
        <ul className="activity-list">
          {items.map((it, i) => (
            <li key={i}>
              <ActivityItem item={it} />
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function ActivityItem({ item }: { item: Activity }) {
  const [open, setOpen] = useState(false);
  if (item.kind === "edit") {
    return (
      <div className="act-row act-edit">
        <FileEdit size={13} className="act-icon" />
        <span className="act-files">
          {item.files.map((f) => (
            <code key={f}>{f}</code>
          ))}
        </span>
      </div>
    );
  }
  const output = item.output.trimEnd();
  const isCommand = item.kind === "command";
  const bad = isCommand ? item.exit !== null && item.exit !== 0 : item.failed;
  return (
    <div className={`act-block${open ? " open" : ""}`}>
      <button
        type="button"
        className="act-row act-cmd"
        onClick={() => output && setOpen((o) => !o)}
        aria-expanded={output ? open : undefined}
        disabled={!output}
      >
        {isCommand ? <Terminal size={13} className="act-icon" /> : <Wrench size={13} className="act-icon" />}
        <code className="act-command">{isCommand ? item.command : `${item.name}  ${item.detail}`}</code>
        {isCommand ? (
          item.exit === null ? (
            <Spinner size={11} />
          ) : (
            <span className={`exit${bad ? " bad" : ""}`}>exit {item.exit}</span>
          )
        ) : (
          bad && <span className="exit bad">失败</span>
        )}
        <span className="act-toggle">{output ? (open ? "收起输出" : "输出") : "无输出"}</span>
      </button>
      {open && <pre className="act-output">{output}</pre>}
    </div>
  );
}

function useNow(active: boolean) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    if (!active) return;
    const t = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(t);
  }, [active]);
  return now;
}

function TurnStatus({ attempt, view }: { attempt: AttemptSummary; view: AttemptView | undefined }) {
  const live = isLive(attempt.status);
  const now = useNow(live);
  const start = parseTime(attempt.started_at);
  const end = parseTime(attempt.finished_at) ?? now;
  const took = start !== null ? duration(end - start) : null;
  const usage = view && (view.input_tokens || view.output_tokens) ? `${tokens(view.input_tokens)} 输入 · ${tokens(view.output_tokens)} 输出` : null;

  if (live) {
    return (
      <div className="turn-status live">
        <Spinner size={12} />
        <span>{attempt.status === "initializing" ? "正在启动" : "正在执行"}</span>
        {took && <span className="muted">· {took}</span>}
        {usage && <span className="muted">· {usage}</span>}
      </div>
    );
  }
  if (attempt.status === "succeeded") {
    return (
      <div className="turn-status ok">
        <Check size={13} />
        <span>完成</span>
        {took && <span className="muted">· 用时 {took}</span>}
        {usage && <span className="muted">· {usage}</span>}
      </div>
    );
  }
  if (attempt.status === "failed") {
    return (
      <div className="turn-status fail">
        <div className="turn-status-line">
          <X size={13} />
          <span>未通过</span>
          {took && <span className="muted">· 用时 {took}</span>}
        </div>
        {(attempt.error ?? view?.error) && <p className="turn-error">{attempt.error ?? view?.error}</p>}
        {attempt.route_back && (
          <p className="turn-routeback">
            <CornerUpLeft size={13} />
            <span>
              需要「{attempt.route_back.name}」先解决：{attempt.route_back.question}
            </span>
          </p>
        )}
      </div>
    );
  }
  if (attempt.status === "cancelled") {
    return (
      <div className="turn-status muted">
        <Square size={11} />
        <span>已停止</span>
        {took && <span>· {took}</span>}
      </div>
    );
  }
  return (
    <div className="turn-status muted">
      <span>{attempt.status || "未知状态"}</span>
    </div>
  );
}
