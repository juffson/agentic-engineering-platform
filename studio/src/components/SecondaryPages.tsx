import { useCallback, useEffect, useState } from "react";
import { Check, ChevronDown, ChevronRight, Download, FolderGit2, FolderOpen, History, Pencil, Plus, RotateCcw, Sparkles } from "lucide-react";
import { ImportDialog } from "./Dialogs";
import type { FolderResolved, MemoryNote, ProjectSummary, TidyStatus, Upstream, UpstreamView } from "../api/types";
import { api, errorMessage } from "../api/client";
import { useStudio } from "../state/store";
import { clock } from "../lib/format";
import { Button, SectionLabel, Segmented, Spinner } from "./ui";
import { FolderScan } from "./FolderScan";

const KIND_LABEL: Record<string, string> = {
  note: "笔记",
  decision: "决策",
  convention: "约定",
  delivery: "交付",
};

export function NoteList({ notes }: { notes: MemoryNote[] }) {
  const sorted = [...notes].sort((a, b) => b.id - a.id);
  return (
    <ul className="memory-list">
      {sorted.map((n) => (
        <li key={n.id} className={`memory${n.superseded ? " superseded" : ""}`}>
          <div className="memory-meta">
            <span className="memory-id">#{n.id}</span>
            <span className="tag">{KIND_LABEL[n.kind] ?? n.kind}</span>
            <span>{n.author}</span>
            <span>{clock(n.created_at)}</span>
            {n.superseded && <span className="tag tag-dashed">已被取代</span>}
          </div>
          <p className="memory-body">{n.body}</p>
        </li>
      ))}
    </ul>
  );
}

export function MemoryPage({ project }: { project: ProjectSummary }) {
  const { act, revision } = useStudio();
  const [notes, setNotes] = useState<MemoryNote[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      setNotes(await api.memories({ project: project.id }));
      setError(null);
    } catch (e) {
      setError(errorMessage(e));
    }
  }, [project.id]);

  useEffect(() => {
    void load();
  }, [load, revision]);

  const add = async () => {
    setBusy(true);
    const id = await act(() => api.addMemory({ project: project.id, text: text.trim() }), "已写入项目记忆");
    setBusy(false);
    if (id !== undefined) setText("");
  };

  return (
    <div className="page">
      <div className="page-inner">
        <header className="page-head">
          <h1>项目记忆</h1>
          <p>每个阶段的 Agent 开始工作前都会读取这些笔记。记录决策、约定与交付要点，保持简短。</p>
        </header>
        <section className="card add-card">
          <textarea
            className="input"
            rows={3}
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && (e.metaKey || e.ctrlKey) && text.trim() && void add()}
            placeholder="新增一条记忆，例如：行情推送统一使用 protobuf 编码"
            aria-label="新增记忆"
          />
          <div className="row-between">
            <span className={`count${text.length > 1000 ? " over" : ""}`}>{text.length}/1000</span>
            <Button variant="primary" icon={<Plus size={14} />} disabled={!text.trim() || text.length > 1000} busy={busy} onClick={add}>
              添加记忆
            </Button>
          </div>
        </section>
        {notes && notes.some((n) => !n.superseded) && <TidyPanel project={project.id} notes={notes} onApplied={load} />}
        <SectionLabel right={notes && <span className="muted">{notes.filter((n) => !n.superseded).length} 条</span>}>全部记忆</SectionLabel>
        {error && <p className="inline-error">{error}</p>}
        {!notes && !error && <Spinner />}
        {notes && notes.length === 0 && <p className="empty-text">还没有记忆。归档交付时也可以自动写入。</p>}
        {notes && <VersionedNotes project={project.id} notes={notes} />}
      </div>
    </div>
  );
}

/** Project memory, newest versions only; each can be edited (a new version) and shows its
 *  older versions with what changed, any of which can be restored. */
