import { useEffect, useState } from "react";
import { ArrowRight, Check, CornerUpLeft, PackageCheck, Play, RotateCcw, ShieldCheck, SkipForward, Square, Undo2 } from "lucide-react";
import type { StageDetail, WorkDetail } from "../api/types";
import { api } from "../api/client";
import { useStudio } from "../state/store";
import { relative } from "../lib/format";
import { isGate, latestAttempt, needsRecheck, sendBackTarget } from "../lib/work";
import { StageConfigCard } from "./StageConfig";
import { ArchiveCard, DeliveryCard, ShelvedCard } from "./Delivery";
import { Button, Spinner } from "./ui";

export function DecisionPanel({ detail, viewed }: { detail: WorkDetail; viewed: StageDetail | undefined }) {
  if (detail.shelved) {
    return (
      <aside className="decision" aria-label="决策">
        <ShelvedCard detail={detail} />
      </aside>
    );
  }
  if (detail.delivered) {
    return (
      <aside className="decision" aria-label="交付">
        <DeliveryCard detail={detail} />
        <ArchiveCard detail={detail} />
      </aside>
    );
  }
  const current = detail.stages[detail.stage];
  return (
    <aside className="decision" aria-label="决策">
      {current && <Decision detail={detail} current={current} />}
      {viewed && viewed.state !== "skipped" && (
        <StageConfigCard detail={detail} stage={viewed} collapsed={viewed.state === "done" || latestAttempt(viewed)?.status === "succeeded"} />
      )}
      {viewed && <More detail={detail} stage={viewed} />}
    </aside>
  );
}

function Decision({ detail, current }: { detail: WorkDetail; current: StageDetail }) {
  const { act } = useStudio();
  const [busy, setBusy] = useState<string | null>(null);
  const [waiving, setWaiving] = useState(false);
  const [reason, setReason] = useState("");
  useEffect(() => {
    setWaiving(false);
    setReason("");
  }, [detail.id, detail.stage]);

  const base = { project: detail.project, work: detail.id };
  const latest = latestAttempt(current);
  const next = detail.stages.slice(current.index + 1).find((s) => s.state !== "skipped");
  const run = async (key: string, fn: () => Promise<unknown>) => {
    setBusy(key);
    await act(fn);
    setBusy(null);
  };

  let body;
  if (detail.running) {
    const runningId = detail.running;
    body = (
      <>
        <div className="decision-state">
          <Spinner size={14} />
          <div>
            <p className="decision-title">Agent 正在执行</p>
            <p className="decision-text">「{current.name}」运行中，完成后在这里确认结果。</p>
          </div>
        </div>
        <Button variant="secondary" size="lg" className="block" icon={<Square size={12} />} onClick={() => run("cancel", () => api.cancel({ ...base, attempt: runningId }))} busy={busy === "cancel"}>
          停止
        </Button>
      </>
    );
  } else if (needsRecheck(current)) {
    const rework = [...detail.reworks].reverse().find((r) => r.to < current.index);
    body = (
      <>
        <div className="decision-state">
          <RotateCcw size={15} className="muted" />
          <div>
            <p className="decision-title">前序阶段已修改，需要复核</p>
            <p className="decision-text">
              {rework
                ? `「${detail.stages[rework.from]?.name}」退回「${detail.stages[rework.to]?.name}」修复后回到这里。`
                : "前序阶段重新交接后回到这里。"}
              将在「{current.name}」原会话里继续，带上退回原因和修改后的交接，复核上次指出的问题。
            </p>
          </div>
        </div>
        <Button variant="primary" size="lg" className="block" icon={<RotateCcw size={14} />} busy={busy === "recheck"} onClick={() => run("recheck", () => api.runStage({ ...base, stage: current.index }))}>
          复核「{current.name}」
        </Button>
      </>
    );
  } else if (!latest) {
    body = (
      <div className="decision-state">
        <Play size={15} className="muted" />
        <div>
          <p className="decision-title">等待运行</p>
          <p className="decision-text">在左侧运行「{current.name}」，或使用「自动运行」连续推进后续阶段。</p>
        </div>
      </div>
    );
  } else if (latest.status === "failed" || latest.route_back) {
    // Gates go back to the build; any stage that named an owner with AEP-BACK goes there.
    const target = isGate(current) || latest.route_back ? sendBackTarget(detail, current.index) : null;
    const targetStage = target !== null ? detail.stages[target] : undefined;
    body = (
      <>
        <div className="decision-state fail">
          <span className="state-dot" />
          <div>
            <p className="decision-title">「{current.name}」未通过</p>
            {latest.error && <p className="decision-text">{latest.error}</p>}
          </div>
        </div>
        {latest.route_back && (
          <p className="routeback-note">
            <CornerUpLeft size={13} />
            <span>{latest.route_back.question}</span>
          </p>
        )}
        {targetStage ? (
          <>
            <Button
              variant="primary"
              size="lg"
              className="block"
              icon={<CornerUpLeft size={14} />}
              busy={busy === "back"}
              onClick={() =>
                run("back", () =>
                  api.sendBack({ ...base, from: current.index, to: targetStage.index, reason: latest.route_back?.question ?? latest.error ?? undefined }),
                )
              }
            >
              退回「{targetStage.name}」修复
            </Button>
            <p className="decision-foot">退回后 Agent 会立即在「{targetStage.name}」开始修复，修好后再回到「{current.name}」。</p>
          </>
        ) : (
          <Button variant="primary" size="lg" className="block" icon={<RotateCcw size={14} />} busy={busy === "rerun"} onClick={() => run("rerun", () => api.runStage({ ...base, stage: current.index }))}>
            重新运行「{current.name}」
          </Button>
        )}
        {waiving ? (
          <div className="waive">
            <textarea className="input" rows={2} value={reason} onChange={(e) => setReason(e.target.value)} placeholder="放行理由（必填），例如：已知问题，下个版本修复" aria-label="放行理由" autoFocus />
            <div className="row-end">
              <Button variant="ghost" size="sm" onClick={() => setWaiving(false)}>
                取消
              </Button>
              <Button variant="secondary" size="sm" icon={<ShieldCheck size={13} />} disabled={!reason.trim()} busy={busy === "waive"} onClick={() => run("waive", () => api.waive({ ...base, stage: current.index, reason: reason.trim() }))}>
                确认放行
              </Button>
            </div>
          </div>
        ) : (
          <Button variant="ghost" className="block" icon={<ShieldCheck size={13} />} onClick={() => setWaiving(true)}>
            人工放行
          </Button>
        )}
      </>
    );
  } else if (latest.status === "succeeded") {
    const last = !next;
    body = (
      <>
        <div className="decision-state ok">
          <Check size={15} />
          <div>
            <p className="decision-title">「{current.name}」已完成</p>
            <p className="decision-text">检查左侧输出；需要调整可以继续对话，满意就确认。</p>
          </div>
        </div>
        <Button
          variant="primary"
          size="lg"
          className="block"
          icon={last ? <PackageCheck size={15} /> : <ArrowRight size={15} />}
          busy={busy === "advance"}
          onClick={() => run("advance", () => api.advance({ ...base, stage: current.index }))}
        >
          {last ? "确认交付" : `确认，进入「${next!.name}」`}
        </Button>
      </>
    );
  } else {
    body = (
      <>
        <div className="decision-state">
          <Square size={13} className="muted" />
          <div>
            <p className="decision-title">{latest.status === "cancelled" ? "运行已停止" : `状态：${latest.status}`}</p>
            <p className="decision-text">可以重新运行，或在左侧继续对话。</p>
          </div>
        </div>
        <Button variant="primary" size="lg" className="block" icon={<RotateCcw size={14} />} busy={busy === "rerun"} onClick={() => run("rerun", () => api.runStage({ ...base, stage: current.index }))}>
          重新运行「{current.name}」
        </Button>
      </>
    );
  }

  return (
    <section className="card card-decision" data-testid="decision">
      <div className="card-head">
        <span className="card-title">
          当前阶段 · {current.index + 1}/{detail.stages.length}
        </span>
      </div>
      {body}
    </section>
  );
}

