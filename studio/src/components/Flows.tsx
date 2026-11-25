import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { ArrowDown, ArrowUp, ChevronDown, ChevronRight, Plus, Trash2, TriangleAlert } from "lucide-react";
import { api, errorMessage } from "../api/client";
import type { FlowDef, FlowInput, FlowStage, ProjectSummary, Role } from "../api/types";
import { BUILTIN_FLOW_LABELS, copyLabel, defaultNewFlow, ROLE_HINT, ROLE_LABEL, trimFlowText, validateFlow, type FlowIssue } from "../lib/flow";
import { useStudio } from "../state/store";
import { Button, Drawer, Modal, Segmented, Spinner } from "./ui";

type DeleteTarget = { id: string; label: string; in_use: number };

const roles = (Object.keys(ROLE_LABEL) as Role[]).map((value) => ({ value, label: ROLE_LABEL[value] }));
const deletedFlowError = "这套流程已被删除";

export function FlowsPage({ project }: { project: ProjectSummary }) {
  const { snapshotError, reloadSnapshot, notify } = useStudio();
  const [editor, setEditor] = useState<FlowInput | null>(null);
  const [builtInOpen, setBuiltInOpen] = useState<string | null>(null);
  const [deleting, setDeleting] = useState<DeleteTarget | null>(null);
  const [busy, setBusy] = useState(false);
  const [retrying, setRetrying] = useState(false);
  const content = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLElement | null>(null);

  useLayoutEffect(() => {
    if (content.current) content.current.inert = Boolean(editor || deleting);
  }, [editor, deleting]);

  const builtIn = project.flows.filter((flow) => flow.builtin && flow.id !== "Legacy");
  const custom = project.flows.filter((flow) => !flow.builtin);
  const labels = [...Object.values(BUILTIN_FLOW_LABELS), ...custom.filter((flow) => flow.id !== editor?.id).map((flow) => flow.label)];
  const inUse = custom.find((flow) => flow.id === editor?.id)?.in_use ?? 0;

  const rememberTrigger = () => {
    trigger.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  };
  const restoreFocus = () => {
    const element = trigger.current;
    trigger.current = null;
    if (element) requestAnimationFrame(() => element.focus());
  };
  const openEditor = (flow: FlowInput) => {
    rememberTrigger();
    setEditor(flow);
  };
  const closeEditor = (restore = true) => {
    setEditor(null);
    if (restore) restoreFocus();
    else trigger.current = null;
  };
  const openCopy = (flow: FlowDef) => openEditor({ label: copyLabel(flow.label, labels), stages: flow.stages });
  const askDelete = (target: DeleteTarget) => {
    rememberTrigger();
    setDeleting(target);
  };
  const closeDelete = () => {
    setDeleting(null);
    restoreFocus();
  };
  const deleteFlow = async () => {
    if (!deleting || busy) return;
    setBusy(true);
    try {
      await api.deleteFlow({ project: project.id, id: deleting.id });
      await reloadSnapshot();
      closeDelete();
      notify("流程已删除", "info");
    } catch (error) {
      notify(errorMessage(error));
      void reloadSnapshot();
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="page flows-page">
      <div className="page-inner" ref={content}>
        <header className="page-head">
          <h1>流程</h1>
          <p>新建事项时选择一套流程。事项按创建时的流程推进，之后改动或删除流程不影响已创建的事项。</p>
        </header>

        <section className="flow-section" aria-labelledby="builtin-flows">
          <div className="row-between flow-section-head">
            <h2 id="builtin-flows">内置</h2>
            <span className="count">{builtIn.length} 套</span>
          </div>
          <ul className="flow-list card">
            {builtIn.map((flow) => (
              <li key={flow.id} className="flow-list-row">
                <div className="flow-list-line">
                  <button type="button" className="flow-inspect" aria-expanded={builtInOpen === flow.id} onClick={() => setBuiltInOpen(builtInOpen === flow.id ? null : flow.id)}>
                    {builtInOpen === flow.id ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                    <FlowSummary flow={flow} />
                  </button>
                  <Button size="sm" onClick={() => openCopy(flow)}>复制</Button>
                </div>
                {builtInOpen === flow.id && <ReadonlyStages stages={flow.stages} />}
              </li>
            ))}
          </ul>
        </section>

        <section className="flow-section" aria-labelledby="project-flows">
          <div className="row-between flow-section-head">
            <h2 id="project-flows">本项目</h2>
            <Button size="sm" variant="primary" icon={<Plus size={14} />} onClick={() => openEditor(defaultNewFlow())}>新建流程</Button>
          </div>
          {retrying ? <p className="flow-loading" role="status"><Spinner /> 正在读取流程…</p> : snapshotError && (
            <div className="flow-read-error">
              <p className="inline-error" role="alert">读取流程失败：{snapshotError}</p>
              <Button size="sm" onClick={async () => {
                setRetrying(true);
                await reloadSnapshot();
                setRetrying(false);
              }}>重试</Button>
            </div>
          )}
          {custom.length === 0 && project.broken_flows.length === 0 && !snapshotError && !retrying && (
            <p className="empty-text">还没有自定义流程。从内置流程「复制」一份开始改，或新建一套。</p>
          )}
          {(custom.length > 0 || project.broken_flows.length > 0) && (
            <ul className="flow-list card">
              {custom.map((flow) => (
                <li key={flow.id} className="flow-list-row">
                  <div className="flow-list-line">
                    <FlowSummary flow={flow} />
                    <div className="flow-actions">
                      <Button size="sm" onClick={() => openEditor({ id: flow.id, label: flow.label, stages: flow.stages })}>编辑</Button>
                      <Button size="sm" onClick={() => openCopy(flow)}>复制</Button>
                      <Button size="sm" variant="danger" onClick={() => askDelete(flow)}>删除</Button>
                    </div>
                  </div>
                  <FlowWarnings messages={flow.warnings} />
                </li>
              ))}
              {project.broken_flows.map(({ id, label, error }, index) => {
                const name = label || id || "未知流程";
                const reserved = id !== null && Object.keys(BUILTIN_FLOW_LABELS).includes(id);
                const shared = id !== null && (project.flows.some((flow) => flow.id === id)
                  || project.broken_flows.some((flow, position) => position !== index && flow.id === id));
                const deleteError = id === null ? "缺少流程 ID，无法单独删除。"
                  : reserved || shared ? "流程 ID 与其他流程冲突，无法单独删除。" : null;
                return (
                  <li key={index} className="flow-list-row">
                    <div className="flow-list-line">
                      <div className="flow-summary">
                        <strong>{name}</strong>
                        <p className="inline-error">无法读取：{error}</p>
                        {deleteError && <p className="field-hint">{deleteError}</p>}
                      </div>
                      <Button size="sm" variant="danger" disabled={deleteError !== null} onClick={() => {
                        if (id !== null && !deleteError) askDelete({ id, label: name, in_use: 0 });
                      }}>删除</Button>
                    </div>
                  </li>
                );
              })}
            </ul>
          )}
        </section>
      </div>

      {editor && <FlowEditor project={project.id} initial={editor} takenLabels={labels} inUse={inUse} onClose={closeEditor} />}
      {deleting && (
        <Modal title={`删除流程「${deleting.label}」？`} onClose={() => !busy && closeDelete()} footer={
          <>
            <Button autoFocus disabled={busy} onClick={closeDelete}>取消</Button>
            <Button variant="danger" busy={busy} onClick={() => void deleteFlow()}>删除</Button>
          </>
        }>
          <p>
            {deleting.in_use > 0 && `${deleting.in_use} 个事项正在使用它，它们会继续按创建时的流程推进。 `}
            删除后新建事项不能再选它。此操作不能撤销。
          </p>
        </Modal>
      )}
    </div>
  );
}

function FlowSummary({ flow }: { flow: FlowDef }) {
  const chain = flow.stages.map((stage) => stage.name).join(" → ");
  return (
    <span className="flow-summary">
      <span className="flow-summary-head">
        <strong>{flow.label}</strong>
        {flow.builtin && <span className="tag">内置</span>}
        <span className="count">{flow.stages.length} 个阶段{flow.in_use > 0 && ` · ${flow.in_use} 个事项在用`}</span>
      </span>
      <span className="flow-chain muted" title={chain}>{chain}</span>
    </span>
  );
}

function FlowWarnings({ messages }: { messages: string[] }) {
  if (messages.length === 0) return null;
  return (
    <ul className="flow-warnings">
      {messages.map((message) => <li key={message}><TriangleAlert size={15} aria-label="警告" />{message}</li>)}
    </ul>
  );
}

// Keep references attached to row identities across renames and reordering.
type Target = number | string;
type StageRow = Omit<FlowStage, "back_to"> & { key: number; targets: Target[]; removed?: boolean };

function stageRows(stages: FlowStage[]): StageRow[] {
  const keys = new Map(stages.map((stage, key) => [trimFlowText(stage.name), key]));
  return stages.map(({ back_to, ...stage }, key) => ({
    ...stage, key, targets: back_to.map((name) => keys.get(trimFlowText(name)) ?? name),
  }));
}

function toFlow(id: string | undefined, label: string, rows: StageRow[]): FlowInput {
  const names = new Map(rows.map((row) => [row.key, row.name]));
  const stages = rows.map(({ key: _key, targets, removed: _removed, ...stage }) => ({
    ...stage,
    back_to: targets.flatMap((target) => {
      const name = typeof target === "number" ? names.get(target) : target;
      return name === undefined ? [] : [name];
    }),
  }));
  return { id, label, stages };
}

function stageName(row: StageRow, index: number) {
  return trimFlowText(row.name) || `第 ${index + 1} 个阶段`;
}

function StageHeading({ row, index, expanded, errors = 0, onClick }: {
  row: StageRow; index: number; expanded: boolean; errors?: number; onClick(): void;
}) {
  return (
    <button type="button" className="flow-stage-toggle" aria-expanded={expanded} onClick={onClick}>
      {expanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
      <span className="count">{index + 1}</span>
      <strong>{trimFlowText(row.name) || "未命名阶段"}</strong>
      <span className="tag">{ROLE_LABEL[row.role]}</span>
      {row.skippable && <span className="tag">可跳过</span>}
      {row.targets.length > 0 && <span className="tag">退回限制 {row.targets.length}</span>}
      {errors > 0 && <span className="dot dot-failed" aria-label={`${errors} 个错误`} title={`${errors} 个错误`} />}
    </button>
  );
}

function ReadonlyStages({ stages }: { stages: FlowStage[] }) {
  const rows = stageRows(stages);
  const [expanded, setExpanded] = useState<number | null>(null);
  return (
    <ol className="flow-rows flow-readonly" aria-label="内置流程阶段">
      {rows.map((row, index) => (
        <li key={row.key} className="flow-stage">
          <StageHeading row={row} index={index} expanded={expanded === row.key} onClick={() => setExpanded(expanded === row.key ? null : row.key)} />
          {expanded === row.key && <StageFields row={row} rows={rows} index={index} disabled />}
        </li>
      ))}
    </ol>
  );
}

type Deletion = { key: number; name: string; affected: number };

function FlowEditor({ project, initial, takenLabels, inUse, onClose }: {
  project: string; initial: FlowInput; takenLabels: string[]; inUse: number; onClose(restoreFocus?: boolean): void;
}) {
  const { registerNavigationGuard, reloadSnapshot, notify } = useStudio();
  const [id, setId] = useState(initial.id || undefined);
  const [label, setLabel] = useState(initial.label);
  const [rows, setRows] = useState(() => stageRows(initial.stages));
  const [original] = useState(() => JSON.stringify(toFlow(initial.id || undefined, initial.label, stageRows(initial.stages))));
  const nextKey = useRef(initial.stages.length);
  const [expanded, setExpanded] = useState<number | null>(null);
  const [seenNames, setSeenNames] = useState<Set<number>>(() => new Set());
  const [seenLabel, setSeenLabel] = useState(false);
  const [showAllErrors, setShowAllErrors] = useState(false);
  const [skipReset, setSkipReset] = useState<number | null>(null);
  const [deletions, setDeletions] = useState<Deletion[]>([]);
  const [busy, setBusy] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [leaving, setLeaving] = useState<(() => void) | null>(null);
  const [focus, setFocus] = useState<{ field: string } | null>(null);
  const root = useRef<HTMLDivElement>(null);
  const editorFocus = useRef<HTMLElement | null>(null);

  const activeRows = rows.filter((row) => !row.removed);
  const activeKeys = new Set(activeRows.map((row) => row.key));
  const visibleRows = activeRows.map((row) => ({
    ...row, targets: row.targets.filter((target) => typeof target === "string" || activeKeys.has(target)),
  }));
  const draft = toFlow(id, label, visibleRows);
  const dirty = JSON.stringify(draft) !== original;
  const { errors, warnings } = validateFlow(draft, takenLabels);
  const visibleErrors = errors.filter((issue) => {
    if (showAllErrors || !issue.message.endsWith("不能为空")) return true;
    return issue.stage === null ? seenLabel : seenNames.has(activeRows[issue.stage].key);
  });

  const requestLeave = (navigate?: () => void) => {
    if (busy || leaving) return;
    const leave = () => {
      onClose(!navigate);
      navigate?.();
    };
    if (!dirty) {
      leave();
      return;
    }
    editorFocus.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setLeaving(() => leave);
  };
  useLayoutEffect(() => registerNavigationGuard(requestLeave));

  useLayoutEffect(() => {
    const drawer = root.current?.querySelector<HTMLElement>(".overlay-drawer");
    if (drawer) drawer.inert = leaving !== null;
  }, [leaving]);

  useEffect(() => {
    if (!dirty) return;
    const onUnload = (event: BeforeUnloadEvent) => event.preventDefault();
    window.addEventListener("beforeunload", onUnload);
    return () => window.removeEventListener("beforeunload", onUnload);
  }, [dirty]);

  useLayoutEffect(() => {
    if (focus) root.current?.querySelector<HTMLElement>(`[data-flow-focus="${focus.field}"]`)?.focus();
  }, [focus]);

  const focusField = (field: string) => setFocus({ field });
  const clearDeletion = useCallback((key: number) => setDeletions((entries) => entries.filter((entry) => entry.key !== key)), []);
  const updateRows = (updated: StageRow[]) => {
    setRows(updated);
    setSaveError((error) => error === deletedFlowError ? error : null);
  };

  const addStage = () => {
    const delivery = activeRows.find((row) => row.role === "Deliver");
    const after = activeRows.find((row) => row.key === expanded);
    const beforeDelivery = delivery && (!after || activeRows.indexOf(after) >= activeRows.indexOf(delivery));
    const index = beforeDelivery ? rows.indexOf(delivery) : after ? rows.indexOf(after) + 1 : rows.length;
    const key = nextKey.current++;
    const added: StageRow = { key, name: "", role: "Plan", brief: "", skippable: false, targets: [] };
    updateRows([...rows.slice(0, index), added, ...rows.slice(index)]);
    setExpanded(key);
    focusField(`${key}-name`);
  };

  const moveStage = (row: StageRow, index: number, direction: -1 | 1) => {
    const other = activeRows[index + direction];
    if (!other) return;
    const from = rows.findIndex((entry) => entry.key === row.key);
    const to = rows.indexOf(other);
    const next = [...rows];
    [next[from], next[to]] = [next[to], next[from]];
    updateRows(next);
    focusField(`${row.key}-move-${direction}`);
  };

  const deleteStage = (row: StageRow, index: number) => {
    const affected = activeRows.filter((entry) => entry.key !== row.key && entry.targets.includes(row.key)).length;
    updateRows(rows.map((entry) => entry.key === row.key ? { ...entry, removed: true } : entry));
    if (expanded === row.key) setExpanded(null);
    setDeletions((entries) => [{ key: row.key, name: stageName(row, index), affected }, ...entries]);
    focusField("add");
  };

  const jumpToError = () => {
    const first = errors[0];
    setShowAllErrors(true);
    if (first.stage === null) {
      focusField(first.field === "label" ? "label" : "stages");
      return;
    }
    const key = activeRows[first.stage].key;
    setExpanded(key);
    focusField(`${key}-${first.field}`);
  };

  const save = async (asNew = false) => {
    if (busy || errors.length > 0) return;
    const input = asNew ? { ...draft, id: undefined } : draft;
    if (asNew) setId(undefined);
    setBusy(true);
    setSaveError(null);
    try {
      await api.saveFlow({ project, flow: input });
      await reloadSnapshot();
      onClose();
      notify(input.id && inUse > 0 ? `流程已保存 · 已用它创建的 ${inUse} 个事项不受影响` : "流程已保存", "info");
    } catch (error) {
      const message = errorMessage(error);
      setSaveError(message);
      notify(message);
      void reloadSnapshot();
    } finally {
      setBusy(false);
    }
  };

  const continueEditing = () => {
    setLeaving(null);
    const element = editorFocus.current;
    if (element && root.current?.contains(element)) requestAnimationFrame(() => element.focus());
    else focusField("label");
  };
  const labelIssues = visibleErrors.filter((issue) => issue.field === "label");

  return (
    <div className="flow-editor-shell" ref={root}>
      <Drawer title={id ? "编辑流程" : "新建流程"} onClose={() => requestLeave()} footer={
        <div className="flow-editor-footer">
          {deletions.length > 0 && <div className="flow-undo-list">
            {deletions.map((entry) => <UndoStageNotice key={entry.key} deletion={entry} onExpire={clearDeletion} disabled={busy} onUndo={() => {
              updateRows(rows.map((row) => row.key === entry.key ? { ...row, removed: false } : row));
              clearDeletion(entry.key);
            }} />)}
          </div>}
          {saveError && <p className="inline-error flow-save-error" role="alert">{saveError}</p>}
          <div className="flow-footer-actions">
            {errors.length > 0 && <button type="button" className="flow-error-link" onClick={jumpToError} disabled={busy}>
              {errors.length} 个错误，修正后才能保存
            </button>}
            <div className="flow-actions">
              <Button disabled={busy} onClick={() => requestLeave()}>取消</Button>
              {id && saveError === deletedFlowError
                ? <Button variant="primary" busy={busy} disabled={errors.length > 0} onClick={() => void save(true)}>另存为新流程</Button>
                : <Button variant="primary" busy={busy} disabled={errors.length > 0} onClick={() => void save()}>保存</Button>}
            </div>
          </div>
        </div>
      }>
        <label className="field">
          <span className="field-label">名称</span>
          <input className="input" aria-label="流程名称" aria-invalid={labelIssues.length > 0} data-flow-focus="label" autoFocus value={label} disabled={busy} onChange={(event) => {
            setLabel(event.target.value);
            setSaveError((error) => error === deletedFlowError ? error : null);
          }} onBlur={() => setSeenLabel(true)} />
          <Issues issues={labelIssues} />
        </label>
        <div className="row-between flow-stage-head" data-flow-focus="stages" tabIndex={-1}>
          <h3>阶段（{activeRows.length}）</h3>
          <Button size="sm" icon={<Plus size={14} />} data-flow-focus="add" disabled={busy} onClick={addStage}>添加阶段</Button>
        </div>
        <Issues issues={visibleErrors.filter((issue) => issue.stage === null && issue.field !== "label")} />
        <FlowWarnings messages={warnings.map((issue) => issue.message)} />
        <ol className="flow-rows" aria-label="流程阶段">
          {visibleRows.map((row, index) => {
            const issues = visibleErrors.filter((issue) => issue.stage === index);
            const name = stageName(row, index);
            return (
              <li key={row.key} className="flow-stage">
                <div className="flow-stage-line">
                  <StageHeading row={row} index={index} expanded={expanded === row.key} errors={issues.length} onClick={() => setExpanded(expanded === row.key ? null : row.key)} />
                  <div className="flow-stage-actions">
                    <button type="button" className="icon-btn" title="上移" aria-label={`上移「${name}」`} aria-disabled={index === 0} disabled={busy} data-flow-focus={`${row.key}-move--1`} onClick={() => moveStage(row, index, -1)}><ArrowUp size={15} /></button>
                    <button type="button" className="icon-btn" title="下移" aria-label={`下移「${name}」`} aria-disabled={index === activeRows.length - 1} disabled={busy} data-flow-focus={`${row.key}-move-1`} onClick={() => moveStage(row, index, 1)}><ArrowDown size={15} /></button>
                    <button type="button" className="icon-btn flow-stage-delete" title="删除阶段" aria-label={`删除阶段「${name}」`} disabled={busy} onClick={() => deleteStage(row, index)}><Trash2 size={15} /></button>
                  </div>
                </div>
                {expanded === row.key && (
                  <StageFields row={row} rows={visibleRows} index={index} issues={issues} disabled={busy} skipReset={skipReset === row.key}
                    onNameBlur={() => setSeenNames((current) => new Set(current).add(row.key))}
                    onToggleTarget={(target) => updateRows(rows.map((entry) => entry.key === row.key ? {
                      ...entry,
                      targets: entry.targets.includes(target) ? entry.targets.filter((value) => value !== target) : [...entry.targets, target],
                    } : entry))}
                    onChange={(change) => {
                      const reset = change.role && change.role !== "Plan" && row.skippable;
                      if (change.role) setSkipReset(reset ? row.key : null);
                      updateRows(rows.map((entry) => entry.key === row.key
                        ? { ...entry, ...change, ...(reset ? { skippable: false } : {}) }
                        : entry));
                    }}
                  />
                )}
              </li>
            );
          })}
        </ol>
      </Drawer>
      {leaving && (
        <Modal title={`放弃对「${trimFlowText(label) || "未命名流程"}」的修改？`} onClose={continueEditing} footer={
          <>
            <Button autoFocus onClick={continueEditing}>继续编辑</Button>
            <Button variant="danger" onClick={leaving}>放弃</Button>
          </>
        }>
          <p>离开后，当前修改将丢失。</p>
        </Modal>
      )}
    </div>
  );
}

function Issues({ issues }: { issues: FlowIssue[] }) {
  if (issues.length === 0) return null;
  return <span className="flow-issues inline-error">{issues.map(({ message }, index) => <span key={index}>{message}</span>)}</span>;
}

function StageFields({ row, rows, index, disabled, issues = [], skipReset, onChange, onNameBlur, onToggleTarget }: {
  row: StageRow; rows: StageRow[]; index: number; disabled: boolean; issues?: FlowIssue[]; skipReset?: boolean;
  onChange?(change: Partial<StageRow>): void; onNameBlur?(): void; onToggleTarget?(target: Target): void;
}) {
  const errors = (field: FlowIssue["field"]) => issues.filter((issue) => issue.field === field);
  const targetsLabel = useId();
  const earlier = rows.slice(0, index);
  const invalidTargets = row.targets.filter((target) => !earlier.some((stage) => stage.key === target || trimFlowText(stage.name) === target));

  return (
    <fieldset className="flow-stage-fields" disabled={disabled} aria-label={`「${stageName(row, index)}」阶段详情`}>
      <label className="field">
        <span className="field-label">名称</span>
        <input className="input" aria-label="阶段名称" aria-invalid={errors("name").length > 0} data-flow-focus={`${row.key}-name`} value={row.name} onBlur={onNameBlur} onChange={(event) => onChange?.({ name: event.target.value })} />
        <Issues issues={errors("name")} />
      </label>
      <div className="field">
        <span className="field-label">角色</span>
        <Segmented label="角色" size="sm" value={row.role} options={roles} disabled={disabled} onChange={(role) => onChange?.({ role })} />
        <span className="field-hint">{ROLE_HINT[row.role]}</span>
      </div>
      <label className="field">
        <span className="field-label">职责</span>
        <textarea className="input" rows={3} value={row.brief} onChange={(event) => onChange?.({ brief: event.target.value })} />
      </label>
      <div className="field" data-flow-focus={`${row.key}-skippable`} tabIndex={-1}>
        <label className="check">
          <input type="checkbox" checked={row.skippable} disabled={!row.skippable && (index === 0 || row.role !== "Plan")} onChange={(event) => onChange?.({ skippable: event.target.checked })} />
          可跳过
        </label>
        <span className="field-hint">只有非首个的规划阶段可以跳过</span>
        {skipReset && <span className="field-hint" role="status">非规划阶段不可跳过，已取消勾选</span>}
        <Issues issues={errors("skippable")} />
      </div>
      {(earlier.length > 0 || errors("back_to").length > 0) && (
        <div className="field" data-flow-focus={`${row.key}-back_to`} tabIndex={-1}>
          {earlier.length > 0 && <>
            <span className="field-label" id={targetsLabel}>可退回到</span>
            <div className="flow-targets" role="group" aria-labelledby={targetsLabel}>
              {earlier.map((stage, position) => (
                <label className="check" key={stage.key}>
                  <input type="checkbox" checked={row.targets.includes(stage.key)} onChange={() => onToggleTarget?.(stage.key)} />
                  {stageName(stage, position)}
                </label>
              ))}
            </div>
            <span className="field-hint">不勾选 = 之前的任意阶段</span>
          </>}
          <Issues issues={errors("back_to")} />
          {invalidTargets.length > 0 && <div className="flow-targets">
            {[...new Set(invalidTargets)].map((target) => {
              const position = rows.findIndex((stage) => stage.key === target);
              const name = position < 0 ? target : stageName(rows[position], position);
              return <Button size="sm" variant="ghost" key={target} onClick={() => onToggleTarget?.(target)}>移除退回目标「{name}」</Button>;
            })}
          </div>}
        </div>
      )}
    </fieldset>
  );
}

function UndoStageNotice({ deletion, disabled, onExpire, onUndo }: {
  deletion: Deletion; disabled: boolean; onExpire(key: number): void; onUndo(): void;
}) {
  useEffect(() => {
    const timeout = window.setTimeout(() => onExpire(deletion.key), 6000);
    return () => window.clearTimeout(timeout);
  }, [deletion.key, onExpire]);

  return (
    <div className="flow-undo">
      <span role="status">已删除「{deletion.name}」{deletion.affected > 0 && ` · 同时从 ${deletion.affected} 个阶段的退回目标中移除`}</span>
      <Button variant="ghost" size="sm" disabled={disabled} onClick={onUndo}>撤销</Button>
    </div>
  );
}
