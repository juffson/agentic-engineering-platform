import type { ReactNode } from "react";
import type { FolderResolved } from "../api/types";
import { Spinner } from "./ui";

/** The checkbox list of repositories found under a workspace folder (interaction §2.2–§2.4):
 *  header with select-all, one row per repository, and the depth footnote with 「再查一层」.
 *  Selection state lives in the parent so a deeper scan can keep the user's deselections. */
export function FolderScan({
  resolved,
  selected,
  onChange,
  added,
  onDeeper,
  deepening,
  hint,
}: {
  resolved: Extract<FolderResolved, { kind: "folder" }>;
  selected: Set<string>;
  onChange(next: Set<string>): void;
  /** Repositories already in the project (only the repositories page passes this). */
  added?: Set<string>;
  onDeeper(): void;
  deepening: boolean;
  /** Extra line under the header, e.g. the workspace-root note of the new-project dialog. */
  hint?: ReactNode;
}) {
  const choosable = resolved.repositories.filter((r) => !added?.has(r.path));
  const chosen = choosable.filter((r) => selected.has(r.path));
  const name = resolved.root.split("/").filter(Boolean).pop() ?? resolved.root;
  const deepest = resolved.depth >= 4;
  const all = chosen.length === choosable.length;
  const toggle = (path: string, on: boolean) => {
    const next = new Set(selected);
    if (on) next.add(path);
    else next.delete(path);
    onChange(next);
  };
  return (
    <section className="folder-scan" data-testid="folder-scan">
      <div className="folder-scan-head">
        <span className="folder-scan-title">
          在 <b>{name}</b> 下找到 {resolved.repositories.length} 个 git 仓库
        </span>
        <span className="folder-scan-count" data-testid="folder-scan-count">
          已选 {chosen.length}/{choosable.length}
        </span>
        <span className="folder-scan-actions">
          <button type="button" className="link-btn" disabled={all} onClick={() => onChange(new Set([...selected, ...choosable.map((r) => r.path)]))}>
            全选
          </button>
          <button
            type="button"
            className="link-btn"
            disabled={chosen.length === 0}
            onClick={() => onChange(new Set([...selected].filter((p) => !choosable.some((r) => r.path === p))))}
          >
            全不选
          </button>
        </span>
      </div>
      {hint}
      <ul className="folder-scan-list">
        {resolved.repositories.map((repo) => {
          const isAdded = added?.has(repo.path);
          return (
            <li key={repo.path} className={isAdded ? "added" : undefined} data-testid="folder-scan-row">
              <label className="check" title={repo.path}>
                <input
                  type="checkbox"
                  disabled={isAdded}
                  checked={isAdded || selected.has(repo.path)}
                  onChange={(e) => toggle(repo.path, e.target.checked)}
                  aria-label={repo.name}
                />
                <b>{repo.name}</b>
                {repo.relative !== repo.name && <code className="folder-scan-path">{repo.relative}</code>}
                {isAdded && <span className="tag">已添加</span>}
              </label>
            </li>
          );
        })}
      </ul>
      <p className={`folder-scan-foot field-hint${resolved.truncated ? " warn" : ""}`}>
        {resolved.truncated && <>文件夹太多，只列出了前面找到的仓库，结果可能不完整。{" "}</>}
        {deepest ? <>已查找 {resolved.depth} 层子目录</> : deepening ? (<><Spinner size={11} /> 正在查找…</>) : (
          <>
            只查找了 {resolved.depth} 层子目录 ·{" "}
            <button type="button" className="link-btn" onClick={onDeeper}>
              再查一层
            </button>
          </>
        )}
      </p>
    </section>
  );
}

/** The fixed texts of interaction §2.1 for the non-folder results, so mock and UI agree. */
export function folderErrorMessage(resolved: FolderResolved): string {
  return resolved.message;
}
