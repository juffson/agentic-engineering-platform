import type { AttemptSummary, Client, RunStatus, Snapshot, StageDetail, WorkDetail } from "../api/types";

export const LIVE: RunStatus[] = ["initializing", "running", "claimed"];
export const isLive = (status: RunStatus | null | undefined) => !!status && LIVE.includes(status);

export function latestAttempt(stage: StageDetail | undefined): AttemptSummary | undefined {
  return stage?.attempts[stage.attempts.length - 1];
}

/** The stage ran before, but a later stage sent the item back and cleared its conclusion:
 *  its old result is history, and it must re-check (the backend resumes its own session). */
export function needsRecheck(stage: StageDetail | undefined): boolean {
  return !!stage && stage.attempts.length > 0 && stage.current_attempt === null && stage.state === "current";
}

export const isGate = (stage: StageDetail | undefined) => stage?.role === "Test" || stage?.role === "Accept";

/** Where a failed gate should send the work: the stage it named, else the nearest earlier Build stage. */
export function sendBackTarget(detail: WorkDetail, from: number): number | null {
  const latest = latestAttempt(detail.stages[from]);
  if (latest?.route_back && latest.route_back.stage < from) return latest.route_back.stage;
  for (let i = from - 1; i >= 0; i--) {
    if (detail.stages[i].role === "Build") return i;
  }
  return from > 0 ? from - 1 : null;
}

export type Visual = "done" | "current" | "running" | "failed" | "upcoming" | "skipped";

export function stageVisual(detail: WorkDetail, stage: StageDetail): Visual {
  if (stage.state === "skipped") return "skipped";
  if (stage.state === "done" || (detail.delivered && stage.index < detail.stage)) return "done";
  if (stage.state === "current" || stage.index === detail.stage) {
    const latest = latestAttempt(stage);
    if (detail.running && stage.attempts.some((a) => a.id === detail.running)) return "running";
    if (needsRecheck(stage)) return "current";
    if (latest && (latest.status === "failed" || latest.route_back)) return "failed";
    return "current";
  }
  return "upcoming";
}

export function clientLabel(snapshot: Snapshot | null, client: Client): string {
  return snapshot?.clients.find((c) => c.id === client)?.label ?? client;
}

export const modelLabel = (model: string | null | undefined) => (model && model.trim() ? model : "默认模型");

export function sandboxLabel(snapshot: Snapshot | null, id: string): string {
  return snapshot?.sandboxes.find((s) => s.id === id)?.label ?? id;
}
