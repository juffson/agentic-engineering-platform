import { useCallback, useEffect, useState } from "react";
import { RefreshCw } from "lucide-react";
import type { ProjectSummary, ProjectUsage, UsageRow } from "../api/types";
import { api, errorMessage } from "../api/client";
import { useStudio } from "../state/store";
import { relative, tokens, usd } from "../lib/format";
import { BudgetField, TokenBar, TokenLegend, tokenTitle, UsageBreakdown } from "./Budget";
import { Button, Spinner } from "./ui";

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

          <section className="usage-budget card">
            <span className="picker-label">项目默认阶段预算</span>
            <BudgetField
              value={data.stage_budget}
              placeholder="不限"
              label="项目默认阶段 Token 预算"
              onSave={(n) => act(() => api.setProjectBudget({ project: project.id, tokens: n }), n ? `默认阶段预算 ${tokens(n)}` : "已取消默认阶段预算")}
            />
            <span className="field-hint">没有单独设置预算的阶段用这个值；阶段的所有执行（含继续对话、子任务）累计计算，用完时停下等你追加。</span>
          </section>

          <div className="usage-grid">
            <section className="card">
              <div className="card-head">
                <span className="card-title">按阶段</span>
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
              <span className="card-title">按事项</span>
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
                      {r.client} · {r.model} · {relative(r.started_at)} · {r.calls} 次调用
                    </span>
                    <span className="ur-num">
                      {tokens(r.total)}
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
