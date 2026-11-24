import { useEffect, useState } from "react";
import { Pencil } from "lucide-react";
import type { Client, Profile, Sandbox, StageDetail, WorkDetail } from "../api/types";
import { api } from "../api/client";
import { useStudio } from "../state/store";
import { clientLabel, modelLabel, sandboxLabel } from "../lib/work";
import { Chip, Segmented } from "./ui";

/** CLI / model / sandbox pickers for one stage; shared by the decision panel and the autopilot drawer. */
export function StagePickers({
  detail,
  stage,
  compact,
  disabled,
}: {
  detail: WorkDetail;
  stage: StageDetail;
  compact?: boolean;
  disabled?: boolean;
}) {
  const { snapshot, act, zcode, loadZcode } = useStudio();
  const clients = snapshot?.clients ?? [];
  const isZcode = stage.profile.client === "ZCode";
  useEffect(() => {
    if (isZcode) loadZcode();
  }, [isZcode, loadZcode]);
  const models: { value: string; label: string }[] = isZcode
    ? (zcode.models ?? []).map((m) => ({ value: m.selection, label: m.label }))
    : (clients.find((c) => c.id === stage.profile.client)?.models ?? []).map((m) => ({ value: m, label: m }));
  // The free-text box only shows a model that is not one of the chips.
  const custom = models.some((m) => m.value === stage.profile.model) ? "" : stage.profile.model;
  const [model, setModel] = useState(custom);
  useEffect(() => setModel(custom), [custom]);

  const saveProfile = (profile: Profile) =>
    act(() => api.updateStage({ project: detail.project, work: detail.id, stage: stage.index, profile }));
  const setClient = (client: Client) => {
    if (client === stage.profile.client) return;
    void saveProfile({ ...stage.profile, client, executable: "", model: "" });
  };
  const commitModel = (value: string) => {
    const next = value.trim();
    if (next === stage.profile.model) return;
    void saveProfile({ ...stage.profile, model: next });
  };
  const setSandbox = (sandbox: Sandbox) =>
    void act(() => api.updateStage({ project: detail.project, work: detail.id, stage: stage.index, sandbox }));

  const size = compact ? "sm" : "md";
  return (
    <div className={`pickers${compact ? " compact" : ""}`}>
      <div className="picker">
        {!compact && <span className="picker-label">CLI</span>}
        <Segmented
          label="CLI"
          size={size}
          value={stage.profile.client}
          options={clients.map((c) => ({ value: c.id, label: c.label }))}
          onChange={setClient}
          disabled={disabled}
        />
      </div>
      <div className="picker">
        {!compact && <span className="picker-label">模型</span>}
        <div className="chips">
          <Chip on={!stage.profile.model} onClick={() => commitModel("")} disabled={disabled}>
            默认
          </Chip>
          {models.map((m) => (
            <Chip key={m.value} on={stage.profile.model === m.value} onClick={() => commitModel(m.value)} disabled={disabled} title={m.value}>
              {m.label}
            </Chip>
          ))}
          {isZcode && zcode.loading && <span className="picker-note">正在读取 ZCode 模型…</span>}
          {isZcode && !zcode.loading && zcode.error && (
            <span className="picker-note">
              {zcode.error}{" "}
              <button type="button" className="link-btn inline" onClick={() => loadZcode(true)}>
                重试
              </button>
            </span>
          )}
          {!compact && (
            <input
              className="input input-sm model-input"
              value={model}
              placeholder="留空使用默认"
              aria-label="自定义模型"
              disabled={disabled}
              onChange={(e) => setModel(e.target.value)}
              onBlur={() => model.trim() !== custom && commitModel(model)}
              onKeyDown={(e) => e.key === "Enter" && model.trim() !== custom && commitModel(model)}
            />
          )}
        </div>
      </div>
      <div className="picker">
        {!compact && <span className="picker-label">权限</span>}
        <Segmented
          label="沙箱"
          size={size}
          value={stage.sandbox}
          options={(snapshot?.sandboxes ?? []).map((s) => ({ value: s.id, label: s.label }))}
          onChange={setSandbox}
          disabled={disabled}
        />
      </div>
    </div>
  );
}

export function StageConfigCard({ detail, stage, collapsed: initiallyCollapsed }: { detail: WorkDetail; stage: StageDetail; collapsed: boolean }) {
  const { snapshot, zcode } = useStudio();
  const [expanded, setExpanded] = useState(false);
  useEffect(() => setExpanded(false), [detail.id, stage.index]);
  const collapsed = initiallyCollapsed && !expanded;
  const summary = `${clientLabel(snapshot, stage.profile.client)} · ${(stage.profile.client === "ZCode" && zcode.models?.find((m) => m.selection === stage.profile.model)?.label) || modelLabel(stage.profile.model)} · ${sandboxLabel(snapshot, stage.sandbox)}`;

  return (
    <section className="card">
      <div className="card-head">
        <span className="card-title">此阶段配置</span>
        {collapsed && (
          <button type="button" className="link-btn" onClick={() => setExpanded(true)}>
            <Pencil size={12} />
            修改
          </button>
        )}
      </div>
      {collapsed ? <p className="config-summary">{summary}</p> : <StagePickers detail={detail} stage={stage} />}
    </section>
  );
}
