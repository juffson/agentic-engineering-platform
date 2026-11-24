import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ExternalLink, FolderOpen, GitBranch, RefreshCw } from "lucide-react";
import type { BranchGraph, ProjectSummary } from "../api/types";
import { api, errorMessage } from "../api/client";
import { useStudio } from "../state/store";
import { Button, Spinner } from "./ui";

type Branch = BranchGraph["branches"][number];
type Commit = BranchGraph["commits"][number];

const ROW = 30;
const LANE = 16;
const COLORS = ["#7c83ff", "#3fb950", "#d29922", "#f778ba", "#39c5cf", "#ff7b72", "#a371f7", "#8b949e"];

interface Row {
  commit: Commit;
  lane: number;
  before: (string | null)[];
  after: (string | null)[];
  /** Lanes of merge parents that were already waiting in another lane. */
  joins: number[];
}

/** Assign each commit a lane the way graph views (git log --graph, VS Code Git Graph) do:
 *  a lane waits for the next commit on it; a commit takes the first lane waiting for it,
 *  and its first parent continues there while other parents open or join lanes. */
function layout(commits: Commit[]): { rows: Row[]; width: number } {
  const lanes: (string | null)[] = [];
  const rows: Row[] = [];
  let width = 1;
  for (const commit of commits) {
    const before = lanes.slice();
    let lane = lanes.indexOf(commit.sha);
    if (lane < 0) {
      lane = lanes.indexOf(null);
      if (lane < 0) {
        lane = lanes.length;
        lanes.push(null);
      }
    }
    for (let i = 0; i < lanes.length; i++) if (i !== lane && lanes[i] === commit.sha) lanes[i] = null;
    lanes[lane] = commit.parents[0] ?? null;
    const joins: number[] = [];
    for (const parent of commit.parents.slice(1)) {
      const at = lanes.indexOf(parent);
      if (at >= 0) {
        joins.push(at);
        continue;
      }
      const free = lanes.indexOf(null);
      if (free >= 0) lanes[free] = parent;
      else lanes.push(parent);
    }
    while (lanes.length && lanes[lanes.length - 1] === null) lanes.pop();
    width = Math.max(width, lanes.length, lane + 1);
    rows.push({ commit, lane, before, after: lanes.slice(), joins });
  }
  return { rows, width };
}

const x = (lane: number) => lane * LANE + LANE / 2 + 4;
const y = (row: number) => row * ROW + ROW / 2;
const curve = (x1: number, y1: number, x2: number, y2: number) =>
  x1 === x2 ? `M${x1} ${y1}L${x2} ${y2}` : `M${x1} ${y1}C${x1} ${y1 + ROW / 2} ${x2} ${y2 - ROW / 2} ${x2} ${y2}`;

function Lanes({ rows, width, highlight }: { rows: Row[]; width: number; highlight: string | null }) {
  const paths: JSX.Element[] = [];
  rows.forEach((row, r) => {
    const next = rows[r + 1];
    row.after.forEach((sha, j) => {
      if (!sha) return;
      const passing = row.before[j] === sha && sha !== row.commit.sha && j !== row.lane;
      const x1 = passing ? x(j) : x(row.lane);
      const x2 = next && next.commit.sha === sha ? x(next.lane) : x(j);
      const y2 = next ? y(r + 1) : y(r) + ROW / 2;
      paths.push(<path key={`${r}-${j}`} d={curve(x1, y(r), x2, y2)} stroke={COLORS[j % COLORS.length]} />);
    });
    for (const j of row.joins) {
      const sha = row.after[j];
      const x2 = next && next.commit.sha === sha ? x(next.lane) : x(j);
      paths.push(<path key={`${r}-join-${j}`} d={curve(x(row.lane), y(r), x2, next ? y(r + 1) : y(r) + ROW / 2)} stroke={COLORS[j % COLORS.length]} />);
    }
  });
  return (
    <svg className="graph-svg" width={width * LANE + 8} height={rows.length * ROW} aria-hidden>
      <g fill="none" strokeWidth={2}>{paths}</g>
      {rows.map((row, r) => (
        <circle
          key={row.commit.sha}
          cx={x(row.lane)}
          cy={y(r)}
          r={row.commit.sha === highlight ? 6 : row.commit.parents.length > 1 ? 3 : 4.5}
          fill={row.commit.parents.length > 1 ? "var(--panel)" : COLORS[row.lane % COLORS.length]}
          stroke={COLORS[row.lane % COLORS.length]}
          strokeWidth={2}
        />
      ))}
    </svg>
  );
}

