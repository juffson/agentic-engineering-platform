import { useCallback, useEffect, useState } from "react";
import { Archive, ArchiveRestore, BookmarkPlus, ChevronDown, ChevronRight, GitBranch, GitMerge, Info, Trash2 } from "lucide-react";
import type { MergePreview, WorkDetail } from "../api/types";
import { api, errorMessage } from "../api/client";
import { useStudio } from "../state/store";
import { clock, relative } from "../lib/format";
import { Button, Spinner } from "./ui";

const BRANCH_STATUS: Record<string, string> = {
  fast_forward: "可快进",
  already_contained: "已包含",
  diverged: "已分叉",
};

export function DeliveryCard({ detail }: { detail: WorkDetail }) {
  const { act } = useStudio();
  const [target, setTarget] = useState<string | undefined>(undefined);
  const [preview, setPreview] = useState<MergePreview | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [showDiff, setShowDiff] = useState(false);
  const [busy, setBusy] = useState(false);
  const merged = detail.deliveries[0]?.merged ?? null;

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const p = await api.mergePreview({ project: detail.project, work: detail.id, target });
      setPreview(p);
      setLoadError(null);
    } catch (e) {
      setLoadError(errorMessage(e));
    } finally {
      setLoading(false);
    }
  }, [detail.project, detail.id, target]);

  useEffect(() => {
    void load();
  }, [load, merged?.sha]);

  useEffect(() => setShowDiff(false), [target]);

  const confirm = async () => {
    if (!preview) return;
    setBusy(true);
    await act(() => api.mergeConfirm({ project: detail.project, work: detail.id, target: preview.target, digest: preview.digest }), `已合并到 ${preview.target}`);
    setBusy(false);
    void load();
  };

  return (
    <section className="card card-decision" data-testid="delivery">
      <div className="card-head">
        <span className="card-title">交付 · 合并到本地分支</span>
        {loading && <Spinner size={12} />}
      </div>
      {merged && (
        <p className="merged-line">
          <GitMerge size={13} />
          <span>
            已合并到 <code>{merged.target}</code> · <code>{merged.sha.slice(0, 8)}</code>
            <span className="muted"> · {relative(merged.at)}</span>
          </span>
        </p>
      )}
      {loadError && <p className="inline-error">{loadError}</p>}
      {preview && (
        <>
          <div className="merge-route">
            <code className="branch-name" title={preview.candidate}>
              {preview.candidate || detail.branch}
            </code>
            <span className="muted">→</span>
            <select className="input input-sm" value={preview.target} onChange={(e) => setTarget(e.target.value)} aria-label="目标分支">
              {preview.branches.map((b) => (
                <option key={b.name} value={b.name}>
                  {b.name}
                  {BRANCH_STATUS[b.status] ? ` · ${BRANCH_STATUS[b.status]}` : ""}
                </option>
              ))}
            </select>
          </div>
          {preview.error ? (
            <div className="notice-box">
              <Info size={14} />
              <span>{preview.error}</span>
            </div>
          ) : (
            <>
              <div className="diffstat">
                <span>{preview.files_changed} 个文件</span>
                <span className="plus">+{preview.insertions}</span>
                <span className="minus">−{preview.deletions}</span>
              </div>
              {preview.stat && <pre className="stat-box">{preview.stat}</pre>}
              <Button variant="primary" size="lg" className="block" icon={<GitMerge size={14} />} busy={busy} onClick={confirm}>
                合并到 {preview.target}
              </Button>
              <button type="button" className="link-btn" onClick={() => setShowDiff((s) => !s)} aria-expanded={showDiff}>
                {showDiff ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
                {showDiff ? "收起差异" : "查看完整差异"}
              </button>
              {showDiff && <DiffView diff={preview.diff} truncated={preview.truncated} />}
            </>
          )}
        </>
      )}
    </section>
  );
}

function DiffView({ diff, truncated }: { diff: string; truncated: boolean }) {
  return (
    <div className="diff" role="region" aria-label="差异">
      <pre>
        {diff.split("\n").map((line, i) => {
          const cls =
            line.startsWith("+++") || line.startsWith("---") || line.startsWith("diff ") || line.startsWith("index ") || line.startsWith("new file")
              ? "d-meta"
              : line.startsWith("@@")
                ? "d-hunk"
                : line.startsWith("+")
                  ? "d-add"
                  : line.startsWith("-")
                    ? "d-del"
                    : "";
          return (
            <span key={i} className={`d-line ${cls}`}>
              {line || " "}
              {"\n"}
            </span>
          );
        })}
      </pre>
      {truncated && <p className="diff-trunc">差异过长，已截断显示。</p>}
    </div>
  );
}