function VersionedNotes({ project, notes }: { project: string; notes: MemoryNote[] }) {
  const byId = new Map(notes.map((n) => [n.id, n]));
  const current = notes.filter((n) => !n.superseded).sort((a, b) => b.id - a.id);
  const history = (n: MemoryNote) => {
    const chain: MemoryNote[] = [];
    let prev = n.replaces != null ? byId.get(n.replaces) : undefined;
    while (prev) {
      chain.push(prev);
      prev = prev.replaces != null ? byId.get(prev.replaces) : undefined;
    }
    return chain;
  };
  return (
    <ul className="memory-list">
      {current.map((n) => (
        <VersionedNote key={n.id} project={project} note={n} older={history(n)} />
      ))}
    </ul>
  );
}

function VersionedNote({ project, note, older }: { project: string; note: MemoryNote; older: MemoryNote[] }) {
  const { act } = useStudio();
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState(note.body);
  const [showHistory, setShowHistory] = useState(false);
  const [busy, setBusy] = useState(false);
  const save = async (body: string, done: string) => {
    setBusy(true);
    const id = await act(() => api.reviseMemory({ project, id: note.id, text: body }), done);
    setBusy(false);
    if (id !== undefined) setEditing(false);
  };
  const versions = [note, ...older];
  return (
    <li className="memory" data-testid="memory-note">
      <div className="memory-meta">
        <span className="memory-id">#{note.id}</span>
        <span className="tag">{KIND_LABEL[note.kind] ?? note.kind}</span>
        <span>{note.author}</span>
        <span>{clock(note.created_at)}</span>
        {older.length > 0 && <span className="tag tag-dashed">第 {versions.length} 版</span>}
        <span className="memory-actions">
          {!editing && (
            <button type="button" className="link-btn" onClick={() => (setText(note.body), setEditing(true))}>
              <Pencil size={12} /> 编辑
            </button>
          )}
          {older.length > 0 && (
            <button type="button" className="link-btn" onClick={() => setShowHistory((v) => !v)} aria-expanded={showHistory}>
              <History size={12} /> 历史 {older.length}
            </button>
          )}
        </span>
      </div>
      {editing ? (
        <div className="memory-edit">
          <textarea className="input" rows={4} value={text} onChange={(e) => setText(e.target.value)} aria-label="编辑记忆" />
          <div className="row-between">
            <span className={`count${text.length > 1000 ? " over" : ""}`}>{text.length}/1000 · 保存后旧版本保留在历史里</span>
            <span className="row-gap">
              <Button variant="ghost" size="sm" onClick={() => setEditing(false)}>
                取消
              </Button>
              <Button variant="primary" size="sm" busy={busy} disabled={!text.trim() || text.length > 1000 || text.trim() === note.body} onClick={() => save(text.trim(), "已保存为新版本")}>
                保存为新版本
              </Button>
            </span>
          </div>
        </div>
      ) : (
        <p className="memory-body">{note.body}</p>
      )}
      {showHistory && (
        <ol className="memory-history">
          {older.map((v, i) => (
            <li key={v.id}>
              <div className="memory-meta">
                <span className="memory-id">#{v.id}</span>
                <span>第 {versions.length - 1 - i} 版</span>
                <span>{v.author}</span>
                <span>{clock(v.created_at)}</span>
                <span className="memory-actions">
                  <button type="button" className="link-btn" disabled={busy} onClick={() => save(v.body, `已恢复到 #${v.id} 的内容`)}>
                    <RotateCcw size={12} /> 恢复此版本
                  </button>
                </span>
              </div>
              <p className="memory-diff" aria-label={`#${v.id} 到下一版的改动`}>
                {diffText(v.body, versions[i].body).map((seg, k) => (
                  <span key={k} className={seg.kind === "same" ? undefined : `diff-${seg.kind}`}>
                    {seg.text}
                  </span>
                ))}
              </p>
            </li>
          ))}
        </ol>
      )}
    </li>
  );
}

