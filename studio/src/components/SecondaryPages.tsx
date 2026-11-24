import { useCallback, useEffect, useState } from "react";
import { ChevronDown, ChevronRight, FolderGit2, FolderOpen, Plus } from "lucide-react";
import type { MemoryNote, ProjectSummary } from "../api/types";
import { api, errorMessage } from "../api/client";
import { useStudio } from "../state/store";
import { clock } from "../lib/format";
import { Button, SectionLabel, Spinner } from "./ui";

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
        <SectionLabel right={notes && <span className="muted">{notes.length} 条</span>}>全部记忆</SectionLabel>
        {error && <p className="inline-error">{error}</p>}
        {!notes && !error && <Spinner />}
        {notes && notes.length === 0 && <p className="empty-text">还没有记忆。归档交付时也可以自动写入。</p>}
        {notes && <NoteList notes={notes} />}
      </div>
    </div>
  );
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
            <Button variant="primary" icon={<Plus size={14} />} disabled={!path.trim()} busy={busy} onClick={add}>
              添加仓库
            </Button>
          </div>
        </section>
        <SectionLabel right={<span className="muted">{project.repositories.length} 个</span>}>已添加</SectionLabel>
        {project.repositories.length === 0 && <p className="empty-text">还没有仓库。</p>}
        <ul className="repo-list">
          {project.repositories.map((r) => (
            <RepoRow key={r} path={r} />
          ))}
        </ul>
      </div>
    </div>
  );
}

function RepoRow({ path }: { path: string }) {
  const { act } = useStudio();
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
        <button type="button" className="icon-btn" title="在访达中显示" aria-label="在访达中显示" onClick={() => act(() => api.reveal({ path }))}>
          <FolderOpen size={15} />
        </button>
      </div>
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
