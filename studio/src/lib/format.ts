export function parseTime(value: string | null | undefined): number | null {
  if (!value) return null;
  const t = Date.parse(value);
  return Number.isNaN(t) ? null : t;
}

const pad = (n: number) => String(n).padStart(2, "0");

/** "14:32" today, "11-20 14:32" otherwise. */
export function clock(value: string | null | undefined): string {
  const t = parseTime(value);
  if (t === null) return "";
  const d = new Date(t);
  const now = new Date();
  const hm = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  if (d.toDateString() === now.toDateString()) return hm;
  if (d.getFullYear() === now.getFullYear()) return `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${hm}`;
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

export function duration(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s} 秒`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} 分 ${pad(s % 60)} 秒`;
  return `${Math.floor(m / 60)} 小时 ${pad(m % 60)} 分`;
}

export function relative(value: string | null | undefined): string {
  const t = parseTime(value);
  if (t === null) return "";
  const diff = Date.now() - t;
  if (diff < 60_000) return "刚刚";
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)} 分钟前`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)} 小时前`;
  if (diff < 7 * 86_400_000) return `${Math.floor(diff / 86_400_000)} 天前`;
  return clock(value);
}

export function tokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(n >= 100_000_000 ? 0 : 1)}M`;
  if (n >= 1000) return `${(n / 1000).toFixed(n >= 100_000 ? 0 : 1)}k`;
  return String(n);
}

/** "2M", "500k", "1.5m", "300000" → tokens; "" → null; NaN when unreadable. */
export function parseTokens(text: string): number | null {
  const t = text.trim().toLowerCase().replace(/,/g, "");
  if (!t) return null;
  const m = /^(\d+(?:\.\d+)?)\s*([km万]?)$/.exec(t);
  if (!m) return NaN;
  const scale = m[2] === "m" ? 1_000_000 : m[2] === "k" ? 1000 : m[2] === "万" ? 10_000 : 1;
  return Math.round(parseFloat(m[1]) * scale);
}

export function usd(n: number | null | undefined): string {
  if (n === null || n === undefined) return "";
  return n >= 100 ? `$${n.toFixed(0)}` : `$${n.toFixed(2)}`;
}

/** "182K", "1.2M": the token figures used in session and launch labels. */
export function tokensK(n: number): string {
  return tokens(n).replace(/\.0(?=[kM]$)/, "").replace(/k$/, "K");
}

/** Fields shared by a run's session label and the next run's preview (interaction §2.1). */
interface SessionLike {
  kind: "reused" | "continued" | "fresh";
  stage_name?: string | null;
  reason?: string;
  context?: number | null;
  threshold?: number | null;
  prior?: string | null;
}

const PERMISSION: Record<string, string> = { read: "只读", write: "可写", gate: "门禁", Standard: "默认沙箱", Network: "允许网络", Full: "完全权限" };

/** Why a run starts a new session, e.g. "模型不同（前序 opus）"; "" for the first run. */
export function freshReason(s: Pick<SessionLike, "reason" | "context" | "threshold" | "prior">): string {
  const prior = s.prior ?? null;
  switch (s.reason) {
    case "context_over_threshold":
      return s.context != null && s.threshold != null ? `上下文超过阈值（${tokensK(s.context)} > ${tokensK(s.threshold)}）` : "上下文超过阈值";
    case "client_differs":
      return `CLI 不同${prior ? `（前序 ${prior}）` : ""}`;
    case "model_differs":
      return `模型不同（前序 ${prior?.trim() ? prior : "默认模型"}）`;
    case "permission_differs":
      return `权限不同${prior ? `（前序${PERMISSION[prior] ?? ` ${prior}`}）` : ""}`;
    case "workdir_differs":
      return "工作目录不同";
    case "session_unavailable":
      return "前序会话无法续接";
    case "prior_budget_stopped":
      return "前序阶段被预算停下";
    case "session_busy":
      return "前序会话正在使用";
    case "chain_broken":
      return "中间阶段用了其他 CLI";
    case "prior_not_passed":
      return "前序阶段未通过";
    case "independent_review":
      return "独立验收不沿用测试的会话";
    default:
      return "";
  }
}

/** New sessions that carry the last answer and the hand-off summary instead (R2, E4). */
export function carriesSummary(s: { kind: string; reason?: string } | null | undefined): boolean {
  return !!s && s.kind === "fresh" && (s.reason === "context_over_threshold" || s.reason === "session_unavailable");
}

/** One run's session label (interaction §2.1); null for old records and ZCode / custom CLIs. */
export function sessionLabel(s: SessionLike | null | undefined): { text: string; tone: "reuse" | "continue" | "fresh" } | null {
  if (!s) return null;
  if (s.kind === "reused") return { text: `复用自「${s.stage_name ?? "前序阶段"}」的会话`, tone: "reuse" };
  if (s.kind === "continued") return { text: "继续本阶段会话", tone: "continue" };
  const why = freshReason(s);
  return { text: why ? `新会话 · ${why}` : "新会话", tone: "fresh" };
}

/** The next run's session, said before it runs (interaction §4.1). */
export function sessionPreviewText(s: SessionLike | null | undefined): string | null {
  if (!s) return null;
  if (s.kind === "reused") return `↻ 将复用「${s.stage_name ?? "前序阶段"}」的会话 · 只发本阶段职责和说明`;
  if (s.reason === "context_over_threshold" && s.context != null && s.threshold != null)
    return `将开新会话 · 前序会话上下文 ${tokensK(s.context)} 超过上限 ${tokensK(s.threshold)}`;
  const why = freshReason(s);
  return why ? `将开新会话 · ${why}` : null;
}

/** The short form for compact rows: "↻ 复用" or "新会话 · 权限不同". */
export function sessionShort(s: SessionLike | null | undefined): string | null {
  if (!s) return null;
  if (s.kind === "reused") return "↻ 复用";
  if (s.kind !== "fresh") return null;
  const why = freshReason(s).replace(/（.*$/, "");
  return why ? `新会话 · ${why}` : null;
}

interface LaunchLike {
  diet: number;
  minimal?: boolean;
  not_minimized?: string[];
  version?: string;
  mcp_servers?: number;
  skills?: number;
  plugins?: number;
  builtin_plugins?: string[];
  first_context?: number | null;
}

/**
 * A run's launch configuration (interaction §2.2): "最小启动 · MCP 0 · skills 0 · 插件 5 · 首轮 9.8K".
 * Claude without a launch record ran before the diet; other CLIs without init stats say nothing.
 */
export function launchLabel(launch: LaunchLike | null | undefined, client: string): { text: string; title: string; tone: "ok" | "warn" | "old" } | null {
  if (!launch) return client === "Claude" ? { text: "优化前", title: "这次执行在最小启动之前，带着全部 MCP、skills 和插件", tone: "old" } : null;
  if (launch.mcp_servers === undefined && launch.plugins === undefined) return null;
  const first = launch.first_context != null ? tokensK(launch.first_context) : "—";
  const missing = launch.not_minimized ?? [];
  const counts = `MCP ${launch.mcp_servers ?? 0} · skills ${launch.skills ?? 0} · 插件 ${launch.plugins ?? 0} · 首轮 ${first}`;
  const builtin = launch.builtin_plugins?.length ? `不可关闭的内置插件：${launch.builtin_plugins.join("、")}` : "";
  const version = launch.version ? `Claude Code ${launch.version}` : "";
  if (launch.minimal === false || missing.length)
    return { text: `部分最小化 · 未能关闭：${missing.join(" ")} · ${counts}`, title: [version, builtin].filter(Boolean).join("\n") || "当前 Claude Code 版本不支持这些参数", tone: "warn" };
  return { text: `最小启动 · ${counts}`, title: [builtin, version, launch.first_context == null ? "没有首轮用量（被中断的执行不计入对比）" : ""].filter(Boolean).join("\n"), tone: "ok" };
}

/**
 * The session of a stage's latest passed run, looking through follow-ups to the run that
 * opened it (a follow-up continues whatever that run reused). Null until the stage passed.
 */
export function stageSession<S extends { kind: string }>(stage: {
  current_attempt: string | null;
  attempts: { id: string; status: string; follow_up: boolean; session: S | null }[];
}): S | null {
  const at = stage.attempts.findIndex((a) => a.id === stage.current_attempt);
  if (at < 0 || stage.attempts[at].status !== "succeeded") return null;
  for (let i = at; i >= 0; i--) {
    const a = stage.attempts[i];
    if (!a.follow_up || (a.session && a.session.kind !== "continued")) return a.session;
  }
  return null;
}
