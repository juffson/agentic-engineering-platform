import { useEffect, useState } from "react";
import { Download, FolderOpen, Plus, RefreshCw } from "lucide-react";
import type { ImportFound, SyncPlan, Template } from "../api/types";
import { api, errorMessage } from "../api/client";
import { useStudio } from "../state/store";
import { Button, Field, Modal, Segmented, Spinner } from "./ui";

export function NewWorkDialog({ onClose }: { onClose(): void }) {
  const { snapshot, project, act, selectWork } = useStudio();
  const templates = snapshot?.templates ?? [];
  const [name, setName] = useState("");
  const [template, setTemplate] = useState<Template>(templates.find((t) => t.id === "Software")?.id ?? templates[0]?.id ?? "Software");
  const [release, setRelease] = useState(false);
  const [repos, setRepos] = useState<string[]>(project?.repositories ?? []);
  const [busy, setBusy] = useState(false);
  const stages = templates.find((t) => t.id === template)?.stages ?? [];

  const create = async () => {
    if (!project) return;
    setBusy(true);
    const id = await act(() => api.createWork({ project: project.id, name: name.trim(), template, release, repositories: repos }), "事项已创建");
    setBusy(false);
    if (id) {
      selectWork(id);
      onClose();
    }
  };

  return (
    <Modal
      title="新建事项"
      onClose={onClose}
      width={560}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            取消
          </Button>
          <Button variant="primary" onClick={create} busy={busy} disabled={!name.trim() || repos.length === 0}>
            创建事项
          </Button>
        </>
      }
    >
      <Field label="名称">
        <input
          className="input"
          autoFocus
          value={name}
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && name.trim() && repos.length > 0 && void create()}
          placeholder="例如：自选股分组支持拖拽排序"
          aria-label="事项名称"
        />
      </Field>
      <div className="field-row">
        <Field label="类型">
          <Segmented
            label="类型"
            value={release ? "release" : "feature"}
            options={[
              { value: "feature", label: "功能" },
              { value: "release", label: "发布" },
            ]}
            onChange={(v) => setRelease(v === "release")}
          />
        </Field>
        <Field label="模板">
          <Segmented<Template> label="模板" value={template} options={templates.map((t) => ({ value: t.id, label: t.label }))} onChange={setTemplate} />
        </Field>
      </div>
      <ol className="template-preview" aria-label="阶段预览">
        {stages.map((s, i) => (
          <li key={s}>
            <span className="tp-num">{i + 1}</span>
            {s}
          </li>
        ))}
      </ol>
      <Field label="仓库" hint={repos.length === 0 ? "至少选择一个仓库" : undefined}>
        <div className="repo-checks">
          {(project?.repositories ?? []).map((r) => (
            <label key={r} className="check">
              <input type="checkbox" checked={repos.includes(r)} onChange={(e) => setRepos((cur) => (e.target.checked ? [...cur, r] : cur.filter((x) => x !== r)))} />
              <code>{r}</code>
            </label>
          ))}
          {project?.repositories.length === 0 && <p className="muted">项目还没有仓库，请先在「仓库」页添加。</p>}
        </div>
      </Field>
    </Modal>
  );
}

