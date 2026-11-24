import { useState } from "react";
import { Check, Info, Zap } from "lucide-react";
import type { WorkDetail } from "../api/types";
import { api } from "../api/client";
import { useStudio } from "../state/store";
import { StagePickers } from "./StageConfig";
import { Button, Drawer } from "./ui";
import { BudgetField, BudgetMeter, StageBudget } from "./Budget";
import { tokens } from "../lib/format";

export function AutopilotDrawer({ detail, onClose }: { detail: WorkDetail; onClose(): void }) {
  const { act } = useStudio();
  const remaining = detail.stages.slice(detail.stage);
  const lastRunnable = [...remaining].reverse().find((s) => s.state !== "skipped") ?? remaining[remaining.length - 1];
  const initial = detail.autopilot.until !== null && detail.autopilot.until >= detail.stage ? detail.autopilot.until : lastRunnable?.index ?? detail.stage;
  const [until, setUntil] = useState<number>(initial);
  const [busy, setBusy] = useState(false);
  const target = detail.stages[until];
  const ap = detail.autopilot;

  const start = async () => {
    setBusy(true);
    const ok = await act(() => api.autopilotStart({ project: detail.project, work: detail.id, until }), "自动运行已开始");
    setBusy(false);
    if (ok !== undefined) onClose();
  };

  // `skip` toggles on a later stage; on the current stage it skips and advances.
  const toggleSkip = (index: number, skipped: boolean) =>
    act(() => api.skip({ project: detail.project, work: detail.id, stage: index, reason: skipped ? "" : "自动运行时跳过" }));

  return (
    <Drawer
      title="自动运行"
      subtitle="从当前阶段开始依次运行，到你选定的阶段停下等你确认。"
      onClose={onClose}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            取消
          </Button>
          <Button variant="primary" icon={<Zap size={14} />} busy={busy} disabled={!target || ap.running} onClick={start}>
            自动运行到「{target?.name}」
          </Button>
        </>
      }
    >
      <p className="ap-explain">
        <Info size={14} />
        <span>测试或验收发现问题时，会自动退回负责的前序阶段修复（最多循环 2 次）；Agent 报告 BLOCKED 或 Token 预算用完时停下等人处理，不会自动重试。</span>
      </p>
      <div className="ap-budget">
        <span className="picker-label">事项总预算</span>
        <BudgetField
          value={detail.budget.total_limit}
          placeholder="不限"
          label="事项 Token 总预算"
          onSave={(n) => act(() => api.setBudget({ project: detail.project, work: detail.id, tokens: n }))}
        />
        <BudgetMeter used={detail.budget.total_used} limit={detail.budget.total_limit} />
        <span className="field-hint">
          所有阶段合计；用完时自动运行停下等你。单个阶段的预算在下面设置{detail.budget.stage_default ? `，未设置的阶段用项目默认 ${tokens(detail.budget.stage_default)}` : ""}。
        </span>
      </div>
      {ap.message && !ap.running && (
        <p className="ap-last">
          <span className="muted">上次结果</span>
          <span>{ap.message}</span>
        </p>
      )}
      <ol className="ap-list">
        {remaining.map((s) => {
          const after = s.index > until;
          const skipped = s.state === "skipped";
          const chosen = s.index === until;
          return (
            <li key={s.index} className={`ap-row${after ? " dim" : ""}${chosen ? " chosen" : ""}${skipped ? " skipped" : ""}`}>
              <div className="ap-row-head">
                <button
                  type="button"
                  className={`ap-until${chosen ? " on" : ""}`}
                  onClick={() => setUntil(s.index)}
                  aria-pressed={chosen}
                  disabled={skipped}
                  title="运行到这里"
                >
                  {chosen ? <Check size={13} strokeWidth={3} /> : <span className="ap-num">{s.index + 1}</span>}
                </button>
                <span className="ap-name">{s.name}</span>
                {s.index === detail.stage && <span className="tag">当前</span>}
                {chosen && <span className="ap-stop">运行到这里</span>}
                <span className="ap-spacer" />
                {s.skippable && (
                  <button
                    type="button"
                    className={`ap-skip${skipped ? " on" : ""}`}
                    onClick={() => toggleSkip(s.index, skipped)}
                    aria-pressed={skipped}
                    disabled={!!detail.running && s.index === detail.stage}
                    title={skipped ? "再次点击取消跳过" : s.index === detail.stage ? "跳过当前阶段并进入下一阶段" : "自动运行时跳过此阶段"}
                  >
                    {skipped ? "已跳过" : "跳过"}
                  </button>
                )}
              </div>
              {!skipped && !after && (
                <div className="ap-row-body">
                  <StagePickers detail={detail} stage={s} compact />
                  <StageBudget detail={detail} stage={s} compact />
                </div>
              )}
            </li>
          );
        })}
      </ol>
    </Drawer>
  );
}