/** Ask a CLI to tidy project memory, then review its proposal and apply what you accept. */
function TidyPanel({ project, notes, onApplied }: { project: string; notes: MemoryNote[]; onApplied(): void }) {
  const { act, notify } = useStudio();
  const [status, setStatus] = useState<TidyStatus | null>(null);
  const [open, setOpen] = useState(false);
  const [client, setClient] = useState<"Claude" | "Codex">("Claude");
  const [model, setModel] = useState("");
  const [instruction, setInstruction] = useState("");
  const [chosen, setChosen] = useState<Set<number>>(new Set());
  const [busy, setBusy] = useState(false);
  const [dismissed, setDismissed] = useState<string | null>(null);

  const poll = useCallback(async () => {
    try {
      const s = await api.tidyStatus({ project });
      setStatus(s);
      return s;
    } catch {
      return null;
    }
  }, [project]);
  useEffect(() => {
    void poll().then((s) => {
      if (s?.proposal) setChosen(new Set(s.proposal.ops.map((_, i) => i).filter((i) => !s.applied?.includes(i))));
    });
  }, [poll]);
  const running = status?.status === "running" || status?.status === "starting";
  useEffect(() => {
    if (!running) return;
    const t = window.setInterval(async () => {
      const s = await poll();
      if (s?.proposal) setChosen(new Set(s.proposal.ops.map((_, i) => i)));
    }, 2000);
    return () => window.clearInterval(t);
  }, [running, poll]);

  const current = notes.filter((n) => !n.superseded);
  const byId = new Map(notes.map((n) => [n.id, n]));
  const start = async () => {
    setBusy(true);
    const id = await act(() => api.tidyStart({ project, client, model, instruction }));
    setBusy(false);
    if (id) {
      setOpen(false);
      await poll();
    }
  };
  const apply = async () => {
    if (!status?.id) return;
    setBusy(true);
    const n = await act(() => api.tidyApply({ project, job: status.id!, ops: [...chosen].sort((a, b) => a - b) }));
    setBusy(false);
    if (n !== undefined) {
      notify(`已应用 ${n} 项整理，旧内容都在各条记忆的历史里`, "info");
      onApplied();
      await poll();
    }
  };

  const proposal = status?.status === "succeeded" && status.id !== dismissed ? status.proposal : null;
  const pending = proposal?.ops.map((op, i) => ({ op, i })).filter(({ i }) => !status?.applied?.includes(i)) ?? [];
  return (
    <section className="card tidy" data-testid="tidy">
      <div className="row-between">
        <div>
          <span className="card-title">整理记忆</span>
          <p className="card-text">
            让 Claude 或 Codex 通读 {current.length} 条记忆，提出合并重复、改写含糊、移出过时的方案；你逐条确认后才会生效，旧内容留在历史里可恢复。
            {current.length > 32 && " Agent 每次只读最新 32 条，记忆多时尤其值得整理。"}
          </p>
        </div>
        {!running && !open && (
          <Button variant="secondary" size="sm" icon={<Sparkles size={13} />} onClick={() => setOpen(true)}>
            {proposal && pending.length ? "重新整理" : "开始整理"}
          </Button>
        )}
      </div>
      {open && (
        <div className="tidy-form">
          <Segmented<"Claude" | "Codex">
            value={client}
            onChange={(v) => setClient(v)}
            label="CLI"
            options={[
              { value: "Claude", label: "Claude" },
              { value: "Codex", label: "Codex" },
            ]}
          />
          <input className="input mono" value={model} onChange={(e) => setModel(e.target.value)} placeholder="模型（留空使用 CLI 默认）" aria-label="整理模型" />
          <textarea
            className="input"
            rows={2}
            value={instruction}
            onChange={(e) => setInstruction(e.target.value)}
            placeholder="整理要求（可选），例如：战斗相关的合成一组；已合并的 PR 记录都移出"
            aria-label="整理要求"
          />
          <div className="row-gap">
            <Button variant="ghost" size="sm" onClick={() => setOpen(false)}>
              取消
            </Button>
            <Button variant="primary" size="sm" icon={<Sparkles size={13} />} busy={busy} onClick={start}>
              开始整理
            </Button>
          </div>
        </div>
      )}
      {running && (
        <p className="field-hint scan-hint">
          <Spinner size={12} /> {status?.client} 正在整理 {current.length} 条记忆，通常需要一两分钟…
        </p>
      )}
      {status?.status === "failed" && status.id !== dismissed && (
        <p className="inline-error">
          整理失败：{status.error}
          <button type="button" className="link-btn" onClick={() => setDismissed(status.id)}>
            关闭
          </button>
        </p>
      )}
      {proposal && (
        <div className="tidy-proposal">
          {proposal.summary && <p className="tidy-summary">{proposal.summary}</p>}
          {pending.length === 0 ? (
            <p className="field-hint">{proposal.ops.length ? "这次整理的方案已全部处理。" : "Agent 认为当前记忆不需要整理。"}</p>
          ) : (
            <ul className="tidy-ops">
              {pending.map(({ op, i }) => {
                const ids = op.op === "merge" ? op.ids : [op.id];
                const stale = ids.some((id) => byId.get(id)?.superseded !== false);
                return (
                  <li key={i} className={stale ? "stale" : undefined}>
                    <label className="check">
                      <input
                        type="checkbox"
                        disabled={stale}
                        checked={chosen.has(i) && !stale}
                        onChange={() => {
                          const next = new Set(chosen);
                          if (next.has(i)) next.delete(i);
                          else next.add(i);
                          setChosen(next);
                        }}
                      />
                      <span>
                        <span className={`tag tidy-${op.op}`}>{op.op === "merge" ? "合并" : op.op === "rewrite" ? "改写" : "移出"}</span>{" "}
                        {ids.map((id) => `#${id}`).join(" + ")} <span className="muted">· {op.why}</span>
                        {stale && <span className="muted">（记忆已变化，不能应用）</span>}
                      </span>
                    </label>
                    <div className="tidy-detail">
                      {op.op === "retire" && <p className="memory-diff diff-del">{byId.get(op.id)?.body}</p>}
                      {op.op === "rewrite" && (
                        <p className="memory-diff">
                          {diffText(byId.get(op.id)?.body ?? "", op.text).map((seg, k) => (
                            <span key={k} className={seg.kind === "same" ? undefined : `diff-${seg.kind}`}>
                              {seg.text}
                            </span>
                          ))}
                        </p>
                      )}
                      {op.op === "merge" && (
                        <>
                          {op.ids.map((id) => (
                            <p key={id} className="memory-diff diff-del">
                              #{id} {byId.get(id)?.body}
                            </p>
                          ))}
                          <p className="memory-diff diff-add">→ {op.text}</p>
                        </>
                      )}
                    </div>
                  </li>
                );
              })}
            </ul>
          )}
          {pending.length > 0 && (
            <div className="row-gap tidy-actions">
              <Button variant="ghost" size="sm" onClick={() => setDismissed(status!.id)}>
                放弃这次方案
              </Button>
              <Button variant="primary" size="sm" icon={<Check size={13} />} busy={busy} disabled={chosen.size === 0} onClick={apply}>
                应用所选（{pending.filter(({ i }) => chosen.has(i)).length}）
              </Button>
            </div>
          )}
        </div>
      )}
    </section>
  );
}