export function ArchiveCard({ detail }: { detail: WorkDetail }) {
  const { act } = useStudio();
  const [memory, setMemory] = useState(detail.memory_draft ?? "");
  const [remove, setRemove] = useState(false);
  const [busy, setBusy] = useState<"memory" | "plain" | null>(null);
  useEffect(() => setMemory(detail.memory_draft ?? ""), [detail.id]); // eslint-disable-line react-hooks/exhaustive-deps

  const over = memory.length > 1000;
  const archive = async (withMemory: boolean) => {
    setBusy(withMemory ? "memory" : "plain");
    await act(
      () => api.archive({ project: detail.project, work: detail.id, memory: withMemory ? memory.trim() : "", removeWorktree: remove }),
      withMemory ? "已归档并写入项目记忆" : "已归档",
    );
    setBusy(null);
  };

  return (
    <section className="card" data-testid="archive">
      <div className="card-head">
        <span className="card-title">归档</span>
      </div>
      <p className="card-text">把这次交付的要点写入项目记忆，后续事项的 Agent 会读到它。</p>
      <textarea className="input memory-input" rows={7} value={memory} onChange={(e) => setMemory(e.target.value)} aria-label="项目记忆" />
      <div className="row-between">
        <label className="check">
          <input type="checkbox" checked={remove} onChange={(e) => setRemove(e.target.checked)} />
          <span>同时清理事项工作区和已合并的分支</span>
        </label>
        <span className={`count${over ? " over" : ""}`}>{memory.length}/1000</span>
      </div>
      <Button variant="secondary" className="block" icon={<BookmarkPlus size={14} />} disabled={over || !memory.trim()} busy={busy === "memory"} onClick={() => archive(true)}>
        归档并写入项目记忆
      </Button>
      <Button variant="ghost" className="block" icon={<Archive size={13} />} busy={busy === "plain"} onClick={() => archive(false)}>
        仅归档
      </Button>
    </section>
  );
}

export function ShelvedCard({ detail }: { detail: WorkDetail }) {
  const { act, setPage } = useStudio();
  const shelved = detail.shelved!;
  const merged = detail.deliveries[0]?.merged ?? null;
  return (
    <section className="card card-decision" data-testid="shelved">
      <div className="card-head">
        <span className="card-title">已归档</span>
      </div>
      <div className="decision-state">
        <Archive size={15} className="muted" />
        <div>
          <p className="decision-title">
            已归档
            {shelved.memory !== null && (
              <>
                {" · "}
                <button type="button" className="link-btn inline" onClick={() => setPage("memory")}>
                  记忆 #{shelved.memory}
                </button>
              </>
            )}
          </p>
          <p className="decision-text">{clock(shelved.at)} 归档</p>
          {merged && (
            <p className="decision-text">
              已合并到 <code>{merged.target}</code> · <code>{merged.sha.slice(0, 8)}</code>
            </p>
          )}
        </div>
      </div>
      <Cleanup detail={detail} />
      <Button variant="secondary" className="block" icon={<ArchiveRestore size={14} />} onClick={() => act(() => api.unarchive({ project: detail.project, work: detail.id }), "已取消归档")}>
        取消归档
      </Button>
    </section>
  );
}

/** Leftover worktrees and branches of an archived item, and the button that removes them. */
function Cleanup({ detail }: { detail: WorkDetail }) {
  const { act, notify } = useStudio();
  const [busy, setBusy] = useState(false);
  const { worktrees, branches } = detail;
  if (worktrees.length === 0 && branches.length === 0) return null;
  const unmerged = branches.filter((b) => !b.merged);
  const clean = async (force: boolean) => {
    if (force && !window.confirm(`删除未合并的分支 ${unmerged.map((b) => b.name).join("、")}？\n分支上的提交仍可从阶段引用 refs/aep/stages/* 找回。`)) return;
    setBusy(true);
    const r = await act(() => api.cleanup({ project: detail.project, work: detail.id, force }));
    if (r) {
      const parts = [];
      if (r.worktrees) parts.push(`移除 ${r.worktrees} 个工作区`);
      if (r.deleted.length) parts.push(`删除分支 ${r.deleted.join("、")}`);
      if (r.kept.length) parts.push(`保留未合并分支 ${r.kept.join("、")}`);
      notify(parts.join("，") || "没有需要清理的内容", "info");
    }
    setBusy(false);
  };
  return (
    <div className="cleanup" data-testid="cleanup">
      <ul className="cleanup-list">
        {worktrees.length > 0 && <li>事项工作区 {worktrees.length} 个</li>}
        {branches.map((b) => (
          <li key={b.name}>
            <GitBranch size={12} /> <code>{b.name}</code> {b.merged ? <span className="muted">已合并</span> : <span className="warn-text">未合并</span>}
          </li>
        ))}
      </ul>
      {worktrees.length > 0 || branches.some((b) => b.merged) ? (
        <Button variant="secondary" className="block" icon={<Trash2 size={13} />} busy={busy} onClick={() => clean(false)}>
          清理工作区和已合并分支
        </Button>
      ) : (
        <Button variant="danger" className="block" icon={<Trash2 size={13} />} busy={busy} onClick={() => clean(true)}>
          删除未合并的分支
        </Button>
      )}
    </div>
  );
}
