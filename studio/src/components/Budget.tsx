import { useEffect, useState } from "react";
import { PlayCircle } from "lucide-react";
import type { AttemptSummary, StageDetail, Tokens, UsageAnalysis, WorkDetail } from "../api/types";
import { api, errorMessage } from "../api/client";
import { useStudio } from "../state/store";
import { carriesSummary, launchLabel, parseTokens, tokens, tokensK, usd } from "../lib/format";
import { Button, Spinner } from "./ui";

/** A token amount typed as "2M", "500k" or a plain number; empty clears it. */
export function BudgetField({
  value,
  placeholder,
  label,
  onSave,
  disabled,
  min,
  minHint,
}: {
  value: number | null;
  placeholder: string;
  label: string;
  onSave(tokens: number | null): void;
  disabled?: boolean;
  /** Smallest accepted value; below it the field turns red and says `minHint`. */
  min?: number;
  minHint?: string;
}) {
  const shown = value ? tokens(value) : "";
  const [text, setText] = useState(shown);
  useEffect(() => setText(shown), [shown]);
  const parsed = parseTokens(text);
  const tooSmall = parsed !== null && !Number.isNaN(parsed) && min !== undefined && parsed < min;
  const invalid = (parsed !== null && Number.isNaN(parsed)) || tooSmall;
  const commit = () => {
    if (invalid || text === shown) return;
    if ((parsed ?? null) === (value ?? null)) return;
    onSave(parsed);
  };
  return (
    <input
      className={`input input-sm budget-input${invalid ? " invalid" : ""}`}
      value={text}
      placeholder={placeholder}
      aria-label={label}
      title={tooSmall ? minHint : "例如 2M、500k；留空表示不限"}
      aria-invalid={invalid || undefined}
      disabled={disabled}
      onChange={(e) => setText(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => e.key === "Enter" && commit()}
    />
  );
}

/** Used against a limit; amber from 80%, red once used up. */
export function BudgetMeter({ used, limit }: { used: number; limit: number | null }) {
  if (!limit) return used > 0 ? <span className="budget-used muted">已用 {tokens(used)}</span> : null;
  const ratio = used / limit;
  const tone = ratio >= 1 ? "over" : ratio >= 0.8 ? "warn" : "";
  return (
    <span className={`budget-meter ${tone}`} title={`${used.toLocaleString()} / ${limit.toLocaleString()} tokens`}>
      <span className="budget-bar">
        <span style={{ width: `${Math.min(100, ratio * 100)}%` }} />
      </span>
      <span>
        {tokens(used)} / {tokens(limit)}
      </span>
    </span>
  );
}

/** One stage's budget: the input plus how much it has used. */
export function StageBudget({ detail, stage, compact }: { detail: WorkDetail; stage: StageDetail; compact?: boolean }) {
  const { act } = useStudio();
  const fallback = detail.budget.stage_default;
  const save = (n: number | null, done?: string) => act(() => api.setBudget({ project: detail.project, work: detail.id, stage: stage.index, tokens: n }), done);
  // A suggestion from runs after the minimal launch (R9); only while no own budget is set.
  const suggested = stage.budget.own ? null : stage.budget.suggested;
  return (
    <div className={`picker stage-budget${compact ? " compact" : ""}`}>
      {!compact && <span className="picker-label">Token 预算</span>}
      <div className="budget-row">
        <BudgetField
          value={stage.budget.own}
          placeholder={suggested ? `建议 ${tokensK(suggested.value)}` : fallback ? `默认 ${tokens(fallback)}` : "不限，如 5M"}
          label={`「${stage.name}」Token 预算`}
          onSave={(n) => save(n)}
        />
        {!compact &&
          [2_000_000, 5_000_000, 10_000_000].map((n) => (
            <button key={n} type="button" className={`chip budget-chip${stage.budget.own === n ? " on" : ""}`} onClick={() => save(n)}>
              {tokens(n)}
            </button>
          ))}
        {!compact && stage.budget.own !== null && (
          <button type="button" className="link-btn inline" onClick={() => save(null)}>
            不限
          </button>
        )}
        <BudgetMeter used={stage.budget.used} limit={stage.budget.limit} />
      </div>
      {!compact && (
        <p className="field-hint">
          填这个阶段最多能用多少 Token，如 <code>5M</code>、<code>500k</code>；留空不限{fallback ? `（用项目默认 ${tokens(fallback)}）` : ""}。阶段的所有执行（含继续对话、子任务）合计，到 80% 提醒，用完就停下，等你追加后在原会话继续。参考：一次阶段执行通常用 2–6M，大部分是缓存读取。
        </p>
      )}
      {suggested && !compact && (
        <span className="field-hint budget-suggest">
          优化后 {suggested.runs} 次执行中位数 {tokensK(suggested.median)} × 1.5
          <button type="button" className="link-btn inline" onClick={() => save(suggested.value, `已设为建议值 ${tokensK(suggested.value)}`)}>
            采用
          </button>
        </span>
      )}
    </div>
  );
}

export function tokenTitle(t: Tokens): string {
  return `新输入 ${t.input.toLocaleString()} · 缓存读取 ${t.cache_read.toLocaleString()} · 缓存写入 ${t.cache_write.toLocaleString()} · 输出 ${t.output.toLocaleString()}${t.reasoning ? `（其中推理 ${t.reasoning.toLocaleString()}）` : ""}`;
}

const total = (t: Tokens) => t.input + t.cache_read + t.cache_write + t.output;

/** Stacked bar of where a token total came from. */
export function TokenBar({ t, scale }: { t: Tokens; scale?: number }) {
  const sum = scale ?? total(t);
  if (!sum) return null;
  const part = (n: number) => `${(n / sum) * 100}%`;
  return (
    <span className="token-bar" title={tokenTitle(t)}>
      <span className="tb-cache" style={{ width: part(t.cache_read) }} />
      <span className="tb-write" style={{ width: part(t.cache_write) }} />
      <span className="tb-input" style={{ width: part(t.input) }} />
      <span className="tb-output" style={{ width: part(t.output) }} />
    </span>
  );
}

export function TokenLegend() {
  return (
    <span className="token-legend">
      <span>
        <i className="tb-cache" />
        缓存读取
      </span>
      <span>
        <i className="tb-write" />
        缓存写入
      </span>
      <span>
        <i className="tb-input" />
        新输入
      </span>
      <span>
        <i className="tb-output" />
        输出
      </span>
    </span>
  );
}

/** Where one run's tokens went: by kind of step, context size per call, and the steps that
 *  grew the context most. */
export function UsageBreakdown({
  project,
  work,
  attempt,
  client,
  session,
}: {
  project: string;
  work: string;
  attempt: string;
  client?: string;
  session?: AttemptSummary["session"];
}) {
  const [data, setData] = useState<UsageAnalysis | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    api.usageAnalysis({ project, work, attempt }).then(setData, (e) => setError(errorMessage(e)));
  }, [project, work, attempt]);
  if (error) return <p className="inline-error">{error}</p>;
  if (!data)
    return (
      <p className="field-hint">
        <Spinner size={11} /> 正在分析…
      </p>
    );
  const s = data.summary;
  const sum = total(s.tokens);
  if (!sum) return <p className="field-hint">这次执行没有可读取的用量记录。</p>;
  const peak = Math.max(1, ...data.calls.map((c) => c.context));
  const launch = data.reused ? null : launchLabel(data.launch, client ?? (data.launch?.mcp_servers !== undefined ? "Claude" : ""));
  const carried = carriesSummary(session);
  return (
    <div className="usage-breakdown" data-testid="usage-breakdown">
      <div className="ub-head">
        <strong>{tokens(sum)}</strong>
        <span className="muted">
          tokens · {s.calls} 次模型调用{s.cost_usd ? ` · ${usd(s.cost_usd)}` : ""}
        </span>
        <TokenLegend />
      </div>
      {data.reused ? (
        <p className="ub-launch" data-testid="usage-launch">
          <span className="tag tag-auto">复用会话</span>
          <span>首轮已缓存 {data.launch?.first_context != null ? tokensK(data.launch.first_context) : "—"}</span>
          <span className="muted">本轮只计本阶段窗口内的 {data.window_calls} 次调用</span>
        </p>
      ) : (
        launch && (
          <p className={`ub-launch ${launch.tone}`} data-testid="usage-launch" title={launch.title}>
            <span className="picker-label">启动配置</span>
            <span>{launch.text}</span>
          </p>
        )
      )}
      <TokenBar t={s.tokens} />
      <p className="field-hint">
        缓存读取 {Math.round((s.tokens.cache_read / sum) * 100)}%：每次调用都会把整段上下文再读一遍，上下文越长、调用越多，消耗越大。
      </p>
      {data.by_kind.length > 0 && (
        <table className="ub-table">
          <thead>
            <tr>
              <th>步骤</th>
              <th>调用</th>
              <th>Token</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {data.by_kind.map((k) => (
              <tr key={k.kind}>
                <td>{k.kind}</td>
                <td>{k.calls}</td>
                <td>{tokens(k.total)}</td>
                <td className="ub-bar-cell">
                  <TokenBar t={k.tokens} scale={data.by_kind[0].total} />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {data.calls.length > 1 && (
        <div className="ub-context" aria-label="每次调用的上下文大小">
          <span className="field-hint">每次调用的上下文（最高 {tokens(peak)}）</span>
          <div className="ub-spark">
            {data.calls.map((c, i) => (
              <span key={i} className={`k-${c.kind}`} style={{ height: `${Math.max(4, (c.context / peak) * 100)}%` }} title={`#${i + 1} ${c.kind} ${c.detail}\n上下文 ${c.context.toLocaleString()} · 输出 ${c.output.toLocaleString()}${i === 0 && carried ? "\n新会话 + 交接摘要" : ""}`} />
            ))}
          </div>
        </div>
      )}
      {data.heaviest.length > 0 && (
        <div className="ub-heavy">
          <span className="field-hint">让上下文涨得最多的步骤（之后每次调用都要再读一遍）</span>
          <ol>
            {data.heaviest.map((h) => (
              <li key={h.call}>
                <span className="tag">{h.kind}</span>
                <code title={h.detail}>{h.detail || `第 ${h.call + 1} 次调用`}</code>
                <span className="muted">
                  +{tokens(h.tokens)} · 累计重复读取约 {tokens(h.carried)}
                </span>
              </li>
            ))}
          </ol>
        </div>
      )}
    </div>
  );
}

/** A run stopped by its budget: add budget and continue in the same session, or re-run the
 *  stage when its CLI cannot continue the original task (ZCode desktop, R7). */
export function ExtendBudget({ detail, stage, attempt }: { detail: WorkDetail; stage: StageDetail; attempt?: AttemptSummary }) {
  const continuable = attempt?.continuable ?? true;
  const { act } = useStudio();
  const step = stage.budget.limit ?? detail.budget.total_limit ?? 1_000_000;
  const [extra, setExtra] = useState(tokens(Math.max(100_000, Math.round(step / 2))));
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const n = parseTokens(extra);
  const go = async () => {
    if (!n || Number.isNaN(n)) return;
    setBusy(true);
    await act(() => api.extendBudget({ project: detail.project, work: detail.id, stage: stage.index, extra: n, message: continuable ? message : "" }), continuable ? "已追加预算，继续执行" : "已追加预算，重新运行");
    setBusy(false);
  };
  return (
    <div className="extend-budget">
      {!continuable && (
        <p className="field-hint extend-note">
          {attempt?.client === "ZCode" ? "ZCode 不支持在原任务上继续" : "这个 CLI 不支持在原会话上继续"}，会按原说明重新运行这一阶段
        </p>
      )}
      <label className="budget-row">
        <span className="picker-label">追加</span>
        <input className="input input-sm budget-input" value={extra} onChange={(e) => setExtra(e.target.value)} aria-label="追加 Token" />
        <BudgetMeter used={stage.budget.used} limit={stage.budget.limit} />
      </label>
      {continuable && (
        <textarea className="input" rows={2} value={message} onChange={(e) => setMessage(e.target.value)} placeholder="继续时要说的话（可留空：从中断处继续）" aria-label="继续说明" />
      )}
      <Button variant="primary" size="lg" className="block" icon={<PlayCircle size={14} />} busy={busy} disabled={!n || Number.isNaN(n)} onClick={go}>
        {continuable ? "追加预算并继续" : "追加预算并重跑"}
      </Button>
    </div>
  );
}