function More({ detail, stage }: { detail: WorkDetail; stage: StageDetail }) {
  const { act } = useStudio();
  const [skipping, setSkipping] = useState(false);
  const [reason, setReason] = useState("");
  useEffect(() => {
    setSkipping(false);
    setReason("");
  }, [detail.id, stage.index]);
  const base = { project: detail.project, work: detail.id, stage: stage.index };
  const canSkip = stage.skippable && stage.state !== "skipped" && stage.index >= detail.stage && !(detail.running && stage.index === detail.stage);
  const canReopen = stage.state === "done" && !detail.running;
  const rework = [...detail.reworks].reverse().find((r) => r.to === stage.index) ?? null;
  const canUnskip = stage.state === "skipped" && stage.index > detail.stage && !detail.delivered;
  if (!canSkip && !canReopen && !canUnskip && !rework) return null;

  return (
    <section className="more">
      <div className="section-label">
        <span>更多</span>
      </div>
      {rework && (
        <p className="rework-line">
          <CornerUpLeft size={12} />
          <span>
            由「{detail.stages[rework.from]?.name ?? `阶段 ${rework.from + 1}`}」退回 · {rework.reason}
            <span className="muted"> · {relative(rework.at)}</span>
          </span>
        </p>
      )}
      {canSkip &&
        (skipping ? (
          <div className="waive">
            <input className="input" value={reason} onChange={(e) => setReason(e.target.value)} placeholder="跳过原因" aria-label="跳过原因" autoFocus />
            <div className="row-end">
              <Button variant="ghost" size="sm" onClick={() => setSkipping(false)}>
                取消
              </Button>
              <Button variant="secondary" size="sm" disabled={!reason.trim()} onClick={() => act(() => api.skip({ ...base, reason: reason.trim() }))}>
                确认跳过
              </Button>
            </div>
          </div>
        ) : (
          <button type="button" className="quiet-action" onClick={() => setSkipping(true)}>
            <SkipForward size={13} />
            跳过此阶段
          </button>
        ))}
      {canUnskip && (
        <button type="button" className="quiet-action" onClick={() => act(() => api.skip({ ...base, reason: "" }), `已恢复「${stage.name}」`)}>
          <Undo2 size={13} />
          取消跳过
        </button>
      )}
      {canReopen && (
        <button type="button" className="quiet-action" onClick={() => act(() => api.reopen(base), `已重新打开「${stage.name}」`)}>
          <RotateCcw size={13} />
          重新打开此阶段
        </button>
      )}
    </section>
  );
}