export function NewProjectDialog({ onClose }: { onClose(): void }) {
  const { act, notify } = useStudio();
  const [name, setName] = useState("");
  const [repos, setRepos] = useState("");
  const [busy, setBusy] = useState(false);
  const [found, setFound] = useState<Record<string, ImportFound>>({});
  const [memories, setMemories] = useState<Record<string, Set<string>>>({});
  const [branches, setBranches] = useState<Record<string, Set<string>>>({});
  const [scanning, setScanning] = useState<string[]>([]);
  const list = repos
    .split("\n")
    .map((s) => s.trim())
    .filter(Boolean);

  /** Look at what the coding agents already have for a folder; all memory files start selected. */
  const scan = async (path: string) => {
    if (!path || found[path] || scanning.includes(path)) return;
    setScanning((s) => [...s, path]);
    try {
      const f = await api.importScan({ path });
      setFound((m) => ({ ...m, [path]: f }));
      setMemories((m) => ({ ...m, [path]: new Set(f.memories.map((x) => x.file)) }));
      setBranches((m) => ({ ...m, [path]: new Set() }));
    } catch {
      // Not a repository (yet): creating the project reports it.
    } finally {
      setScanning((s) => s.filter((p) => p !== path));
    }
  };
  const pick = async () => {
    const path = await act(() => api.pickFolder({ prompt: "选择项目的 git 仓库文件夹" }));
    if (!path) return;
    setRepos((r) => (r.split("\n").map((x) => x.trim()).includes(path) ? r : r.trim() ? `${r.trim()}\n${path}` : path));
    if (!name.trim()) setName(path.split("/").pop() ?? "");
    void scan(path);
  };
  const create = async () => {
    setBusy(true);
    const pid = await act(() => api.createProject({ name: name.trim(), repositories: list }), "项目已创建");
    if (pid !== undefined) {
      let notes = 0;
      let items = 0;
      for (const path of list) {
        const f = found[path];
        if (!f) continue;
        const chosen = {
          memories: [...(memories[path] ?? [])],
          branches: f.worktrees
            .filter((w) => w.branch && branches[path]?.has(w.path))
            .map((w) => ({ branch: w.branch!, path: w.path })),
        };
        if (chosen.memories.length === 0 && chosen.branches.length === 0) continue;
        const r = await act(() => api.importApply({ project: pid, path, ...chosen }));
        if (r) {
          notes += r.notes;
          items += r.items.length;
        }
      }
      if (notes || items) notify(`已导入 ${notes} 条记忆、${items} 个事项`, "info");
      onClose();
    }
    setBusy(false);
  };
  return (
    <Modal
      title="新建项目"
      onClose={onClose}
      width={620}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            取消
          </Button>
          <Button variant="primary" icon={<Plus size={14} />} onClick={create} busy={busy} disabled={!name.trim() || scanning.length > 0}>
            {scanning.length > 0 ? "正在查找可导入内容…" : "创建项目"}
          </Button>
        </>
      }
    >
      <Field label="项目名称">
        <input className="input" autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder="例如：行情终端" aria-label="项目名称" />
      </Field>
      <Field label="仓库路径" hint="选择本地 git 仓库文件夹，或每行填一个绝对路径；之后也可以在「仓库」页添加。">
        <textarea
          className="input mono"
          rows={3}
          value={repos}
          onChange={(e) => setRepos(e.target.value)}
          onBlur={() => list.forEach((p) => void scan(p))}
          placeholder="/Users/you/code/app"
          aria-label="仓库路径"
        />
        <Button variant="secondary" size="sm" icon={<FolderOpen size={13} />} onClick={pick} className="pick-folder">
          选择文件夹…
        </Button>
      </Field>
      {scanning.length > 0 && (
        <p className="field-hint scan-hint">
          <Spinner size={12} /> 正在查找 Claude / Codex / ZCode 里可导入的记忆和分支…
        </p>
      )}
      {list
        .filter((p) => found[p])
        .map((p) => (
          <ImportSection
            key={p}
            found={found[p]}
            memories={memories[p] ?? new Set()}
            branches={branches[p] ?? new Set()}
            setMemories={(s) => setMemories((m) => ({ ...m, [p]: s }))}
            setBranches={(s) => setBranches((m) => ({ ...m, [p]: s }))}
          />
        ))}
    </Modal>
  );
}

