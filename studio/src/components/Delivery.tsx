import { useCallback, useEffect, useState } from "react";
import { Archive, ArchiveRestore, BookmarkPlus, ChevronDown, ChevronRight, GitBranch, GitMerge, GitPullRequest, Info, Trash2 } from "lucide-react";
import type { MergePreview, PrPreview, WorkDetail } from "../api/types";
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

export function DiffView({ diff, truncated }: { diff: string; truncated: boolean }) {
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

/**
 * Delivery to the repository's remote: push the accepted commit as a branch and open a PR to the
 * target (or update an existing PR's branch). Nothing is pushed until the person confirms.
 */
export function PullRequestCard({ detail }: { detail: WorkDetail }) {
  const { act } = useStudio();
  const recorded = detail.deliveries[detail.deliveries.length - 1]?.pull_request ?? null;
  const [branch, setBranch] = useState<string | null>(null);
  const [preview, setPreview] = useState<PrPreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [title, setTitle] = useState("");
  const [body, setBody] = useState("");
  const [number, setNumber] = useState("");
  const [busy, setBusy] = useState(false);
  const [showCommits, setShowCommits] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const p = await api.prPreview({ project: detail.project, work: detail.id, branch });
      setPreview(p);
      setTitle((t) => t || p.title);
      setBody((b) => b || p.body);
      if (branch === null) setBranch(p.branch);
      setError(null);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setLoading(false);
    }
    // Only an explicit refresh re-reads the remote (branch edits wait for 「重新检查」).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [detail.project, detail.id]);

  useEffect(() => {
    void load();
  }, [load, recorded?.pushed_sha]);

  const recheck = async () => {
    setLoading(true);
    try {
      const p = await api.prPreview({ project: detail.project, work: detail.id, branch });
      setPreview(p);
      setError(null);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setLoading(false);
    }
  };

  const stale = preview !== null && branch !== null && branch !== preview.branch;
  const update = !!preview?.remote_sha;
  const blocked = !preview || stale || preview.conflicts.length > 0 || !title.trim() || title.includes("\n");

  const submit = async () => {
    if (!preview || blocked) return;
    setBusy(true);
    const n = number.trim() ? Number(number.trim()) : (recorded?.branch === preview.branch ? recorded.number ?? null : null);
    const pr = await act(
      () =>
        api.prSubmit({
          project: detail.project,
          work: detail.id,
          branch: preview.branch,
          title: title.trim(),
          body,
          expect: preview.remote_sha,
          open: !update,
          number: Number.isFinite(n) ? n : null,
        }),
      update ? `已推送到 ${preview.upstream.remote}/${preview.branch}` : `已推送并提交 PR 到 ${preview.upstream.target}`,
    );
    setBusy(false);
    if (pr) void recheck();
  };

  return (
    <section className="card card-decision" data-testid="pull-request">
      <div className="card-head">
        <span className="card-title">交付 · 推送并提 PR</span>
        {loading && <Spinner size={12} />}
      </div>
      {recorded && (
        <div className="merged-line" data-testid="pr-recorded">
          <GitPullRequest size={13} />
          <span>
            {recorded.number ? `PR #${recorded.number}` : recorded.mode === "manual" ? "已推送分支，PR 待在网页上创建" : "已推送"}
            {" · "}
            <code>{recorded.branch}</code> → <code>{recorded.target}</code>
            <span className="muted"> · {relative(recorded.at)}</span>
            {recorded.url && (
              <>
                <br />
                <button type="button" className="link-btn pr-link" onClick={() => act(() => api.openUrl({ url: recorded.url! }))}>
                  {recorded.url}
                </button>
              </>
            )}
          </span>
        </div>
      )}
      {error && <p className="inline-error">{error}</p>}
      {preview && (
        <>
          <div className="merge-route">
            <code className="branch-name" title={preview.candidate}>
              {preview.candidate.slice(0, 8)}
            </code>
            <span className="muted">→</span>
            <code>
              {preview.upstream.remote}/{preview.upstream.target}
            </code>
          </div>
          {preview.conflicts.length > 0 ? (
            <div className="notice-box notice-danger" role="alert">
              <Info size={14} />
              <span>
                和 {preview.upstream.remote}/{preview.upstream.target} 冲突：{preview.conflicts.join("、")}。请退回开发合入并解决冲突，重新验收后再提交。
              </span>
            </div>
          ) : (
            preview.behind > 0 && (
              <div className="notice-box">
                <Info size={14} />
                <span>
                  目标分支在验收之后前进了 {preview.behind} 个提交，没有冲突，可以直接提 PR。
                </span>
              </div>
            )
          )}
          <label className="field">
            <span className="field-label">推送到分支</span>
            <div className="field-inline">
              <input className="input input-sm" aria-label="推送分支" value={branch ?? ""} onChange={(e) => setBranch(e.target.value.trim())} />
              <Button variant="secondary" size="sm" onClick={recheck} disabled={loading}>
                重新检查
              </Button>
            </div>
            <span className="field-hint">
              {stale
                ? "分支名改了，先「重新检查」远端。"
                : update
                  ? `远端已有这个分支（${preview.remote_sha?.slice(0, 8)}），将更新它，已有的 PR 会跟着更新。`
                  : "远端还没有这个分支，将新建并提 PR。"}
            </span>
          </label>
          {update ? (
            <label className="field">
              <span className="field-label">PR 编号（可选，用于记录）</span>
              <input className="input input-sm" aria-label="PR 编号" inputMode="numeric" value={number} placeholder={recorded?.number ? String(recorded.number) : "如 1926"} onChange={(e) => setNumber(e.target.value.replace(/\D/g, ""))} />
            </label>
          ) : (
            <>
              <label className="field">
                <span className="field-label">PR 标题</span>
                <input className="input input-sm" aria-label="PR 标题" value={title} onChange={(e) => setTitle(e.target.value)} />
              </label>
              <label className="field">
                <span className="field-label">PR 描述</span>
                <textarea className="input" aria-label="PR 描述" rows={5} value={body} onChange={(e) => setBody(e.target.value)} />
              </label>
            </>
          )}
          <button type="button" className="link-btn" onClick={() => setShowCommits((s) => !s)} aria-expanded={showCommits}>
            {showCommits ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
            {preview.commits.length} 个提交
          </button>
          {showCommits && <pre className="stat-box">{preview.commits.join("\n")}</pre>}
          <Button variant="primary" size="lg" className="block" icon={<GitPullRequest size={14} />} busy={busy} disabled={blocked} onClick={submit}>
            {update ? `推送更新到 ${preview.branch}` : `推送并提 PR 到 ${preview.upstream.target}`}
          </Button>
        </>
      )}
    </section>
  );
}