function ago(seconds: number): string {
  const d = Date.now() / 1000 - seconds;
  if (d < 3600) return `${Math.max(1, Math.round(d / 60))} 分钟前`;
  if (d < 86400) return `${Math.round(d / 3600)} 小时前`;
  return `${Math.round(d / 86400)} 天前`;
}

export function BranchesPage({ project }: { project: ProjectSummary }) {
  const { act, revision } = useStudio();
  const [repo, setRepo] = useState(project.repositories[0] ?? "");
  const [graph, setGraph] = useState<BranchGraph | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [tools, setTools] = useState<{ tools: { id: string; label: string }[]; config: string } | null>(null);
  const [filter, setFilter] = useState("");
  const scroller = useRef<HTMLDivElement>(null);

  useEffect(() => setRepo(project.repositories[0] ?? ""), [project.id, project.repositories]);
  const load = useCallback(async () => {
    if (!repo) return;
    try {
      setGraph(await api.branchGraph({ project: project.id, path: repo, limit: 400 }));
      setError(null);
    } catch (e) {
      setError(errorMessage(e));
    }
  }, [project.id, repo]);
  useEffect(() => {
    setGraph(null);
    void load();
  }, [load]);
  useEffect(() => {
    void load();
  }, [revision]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    api.gitTools().then(setTools).catch(() => setTools({ tools: [], config: "" }));
  }, []);

  const { rows, width } = useMemo(() => layout(graph?.commits ?? []), [graph]);
  const tips = useMemo(() => {
    const m = new Map<string, Branch[]>();
    for (const b of graph?.branches ?? []) m.set(b.sha, [...(m.get(b.sha) ?? []), b]);
    return m;
  }, [graph]);
  const branch = graph?.branches.find((b) => b.name === selected) ?? null;
  const target = branch?.worktree?.path ?? (branch?.current || !branch ? graph?.repository : null) ?? null;
  const shown = (graph?.branches ?? []).filter((b) => !filter || b.name.includes(filter) || b.item?.name.includes(filter));

  const pick = (b: Branch) => {
    setSelected(b.name);
    const r = rows.findIndex((row) => row.commit.sha === b.sha);
    if (r >= 0) scroller.current?.scrollTo({ top: Math.max(0, r * ROW - 120), behavior: "smooth" });
  };

  return (
    <div className="page branches-page">
      <header className="branches-head">
        <div>
          <h1>分支</h1>
          <p>本地分支、它们所在的工作区（Claude / ZCode / AEP 事项），以及最近的提交图。</p>
        </div>
        <div className="row-gap">
          {project.repositories.length > 1 && (
            <select className="input select" value={repo} onChange={(e) => setRepo(e.target.value)} aria-label="仓库">
              {project.repositories.map((r) => (
                <option key={r} value={r}>
                  {r.split("/").pop()}
                </option>
              ))}
            </select>
          )}
          <Button variant="secondary" size="sm" icon={<RefreshCw size={13} />} onClick={() => void load()}>
            刷新
          </Button>
        </div>
      </header>
      {!repo && <p className="empty-text">这个项目还没有仓库。</p>}
      {error && <p className="inline-error">{error}</p>}
      {repo && !graph && !error && (
        <p className="field-hint scan-hint">
          <Spinner size={12} /> 正在读取分支和工作区…
        </p>
      )}
      {graph && (
        <div className="branches-body">
          <aside className="branch-list" aria-label="分支列表">
            <input className="input" value={filter} onChange={(e) => setFilter(e.target.value)} placeholder={`筛选 ${graph.branches.length} 个分支`} aria-label="筛选分支" />
            <ul>
              {shown.map((b) => (
                <li key={b.name}>
                  <button type="button" className={`branch-row${b.name === selected ? " on" : ""}`} onClick={() => pick(b)}>
                    <span className="branch-name">
                      <GitBranch size={12} />
                      <code>{b.name}</code>
                      {b.current && <span className="tag">当前</span>}
                    </span>
                    {b.item && <span className="branch-item">事项：{b.item.name}</span>}
                    <span className="branch-meta">
                      {!b.current && (
                        <span>
                          领先 {b.ahead} · 落后 {b.behind}
                        </span>
                      )}
                      {b.worktree && (
                        <span className={`src src-${b.worktree.source}`}>
                          {b.worktree.source} 工作区
                          {b.worktree.changed + b.worktree.untracked > 0 && ` · ${b.worktree.changed} 改动 · ${b.worktree.untracked} 未跟踪`}
                        </span>
                      )}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          </aside>
          <section className="graph-pane">
            <div className="graph-toolbar">
              <span className="muted">{branch ? <code>{branch.name}</code> : "整个仓库"}</span>
              {target ? (
                <span className="row-gap">
                  <Button variant="ghost" size="sm" icon={<FolderOpen size={13} />} onClick={() => act(() => api.reveal({ path: target }))}>
                    访达
                  </Button>
                  {tools?.tools.map((t) => (
                    <Button key={t.id} variant="secondary" size="sm" icon={<ExternalLink size={13} />} onClick={() => act(() => api.openGitTool({ id: t.id, path: target }))}>
                      {t.label}
                    </Button>
                  ))}
                </span>
              ) : (
                <span className="muted">这个分支没有检出到工作区，选中仓库或有工作区的分支后可用外部工具打开</span>
              )}
            </div>
            <div className="graph-scroll" ref={scroller}>
              <div className="graph" style={{ height: rows.length * ROW }}>
                <Lanes rows={rows} width={width} highlight={branch?.sha ?? null} />
                <ol className="graph-rows" style={{ marginLeft: width * LANE + 12 }}>
                  {rows.map((row) => (
                    <li key={row.commit.sha} className={`graph-row${branch?.sha === row.commit.sha ? " on" : ""}`} style={{ height: ROW }}>
                      {tips.get(row.commit.sha)?.map((b) => (
                        <button type="button" key={b.name} className={`ref-chip${b.current ? " current" : ""}${b.item ? " item" : ""}`} onClick={() => pick(b)} title={b.worktree?.path ?? b.name}>
                          {b.item ? b.item.name : b.name}
                        </button>
                      ))}
                      <span className="graph-subject">{row.commit.subject}</span>
                      <span className="graph-meta">
                        {row.commit.author} · {ago(row.commit.time)} · <code>{row.commit.sha.slice(0, 7)}</code>
                      </span>
                    </li>
                  ))}
                </ol>
              </div>
              {graph.truncated && <p className="field-hint graph-more">只显示最近 {graph.commits.length} 个提交。</p>}
            </div>
            {tools && (
              <p className="field-hint">
                外部工具插件：已安装的 Fork / GitHub Desktop / Sourcetree / Tower / GitKraken / VS Code / lazygit / tig / gitk 会自动出现；自定义工具写在 <code>{tools.config}</code>，格式如{" "}
                <code>{'[{"label":"Fork","command":["open","-a","Fork","{path}"]}]'}</code>。
              </p>
            )}
          </section>
        </div>
      )}
    </div>
  );
}