/** Character diff of two short texts (memory notes are at most 1000 characters). */
function diffText(a: string, b: string): { kind: "same" | "add" | "del"; text: string }[] {
  const x = [...a];
  const y = [...b];
  const n = x.length;
  const m = y.length;
  const dp = new Uint16Array((n + 1) * (m + 1));
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      dp[i * (m + 1) + j] = x[i] === y[j] ? dp[(i + 1) * (m + 1) + j + 1] + 1 : Math.max(dp[(i + 1) * (m + 1) + j], dp[i * (m + 1) + j + 1]);
    }
  }
  const out: { kind: "same" | "add" | "del"; text: string }[] = [];
  const push = (kind: "same" | "add" | "del", ch: string) => {
    const last = out[out.length - 1];
    if (last && last.kind === kind) last.text += ch;
    else out.push({ kind, text: ch });
  };
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (x[i] === y[j]) {
      push("same", x[i]);
      i++;
      j++;
    } else if (dp[(i + 1) * (m + 1) + j] >= dp[i * (m + 1) + j + 1]) push("del", x[i++]);
    else push("add", y[j++]);
  }
  while (i < n) push("del", x[i++]);
  while (j < m) push("add", y[j++]);
  return out;
}

export function ReposPage({ project }: { project: ProjectSummary }) {
  const { act, notify } = useStudio();
  const [path, setPath] = useState("");
  const [busy, setBusy] = useState(false);
  const [resolved, setResolved] = useState<Extract<FolderResolved, { kind: "folder" }> | null>(null);
  const [sel, setSel] = useState<Set<string>>(new Set());
  const [deepening, setDeepening] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const added = new Set(project.repositories);

  /** 「添加仓库」: a plain repository is added at once; a workspace folder opens the list. */
  const add = async () => {
    setError(null);
    setBusy(true);
    try {
      const r = await api.resolveFolder({ path: path.trim() });
      if (r.kind === "repository") {
        const ok = await act(() => api.addRepository({ project: project.id, path: path.trim() }), "仓库已添加");
        if (ok !== undefined) setPath("");
      } else if (r.kind === "folder") {
        setResolved(r);
        setSel(new Set(r.repositories.map((x) => x.path)));
      } else {
        setError(r.message);
      }
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  const pick = async () => {
    const picked = await act(() => api.pickFolder({ prompt: "选择 git 仓库文件夹，或包含多个仓库的工作区文件夹" }));
    if (picked) setPath(picked);
  };
  const deeper = async () => {
    if (!resolved || deepening) return;
    setDeepening(true);
    const before = new Set(resolved.repositories.map((r) => r.path));
    try {
      const r = await api.resolveFolder({ path: resolved.root, depth: Math.min(resolved.depth + 1, 4) });
      if (r.kind === "folder") {
        setResolved(r);
        setSel(new Set(r.repositories.filter((x) => sel.has(x.path) || !before.has(x.path)).map((x) => x.path)));
      }
    } finally {
      setDeepening(false);
    }
  };
  const addChosen = async () => {
    if (!resolved) return;
    setAdding(true);
    const chosen = resolved.repositories.filter((r) => sel.has(r.path)).map((r) => r.path);
    const hadRoot = !!project.workspace?.root;
    const out = await act(() => api.addRepositories({ project: project.id, paths: chosen, workspaceRoot: resolved.root }));
    setAdding(false);
    if (out) {
      const k = out.added.length;
      let text = `已添加 ${k} 个仓库`;
      if (!hadRoot && out.workspace_root) text = `已添加 ${k} 个仓库，并记录为项目工作区`;
      else if (out.kept_root) text = `已添加 ${k} 个仓库；项目工作区仍是 ${out.kept_root.split("/").filter(Boolean).pop()}`;
      notify(text, "info");
      setResolved(null);
      setPath("");
    }
  };
  const allAdded = !!resolved && resolved.repositories.every((r) => added.has(r.path));
  return (
    <div className="page">
      <div className="page-inner">
        <header className="page-head">
          <h1>仓库</h1>
          <p>「{project.name}」中的事项会在这些仓库上创建各自的工作区与 aep-stage 分支。</p>
        </header>
        <section className="card add-card">
          <div className="add-row">
            <input
              className="input mono"
              value={path}
              onChange={(e) => {
                setPath(e.target.value);
                setResolved(null);
              }}
              onKeyDown={(e) => e.key === "Enter" && path.trim() && void add()}
              placeholder="/Users/you/code/repository"
              aria-label="仓库路径"
            />
            <Button variant="secondary" icon={<FolderOpen size={14} />} onClick={pick}>
              选择文件夹…
            </Button>
            <Button variant="primary" icon={<Plus size={14} />} disabled={!path.trim()} busy={busy} onClick={add}>
              添加仓库
            </Button>
          </div>
          {error && <p className="inline-error">{error}</p>}
          {resolved && !allAdded && (
            <>
              <FolderScan resolved={resolved} selected={sel} onChange={setSel} added={added} onDeeper={() => void deeper()} deepening={deepening} />
              <div className="row-gap folder-scan-actions-bar">
                <Button variant="ghost" size="sm" onClick={() => setResolved(null)}>
                  取消
                </Button>
                <Button variant="primary" size="sm" busy={adding} disabled={resolved.repositories.filter((r) => sel.has(r.path) && !added.has(r.path)).length === 0} onClick={addChosen}>
                  添加所选（{resolved.repositories.filter((r) => sel.has(r.path) && !added.has(r.path)).length}）
                </Button>
              </div>
            </>
          )}
          {resolved && allAdded && (
            <div className="row-between folder-scan-actions-bar">
              <p className="field-hint">这些仓库都已在项目里</p>
              <Button variant="ghost" size="sm" onClick={() => setResolved(null)}>
                关闭
              </Button>
            </div>
          )}
        </section>
        {project.workspace?.root && <WorkspaceCard project={project} />}
        <SectionLabel right={<span className="muted">{project.repositories.length} 个</span>}>已添加</SectionLabel>
        {project.repositories.length === 0 && <p className="empty-text">还没有仓库。</p>}
        <ul className="repo-list">
          {project.repositories.map((r) => (
            <RepoRow key={r} path={r} project={project.id} />
          ))}
        </ul>
      </div>
    </div>
  );
}

/** Read-only view of the project's workspace folder with its shared notes, plus a rescan
 *  that reuses the add-list (interaction §4.1, §4.3). */
function WorkspaceCard({ project }: { project: ProjectSummary }) {
  const ws = project.workspace!;
  const { act, notify } = useStudio();
  const [rescanning, setRescanning] = useState(false);
  const [resolved, setResolved] = useState<Extract<FolderResolved, { kind: "folder" }> | null>(null);
  const [sel, setSel] = useState<Set<string>>(new Set());
  const [deepening, setDeepening] = useState(false);
  const [adding, setAdding] = useState(false);
  const [fresh, setFresh] = useState<string | null>(null);
  const added = new Set(project.repositories);

  const rescan = async () => {
    setRescanning(true);
    try {
      const r = await api.resolveFolder({ path: ws.root });
      if (r.kind === "folder") {
        const fresh1 = r.repositories.filter((x) => !added.has(x.path));
        if (fresh1.length === 0) {
          setFresh(`没有新的仓库，${r.repositories.length} 个都已在项目里`);
          window.setTimeout(() => setFresh(null), 3000);
          setResolved(null);
        } else {
          setResolved(r);
          setSel(new Set(fresh1.map((x) => x.path)));
        }
      } else {
        notify(r.message || "重新扫描失败", "error");
      }
    } finally {
      setRescanning(false);
    }
  };
  const deeper = async () => {
    if (!resolved || deepening) return;
    setDeepening(true);
    const before = new Set(resolved.repositories.map((r) => r.path));
    try {
      const r = await api.resolveFolder({ path: ws.root, depth: Math.min(resolved.depth + 1, 4) });
      if (r.kind === "folder") {
        setResolved(r);
        setSel(new Set(r.repositories.filter((x) => sel.has(x.path) || !before.has(x.path)).map((x) => x.path)));
      }
    } finally {
      setDeepening(false);
    }
  };
  const addChosen = async () => {
    if (!resolved) return;
    setAdding(true);
    const chosen = resolved.repositories.filter((r) => sel.has(r.path) && !added.has(r.path)).map((r) => r.path);
    const out = await act(() => api.addRepositories({ project: project.id, paths: chosen, workspaceRoot: ws.root }));
    setAdding(false);
    if (out) {
      notify(`已添加 ${out.added.length} 个仓库`, "info");
      setResolved(null);
    }
  };
  const name = ws.root.split("/").filter(Boolean).pop();
  return (
    <section className="card workspace-root" data-testid="workspace-root">
      <div className="workspace-head">
        <FolderGit2 size={16} className="muted" />
        <span className="workspace-name">{name}</span>
        <span className="workspace-actions">
          <Button variant="secondary" size="sm" busy={rescanning} disabled={!ws.exists} onClick={rescan}>
            重新扫描
          </Button>
          <button type="button" className="icon-btn" title="在访达中显示" aria-label="在访达中显示" onClick={() => act(() => api.reveal({ path: ws.root }))}>
            <FolderOpen size={15} />
          </button>
        </span>
      </div>
      <code className="workspace-path">{ws.root}</code>
      {!ws.exists ? (
        <p className="workspace-notes warn">找不到这个文件夹，阶段不会读取工作区说明</p>
      ) : ws.notes.length > 0 ? (
        <p className="workspace-notes">
          说明文件：
          {ws.notes.map((n, i) => (
            <span key={n}>
              {i > 0 && "、"}
              <code>{n}</code>
            </span>
          ))}{" "}
          · 各阶段开始前会读取
        </p>
      ) : (
        <p className="workspace-notes muted">顶层没有 AGENTS.md 或 CLAUDE.md，阶段不会额外读取说明</p>
      )}
      {fresh && <p className="field-hint">{fresh}</p>}
      {resolved && (
        <>
          <FolderScan resolved={resolved} selected={sel} onChange={setSel} added={added} onDeeper={() => void deeper()} deepening={deepening} />
          <div className="row-gap folder-scan-actions-bar">
            <Button variant="ghost" size="sm" onClick={() => setResolved(null)}>
              取消
            </Button>
            <Button variant="primary" size="sm" busy={adding} disabled={resolved.repositories.filter((r) => sel.has(r.path) && !added.has(r.path)).length === 0} onClick={addChosen}>
              添加所选（{resolved.repositories.filter((r) => sel.has(r.path) && !added.has(r.path)).length}）
            </Button>
          </div>
        </>
      )}
    </section>
  );
}

function RepoRow({ path, project }: { path: string; project: string }) {
  const { act } = useStudio();
  const [importing, setImporting] = useState(false);
  const [open, setOpen] = useState(false);
  const [notes, setNotes] = useState<MemoryNote[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      setNotes(await api.repoMemories({ path }));
      setError(null);
    } catch (e) {
      setError(errorMessage(e));
    }
  }, [path]);

  useEffect(() => {
    if (open) void load();
  }, [open, load]);

  const add = async () => {
    setBusy(true);
    const id = await act(() => api.addRepoMemory({ path, text: text.trim() }), "已写入仓库记忆");
    setBusy(false);
    if (id !== undefined) {
      setText("");
      void load();
    }
  };

  return (
    <li className={`repo${open ? " open" : ""}`}>
      <div className="repo-row">
        <button type="button" className="repo-toggle" onClick={() => setOpen((o) => !o)} aria-expanded={open}>
          {open ? <ChevronDown size={14} className="muted" /> : <ChevronRight size={14} className="muted" />}
          <FolderGit2 size={15} className="muted" />
          <span className="repo-text">
            <span className="repo-name">{path.split("/").filter(Boolean).pop()}</span>
            <code className="repo-path">{path}</code>
          </span>
          <span className="repo-hint">{open ? "收起记忆" : "仓库记忆"}</span>
        </button>
        <Button variant="secondary" size="sm" icon={<Download size={13} />} onClick={() => setImporting(true)}>
          导入 / 同步
        </Button>
        <button type="button" className="icon-btn" title="在访达中显示" aria-label="在访达中显示" onClick={() => act(() => api.reveal({ path }))}>
          <FolderOpen size={15} />
        </button>
      </div>
      {importing && <ImportDialog project={project} path={path} onClose={() => setImporting(false)} />}
      {open && (
        <div className="repo-body">
          <UpstreamCard path={path} project={project} />
          <p className="card-text">在这个仓库上运行的所有阶段，除了项目记忆，也会读取这些笔记。</p>
          <div className="repo-add">
            <textarea
              className="input"
              rows={2}
              value={text}
              onChange={(e) => setText(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && (e.metaKey || e.ctrlKey) && text.trim() && text.length <= 1000 && void add()}
              placeholder="新增一条仓库记忆，例如：构建命令、目录约定、已知坑"
              aria-label="新增仓库记忆"
            />
            <div className="row-between">
              <span className={`count${text.length > 1000 ? " over" : ""}`}>{text.length}/1000</span>
              <Button variant="secondary" size="sm" icon={<Plus size={13} />} disabled={!text.trim() || text.length > 1000} busy={busy} onClick={add}>
                添加
              </Button>
            </div>
          </div>
          {error && <p className="inline-error">{error}</p>}
          {!notes && !error && <Spinner size={12} />}
          {notes && notes.length === 0 && <p className="empty-text">这个仓库还没有记忆。</p>}
          {notes && <NoteList notes={notes} />}
        </div>
      )}
    </li>
  );
}

/**
 * Remote and delivery for one repository: items start from (and keep up with) the remote target
 * branch, and delivery either merges locally or pushes the branch and opens a PR.
 */
function UpstreamCard({ path, project }: { path: string; project: string }) {
  const { act } = useStudio();
  const [view, setView] = useState<UpstreamView | null>(null);
  const [draft, setDraft] = useState<Upstream | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let live = true;
    api
      .upstream({ project, repository: path })
      .then((v) => {
        if (!live) return;
        setView(v);
        setDraft(v.configured ?? v.detected);
      })
      .catch((e) => live && setError(errorMessage(e)));
    return () => {
      live = false;
    };
  }, [project, path]);

  const save = async (up: Upstream | null) => {
    setBusy(true);
    const v = await act(() => api.setUpstream({ project, repository: path, upstream: up }), up ? "已保存远端设置" : "已停用远端联动");
    setBusy(false);
    if (v) {
      setView(v);
      setDraft(v.configured ?? v.detected);
    }
  };

  const set = (patch: Partial<Upstream>) => setDraft((d) => (d ? { ...d, ...patch } : d));
  const on = !!view?.configured;
  const changed = JSON.stringify(view?.configured) !== JSON.stringify(draft);

  return (
    <section className="card" data-testid="upstream">
      <div className="card-head">
        <span className="card-title">远端与交付</span>
        <span className="muted">{on ? `事项从 ${view!.configured!.remote}/${view!.configured!.target} 开启` : "未启用：事项从当前检出的分支开启，交付合并到本地分支"}</span>
      </div>
      {error && <p className="inline-error">{error}</p>}
      {!view && !error && <Spinner size={12} />}
      {view && !draft && <p className="empty-text">这个仓库没有远端。</p>}
      {draft && (
        <>
          <div className="upstream-form">
            <label className="field">
              <span className="field-label">远端</span>
              <input className="input input-sm" aria-label="远端" value={draft.remote} onChange={(e) => set({ remote: e.target.value.trim() })} />
            </label>
            <label className="field">
              <span className="field-label">目标分支</span>
              <input className="input input-sm" aria-label="目标分支" value={draft.target} onChange={(e) => set({ target: e.target.value.trim() })} />
            </label>
            <label className="field">
              <span className="field-label">推送分支前缀</span>
              <input className="input input-sm" aria-label="推送分支前缀" value={draft.branch_prefix} onChange={(e) => set({ branch_prefix: e.target.value.trim() })} />
            </label>
            <label className="field">
              <span className="field-label">代码平台</span>
              <select className="input input-sm" aria-label="代码平台" value={draft.forge ?? ""} onChange={(e) => set({ forge: (e.target.value || null) as Upstream["forge"] })}>
                <option value="">未知（只推送，网页上提 PR）</option>
                <option value="gitea">Gitea（推送时直接建 PR）</option>
                <option value="gitlab">GitLab（推送时直接建 MR）</option>
                <option value="github">GitHub（用 gh 建 PR）</option>
              </select>
            </label>
            <label className="field">
              <span className="field-label">仓库网页地址</span>
              <input className="input input-sm" aria-label="仓库网页地址" placeholder="https://host/owner/repo" value={draft.web ?? ""} onChange={(e) => set({ web: e.target.value.trim() || null })} />
            </label>
          </div>
          <div className="field">
            <span className="field-label">交付方式</span>
            <Segmented
              size="sm"
              label="交付方式"
              value={draft.delivery}
              options={[
                { value: "pull_request", label: "推送并提 PR" },
                { value: "local", label: "合并到本地分支" },
              ]}
              onChange={(v) => set({ delivery: v })}
            />
          </div>
          <div className="row-between">
            {on ? (
              <Button variant="ghost" size="sm" disabled={busy} onClick={() => save(null)}>
                停用远端联动
              </Button>
            ) : (
              <span className="field-hint">保存后，新事项从远端目标分支开启，阶段之间自动跟上它。</span>
            )}
            <Button variant="primary" size="sm" busy={busy} disabled={on && !changed} onClick={() => save(draft)}>
              {on ? "保存" : "启用"}
            </Button>
          </div>
        </>
      )}
    </section>
  );
}
