import { useCallback, useEffect, useState } from "react";
import { RefreshCw } from "lucide-react";
import type { CompareRow, CompareSide, ProjectSummary, ProjectUsage, UsageRow } from "../api/types";
import { api, errorMessage } from "../api/client";
import { useStudio } from "../state/store";
import { launchLabel, relative, tokens, tokensK, usd } from "../lib/format";
import { BudgetField, TokenBar, TokenLegend, tokenTitle, UsageBreakdown } from "./Budget";
import { Button, Segmented, Spinner } from "./ui";

const WINDOW: Record<string, string> = { five_hour: "5 小时", seven_day: "7 天", primary: "当前窗口" };

function Rows({ rows, label }: { rows: UsageRow[]; label: string }) {
  const max = rows[0]?.total ?? 0;
  return (
    <table className="usage-table" aria-label={label}>
      <tbody>
        {rows.map((r) => (
          <tr key={r.key}>
            <td className="ut-key">{r.key || "—"}</td>
            <td className="ut-bar">
              <TokenBar t={r.tokens} scale={max} />
            </td>
            <td className="ut-num" title={tokenTitle(r.tokens)}>
              {tokens(r.total)}
            </td>
            <td className="ut-sub muted">
              {r.runs} 次{r.cost_usd ? ` · ${usd(r.cost_usd)}` : ""}
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

const MAYBE_HIGH = "缺少会话日志，无法按本轮窗口重算，保留原始数值";

type RunRow = ProjectUsage["runs"][number];

/** Tags after a run's meta line: launch (Claude only), reused session, count may be high. */
function RunTags({ r }: { r: RunRow }) {
  const launch = r.client === "Claude" ? (r.diet ? { text: "最小启动", title: launchLabel(r.launch, r.client)?.text ?? "" } : { text: "优化前", title: "这次执行在最小启动之前" }) : null;
  return (
    <>
      {launch && (
        <span className={`tag${r.diet ? " tag-ok" : ""}`} title={launch.title}>
          {launch.text}
        </span>
      )}
      {r.reused_from && <span className="tag tag-auto">↻ 复用自 {r.reused_from}</span>}
    </>
  );
}

const pair = (b: number | null, a: number | null, f: (n: number) => string) => `${b === null ? "—" : f(b)} → ${a === null ? "—" : f(a)}`;
const calls = (n: number) => String(Math.round(n));

function CompareCell({ row, value, f }: { row: CompareRow; value(s: CompareSide): number | null; f(n: number): string }) {
  const [b, a] = [value(row.before), value(row.after)];
  const excluded = (b === null && row.before.runs > 0) || (a === null && row.after.runs > 0);
  return (
    <td className="cmp-num" title={`优化前 ${row.before.runs} 次 · 优化后 ${row.after.runs} 次${excluded ? "\n被中断的执行不计入" : ""}`}>
      {pair(b, a, f)}
    </td>
  );
}

function Change({ row }: { row: CompareRow }) {
  const few = row.after.runs > 0 && row.after.runs < 3 ? <span className="muted cmp-few"> 样本少</span> : null;
  if (row.after.runs === 0) return <td className="cmp-change muted">暂无优化后</td>;
  if (row.change === null) return <td className="cmp-change muted">—{few}</td>;
  const pct = Math.round(row.change * 100);
  if (Math.abs(row.change) < 0.05)
    return (
      <td className="cmp-change muted">
        持平{few}
      </td>
    );
  return (
    <td className={`cmp-change ${row.change < 0 ? "down" : "up"}`}>
      {pct > 0 ? `+${pct}` : `−${Math.abs(pct)}`}%{few}
    </td>
  );
}

/** Before / after the minimal launch (interaction §3.1). Hidden without runs before it. */
function CompareCard({ data, project }: { data: ProjectUsage; project: string }) {
  const [by, setBy] = useState<"stage" | "model">("stage");
  const [open, setOpen] = useState<string | null>(null);
  const rows = by === "stage" ? data.compare.by_stage : data.compare.by_model;
  const all = [...data.compare.by_stage];
  if (data.total.runs === 0 || !all.some((r) => r.before.runs > 0)) return null;
  const onlyBefore = !all.some((r) => r.after.runs > 0);
  const recent = (key: string) =>
    data.runs
      .filter((r) => (by === "stage" ? r.stage === key : `${r.client} · ${r.model || "默认模型"}` === key))
      .sort((x, y) => y.started_at.localeCompare(x.started_at))
      .slice(0, 5);
  return (
    <section className="card compare-card" data-testid="usage-compare">
      <div className="card-head">
        <span className="card-title">优化前后对比</span>
        <Segmented
          label="对比维度"
          size="sm"
          value={by}
          options={[
            { value: "stage", label: "按阶段" },
            { value: "model", label: "按 CLI · 模型" },
          ]}
          onChange={(v) => {
            setBy(v);
            setOpen(null);
          }}
        />
      </div>
      <p className="field-hint">{onlyBefore ? "升级后的第一次执行完成后开始对比。" : "以执行记录里是否带「最小启动」标记分界，旧记录都算优化前。"}</p>
      <table className="usage-table compare-table" aria-label="优化前后对比">
        <thead>
          <tr>
            <th>{by === "stage" ? "阶段" : "CLI · 模型"}</th>
            <th>首轮上下文（中位）</th>
            <th>平均调用次数</th>
            <th>每次执行平均总量</th>
            <th>变化</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((r) => (
            <CompareRows key={r.key} row={r} open={open === r.key} onToggle={() => setOpen(open === r.key ? null : r.key)} runs={open === r.key ? recent(r.key) : []} project={project} />
          ))}
        </tbody>
      </table>
    </section>
  );
}

function CompareRows({ row, open, onToggle, runs }: { row: CompareRow; open: boolean; onToggle(): void; runs: RunRow[]; project: string }) {
  const { selectWork } = useStudio();
  return (
    <>
      <tr className={`cmp-row${open ? " on" : ""}`} onClick={onToggle} aria-expanded={open}>
        <td className="ut-key">{row.key || "—"}</td>
        <CompareCell row={row} value={(s) => s.first_context_median} f={tokensK} />
        <CompareCell row={row} value={(s) => s.avg_calls} f={calls} />
        <CompareCell row={row} value={(s) => s.avg_total} f={tokensK} />
        <Change row={row} />
      </tr>
      {open && (
        <tr className="cmp-detail">
          <td colSpan={5}>
            {runs.length === 0 ? (
              <span className="field-hint">消耗最多的执行里没有这一行的记录。</span>
            ) : (
              <ol className="usage-runs">
                {runs.map((r) => (
                  <li key={r.attempt}>
                    <button type="button" className="usage-run" onClick={() => selectWork(r.work)}>
                      <span className="ur-name">
                        {r.work_name} · {r.stage}
                      </span>
                      <span className="muted">
                        {r.client} · {r.model || "默认模型"} · {relative(r.started_at)} · {r.calls} 次调用 <RunTags r={r} />
                      </span>
                      <span className="ur-num">{tokens(r.total)}</span>
                    </button>
                  </li>
                ))}
              </ol>
            )}
          </td>
        </tr>
      )}
    </>
  );
}

export function UsagePage({ project }: { project: ProjectSummary }) {
  const { act, revision, selectWork } = useStudio();
  const [data, setData] = useState<ProjectUsage | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setData(await api.projectUsage({ project: project.id }));
      setError(null);
    } catch (e) {
      setError(errorMessage(e));
    }
  }, [project.id]);
  useEffect(() => {
    setData(null);
    void load();
  }, [load]);
  useEffect(() => {
    void load();
  }, [revision]); // eslint-disable-line react-hooks/exhaustive-deps

  const t = data?.total;
  const cachePct = t && t.total ? Math.round((t.tokens.cache_read / t.total) * 100) : 0;
  const peakDay = Math.max(1, ...(data?.days.map((d) => d.total) ?? [1]));
  const maybeHigh = data?.maybe_high_runs ? `含 ${data.maybe_high_runs} 次计数可能偏高的 Codex 执行` : undefined;

  return (
    <div className="page usage-page">
      <header className="branches-head">
        <div>
          <h1>Token 消耗</h1>
          <p>按 Claude / Codex / ZCode 自己报告的用量统计（含缓存读写），可以看出钱花在了哪个阶段、哪个模型、哪一步。</p>
        </div>
        <Button variant="secondary" size="sm" icon={<RefreshCw size={13} />} onClick={() => void load()}>
          刷新
        </Button>
      </header>
      {error && <p className="inline-error">{error}</p>}
      {!data && !error && (
        <p className="field-hint scan-hint">
          <Spinner size={12} /> 正在汇总各次执行的用量…
        </p>
      )}
      {data && t && (
        <>
          <section className="usage-cards">
            <div className="usage-card">
              <span className="muted">总计</span>
              <strong title={tokenTitle(t.tokens)}>{tokens(t.total)}</strong>
              <span className="muted">{t.runs} 次执行</span>
            </div>
            <div className="usage-card">
              <span className="muted">缓存读取占比</span>
              <strong>{cachePct}%</strong>
              <span className="muted">新输入 {tokens(t.tokens.input)} · 输出 {tokens(t.tokens.output)}</span>
            </div>
            <div className="usage-card">
              <span className="muted">Claude 报告的费用</span>
              <strong>{t.cost_usd ? usd(t.cost_usd) : "—"}</strong>
              <span className="muted">按 API 标价；订阅账号实际按额度计</span>
            </div>
            {data.quota.map((q) => (
              <div className="usage-card" key={q.client}>
                <span className="muted">{q.client} 账号额度</span>
                <strong>
                  {Object.entries(q.windows)
                    .map(([w, v]) => `${WINDOW[w] ?? w} ${Math.round(v * 100)}%`)
                    .join(" · ")}
                </strong>
                <span className="muted">{relative(q.at)}的执行报告</span>
              </div>
            ))}
          </section>

          <CompareCard data={data} project={project.id} />

          <section className="usage-budget card">
            <span className="picker-label">项目默认阶段预算</span>
            <BudgetField
              value={data.stage_budget}
              placeholder="不限"
              label="项目默认阶段 Token 预算"
              onSave={(n) => act(() => api.setProjectBudget({ project: project.id, tokens: n }), n ? `默认阶段预算 ${tokens(n)}` : "已取消默认阶段预算")}
            />
            <span className="picker-label">续接上限</span>
            <BudgetField
              value={data.resume_limit}
              placeholder="默认 150K"
              label="续接上限"
              min={20_000}
              minHint="至少 20K，否则几乎每次都会开新会话"
              onSave={(n) => act(() => api.setResumeLimit({ project: project.id, tokens: n ? String(n) : "" }), n ? `续接上限 ${tokensK(n)}` : "续接上限恢复默认 150K")}
            />
            <span className="field-hint">
              没有单独设置预算的阶段用这个值；阶段的所有执行（含继续对话、子任务）累计计算，用完时停下等你追加。
              <br />
              续接时上次调用的上下文超过这个值，就改开新会话并带上交接摘要，避免越聊越贵。留空用默认 150K。
            </span>
          </section>

          <div className="usage-grid">
            <section className="card">
              <div className="card-head">
                <span className="card-title" title={maybeHigh}>
                  按阶段
                </span>
                <TokenLegend />
              </div>
              <Rows rows={data.stages} label="按阶段" />
            </section>
            <section className="card">
              <div className="card-head">
                <span className="card-title">按 CLI · 模型</span>
              </div>
              <Rows rows={data.models} label="按模型" />
            </section>
          </div>

          <section className="card">
            <div className="card-head">
              <span className="card-title" title={maybeHigh}>
                按事项
              </span>
            </div>
            <table className="usage-table" aria-label="按事项">
              <tbody>
                {data.items.map((r) => (
                  <tr key={r.key}>
                    <td className="ut-key">
                      <button type="button" className="link-btn inline" onClick={() => selectWork(r.key)}>
                        {r.name}
                      </button>
                      {r.shelved && <span className="tag">已归档</span>}
                    </td>
                    <td className="ut-bar">
                      <TokenBar t={r.tokens} scale={data.items[0].total} />
                    </td>
                    <td className="ut-num" title={tokenTitle(r.tokens)}>
                      {tokens(r.total)}
                      {r.budget ? <span className="muted"> / {tokens(r.budget)}</span> : null}
                    </td>
                    <td className="ut-sub muted">
                      {r.runs} 次{r.cost_usd ? ` · ${usd(r.cost_usd)}` : ""}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </section>

          {data.days.length > 1 && (
            <section className="card">
              <div className="card-head">
                <span className="card-title">按天</span>
              </div>
              <div className="usage-days">
                {data.days.map((d) => (
                  <span key={d.key} title={`${d.key}：${tokens(d.total)} · ${d.runs} 次`}>
                    <i style={{ height: `${Math.max(3, (d.total / peakDay) * 100)}%` }} />
                    <em>{d.key.slice(5)}</em>
                  </span>
                ))}
              </div>
            </section>
          )}

          <section className="card">
            <div className="card-head">
              <span className="card-title">消耗最多的执行</span>
              <span className="field-hint">展开看这一轮的 Token 花在哪些步骤上</span>
            </div>
            <ol className="usage-runs">
              {data.runs.map((r) => (
                <li key={r.attempt}>
                  <button type="button" className={`usage-run${open === r.attempt ? " on" : ""}`} onClick={() => setOpen(open === r.attempt ? null : r.attempt)} aria-expanded={open === r.attempt}>
                    <span className="ur-name">
                      {r.work_name} · {r.stage}
                    </span>
                    <span className="muted">
                      {r.client} · {r.model} · {relative(r.started_at)} · {r.calls} 次调用 <RunTags r={r} />
                    </span>
                    <span className="ur-num">
                      {tokens(r.total)}
                      {r.count_maybe_high && (
                        <span className="tag tag-warn" title={MAYBE_HIGH}>
                          计数可能偏高
                        </span>
                      )}
                      {r.cost_usd ? <span className="muted"> · {usd(r.cost_usd)}</span> : null}
                    </span>
                  </button>
                  {open === r.attempt && <UsageBreakdown project={project.id} work={r.work} attempt={r.attempt} />}
                </li>
              ))}
            </ol>
          </section>
        </>
      )}
    </div>
  );
}