/** What to bring over from Claude Code / Codex / ZCode for one repository. */
export function ImportSection({
  found,
  memories,
  branches,
  setMemories,
  setBranches,
}: {
  found: ImportFound;
  memories: Set<string>;
  branches: Set<string>;
  setMemories(s: Set<string>): void;
  setBranches(s: Set<string>): void;
}) {
  const toggle = (set: Set<string>, key: string, apply: (s: Set<string>) => void) => {
    const next = new Set(set);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    apply(next);
  };
  const all = found.memories.length > 0 && memories.size === found.memories.length;
  const repoName = found.repository.split("/").pop();
  const nothing = found.memories.length === 0 && found.worktrees.length === 0;
  return (
    <section className="import" data-testid="import">
      <div className="import-head">
        <span className="field-label">从 Claude / Codex / ZCode 导入 · {repoName}</span>
      </div>
      {nothing && <p className="field-hint">没有找到可导入的记忆或分支。</p>}
      {found.memories.length > 0 && (
        <div className="import-group">
          <label className="check">
            <input type="checkbox" checked={all} onChange={() => setMemories(all ? new Set() : new Set(found.memories.map((m) => m.file)))} />
            <span>
              Claude 记忆 {memories.size}/{found.memories.length} 条 → 项目记忆（各阶段 Agent 都会读到）
            </span>
          </label>
          <ul className="import-list">
            {found.memories.map((m) => (
              <li key={m.file}>
                <label className="check">
                  <input type="checkbox" checked={memories.has(m.file)} onChange={() => toggle(memories, m.file, setMemories)} />
                  <span>
                    <b>{m.title}</b>
                    {m.description && <span className="muted"> · {m.description}</span>}
                  </span>
                </label>
              </li>
            ))}
          </ul>
        </div>
      )}
      {found.worktrees.length > 0 && (
        <div className="import-group">
          <span className="import-sub">已有工作区分支 → 新事项（从「开发实现」继续，原工作区不动）</span>
          <ul className="import-list">
            {found.worktrees.map((w) => (
              <li key={w.path}>
                <label className={`check${w.branch ? "" : " disabled"}`}>
                  <input type="checkbox" disabled={!w.branch} checked={branches.has(w.path)} onChange={() => toggle(branches, w.path, setBranches)} />
                  <span>
                    <code>{w.branch ?? "（分离 HEAD）"}</code>
                    <span className="muted">
                      {" "}
                      · {w.source} · 领先 {w.ahead} 个提交
                      {w.dirty + w.untracked > 0 && ` · 原工作区有 ${w.dirty} 个改动、${w.untracked} 个未跟踪文件（不带过来）`}
                    </span>
                  </span>
                </label>
              </li>
            ))}
          </ul>
        </div>
      )}
      <p className="field-hint">
        {found.rules.length > 0 && <>{found.rules.join("、")} 已在仓库中，Agent 会直接读取。</>}
        会话记录：Claude {found.claude_sessions} · Codex {found.codex_sessions} · ZCode {found.zcode_tasks}（只统计，不导入）。
      </p>
    </section>
  );
}

/** Import into an existing project from one of its repositories, or sync it: make the
 *  project match what Claude Code and the repository's worktrees hold now. */
export function ImportDialog({ project, path, onClose }: { project: string; path: string; onClose(): void }) {
  const [mode, setMode] = useState<"sync" | "pick">("sync");
  return (
    <Modal title="导入与同步" onClose={onClose} width={640} footer={null}>
      <Segmented<"sync" | "pick">
        value={mode}
        onChange={(v) => setMode(v)}
        label="方式"
        options={[
          { value: "sync", label: "同步（与来源保持一致）" },
          { value: "pick", label: "挑选导入" },
        ]}
      />
      {mode === "sync" ? <SyncPane project={project} path={path} onClose={onClose} /> : <PickPane project={project} path={path} onClose={onClose} />}
    </Modal>
  );
}

