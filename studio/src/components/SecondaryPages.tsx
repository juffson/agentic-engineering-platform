import { useCallback, useEffect, useState } from "react";
import { Check, ChevronDown, ChevronRight, Download, FolderGit2, FolderOpen, History, Pencil, Plus, RotateCcw, Sparkles } from "lucide-react";
import { ImportDialog } from "./Dialogs";
import type { MemoryNote, ProjectSummary, TidyStatus } from "../api/types";
import { api, errorMessage } from "../api/client";
import { useStudio } from "../state/store";
import { clock } from "../lib/format";
import { Button, SectionLabel, Segmented, Spinner } from "./ui";

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
  const { act } = useStudio();
  const [path, setPath] = useState("");
  const [busy, setBusy] = useState(false);
  const add = async () => {
    setBusy(true);
    const ok = await act(() => api.addRepository({ project: project.id, path: path.trim() }), "仓库已添加");
    setBusy(false);
    if (ok !== undefined) setPath("");
  };
  const pick = async () => {
    const picked = await act(() => api.pickFolder({ prompt: "选择要添加的 git 仓库文件夹" }));
    if (picked) setPath(picked);
  };
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
              onChange={(e) => setPath(e.target.value)}
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
        </section>
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