function SyncPane({ project, path, onClose }: { project: string; path: string; onClose(): void }) {
  const { act, notify } = useStudio();
  const [plan, setPlan] = useState<SyncPlan | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [memories, setMemories] = useState(true);
  const [items, setItems] = useState(true);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    api
      .importSyncPlan({ project, path })
      .then(setPlan)
      .catch((e) => setError(errorMessage(e)));
  }, [project, path]);
  if (error) return <p className="inline-error">{error}</p>;
  if (!plan)
    return (
      <p className="field-hint scan-hint">
        <Spinner size={12} /> 正在对比 Claude 记忆和仓库工作区…
      </p>
    );
  const memChanges = plan.memories_add.length + plan.memories_update.length + plan.memories_remove.length;
  const itemChanges = plan.items_add.length + plan.items_remove.length;
  const apply = async () => {
    setBusy(true);
    const r = await act(() => api.importSync({ project, path, memories, items }));
    setBusy(false);
    if (r) {
      const parts = [];
      if (memories && memChanges) parts.push(`记忆 +${r.memories_add.length} ~${r.memories_update.length} −${r.memories_remove.length}`);
      if (items && itemChanges) parts.push(`事项 +${r.items_add.length} −${r.items_remove.length}`);
      notify(parts.length ? `已同步：${parts.join("，")}` : "已是最新，没有需要同步的内容", "info");
      onClose();
    }
  };
  return (
    <div className="sync" data-testid="sync">
      <label className="check">
        <input type="checkbox" checked={memories} onChange={(e) => setMemories(e.target.checked)} />
        <span>
          <b>Claude 记忆 → 项目记忆</b>
          <span className="muted">
            {" "}
            · 新增 {plan.memories_add.length} · 更新 {plan.memories_update.length} · 移出 {plan.memories_remove.length} · 未变 {plan.memories_unchanged}
          </span>
        </span>
      </label>
      {plan.memories_remove.length > 0 && (
        <ul className="sync-list">
          {plan.memories_remove.map((m) => (
            <li key={m.id} className="sync-del">
              − {m.title} <span className="muted">（Claude 里已删除，移出后仍留在历史）</span>
            </li>
          ))}
        </ul>
      )}
      <label className="check">
        <input type="checkbox" checked={items} onChange={(e) => setItems(e.target.checked)} />
        <span>
          <b>工作区分支 → 事项</b>
          <span className="muted">
            {" "}
            · 新增 {plan.items_add.length} · 移除 {plan.items_remove.length} · 保留 {plan.items_keep.length}
          </span>
        </span>
      </label>
      <ul className="sync-list">
        {plan.items_add.map((b) => (
          <li key={b.path} className="sync-add">
            + <code>{b.branch}</code>
          </li>
        ))}
        {plan.items_remove.map((i) => (
          <li key={i.id} className="sync-del">
            − <code>{i.name}</code> <span className="muted">（{i.reason}，未运行过）</span>
          </li>
        ))}
        {plan.items_stale.map((i) => (
          <li key={i.id}>
            ! <code>{i.name}</code> <span className="muted">（{i.reason}，保留，需要你处理）</span>
          </li>
        ))}
      </ul>
      <p className="field-hint">
        已运行过的事项不会被移除；移出的记忆保留在历史里。Agent 每次最多读取 32 条最新的项目记忆，记忆很多时只挑关键的导入效果更好。
      </p>
      <div className="modal-actions">
        <Button variant="ghost" onClick={onClose}>
          取消
        </Button>
        <Button variant="primary" icon={<RefreshCw size={14} />} busy={busy} onClick={apply} disabled={(!memories || memChanges === 0) && (!items || itemChanges === 0)}>
          同步
        </Button>
      </div>
    </div>
  );
}

function PickPane({ project, path, onClose }: { project: string; path: string; onClose(): void }) {
  const { act, notify } = useStudio();
  const [found, setFound] = useState<ImportFound | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [memories, setMemories] = useState<Set<string>>(new Set());
  const [branches, setBranches] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    api
      .importScan({ path })
      .then((f) => {
        setFound(f);
        setMemories(new Set(f.memories.map((m) => m.file)));
      })
      .catch((e) => setError(errorMessage(e)));
  }, [path]);
  const apply = async () => {
    if (!found) return;
    setBusy(true);
    const r = await act(() =>
      api.importApply({
        project,
        path,
        memories: [...memories],
        branches: found.worktrees.filter((w) => w.branch && branches.has(w.path)).map((w) => ({ branch: w.branch!, path: w.path })),
      }),
    );
    setBusy(false);
    if (r) {
      notify(`已导入 ${r.notes} 条记忆、${r.items.length} 个事项`, "info");
      onClose();
    }
  };
  return (
    <>
      {!found && !error && (
        <p className="field-hint scan-hint">
          <Spinner size={12} /> 正在查找 Claude / Codex / ZCode 里可导入的记忆和分支…
        </p>
      )}
      {error && <p className="inline-error">{error}</p>}
      {found && <ImportSection found={found} memories={memories} branches={branches} setMemories={setMemories} setBranches={setBranches} />}
      {found && <p className="field-hint">已导入过的记忆和分支不会重复添加；记忆文件变了会生成新版本。</p>}
      <div className="modal-actions">
        <Button variant="ghost" onClick={onClose}>
          取消
        </Button>
        <Button variant="primary" icon={<Download size={14} />} onClick={apply} busy={busy} disabled={!found || (memories.size === 0 && branches.size === 0)}>
          导入所选
        </Button>
      </div>
    </>
  );
}
