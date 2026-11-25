import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ChevronDown, ChevronRight, ExternalLink, FolderOpen, GitBranch, Github, Gitlab, Globe, RefreshCw } from "lucide-react";
import type { BranchGraph, ForgePluginId, PluginId, ProjectSummary, Pull, RemoteBranchComparison, RemoteStatus } from "../api/types";
import { api, errorMessage } from "../api/client";
import { relative } from "../lib/format";
import { errorText, fetchErrorLine, forgeName, pullLabel, pullMatches, pullNumber, pullStateText, remoteAddress, upstreamText } from "../lib/remote";
import { usePoll, useStudio } from "../state/store";
import { Button, Spinner } from "./ui";

type Branch = BranchGraph["branches"][number];
type Commit = BranchGraph["commits"][number];
type ActiveRemote = Extract<RemoteStatus, { active: true }>;
type RemoteBranch = ActiveRemote["remote_only"][number];
type GitTools = Awaited<ReturnType<typeof api.gitTools>>;
type RemoteRequestError = { action: "read" | "refresh"; message: string };

const expandedRemoteRepos = new Set<string>();

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
  return <ProjectBranches key={project.id} project={project} />;
}

function ProjectBranches({ project }: { project: ProjectSummary }) {
  const [chosenRepo, setChosenRepo] = useState("");
  const repo = project.repositories.includes(chosenRepo) ? chosenRepo : project.repositories[0] ?? "";
  const [localRevision, setLocalRevision] = useState(0);
  const [filter, setFilter] = useState("");
  const [tools, setTools] = useState<GitTools | null>(null);
  useEffect(() => {
    api.gitTools().then(setTools).catch(() => setTools({ tools: [], config: "" }));
  }, []);

  return (
    <div className="page branches-page">
      <header className="branches-head">
        <div>
          <h1>分支</h1>
          <p>本地分支、它们所在的工作区（Claude / ZCode / AEP 事项），以及最近的提交图。</p>
        </div>
        <div className="row-gap">
          {project.repositories.length > 1 && (
            <select className="input select" value={repo} onChange={(e) => setChosenRepo(e.target.value)} aria-label="仓库">
              {project.repositories.map((r) => (
                <option key={r} value={r}>
                  {r.split("/").pop()}
                </option>
              ))}
            </select>
          )}
          <Button variant="secondary" size="sm" icon={<RefreshCw size={13} />} onClick={() => setLocalRevision((n) => n + 1)}>
            刷新
          </Button>
        </div>
      </header>
      {repo
        ? <RepositoryBranches key={repo} project={project.id} repo={repo} tools={tools} filter={filter} setFilter={setFilter} localRevision={localRevision} />
        : <p className="empty-text">这个项目还没有仓库。</p>}
    </div>
  );
}

function RepositoryBranches({ project, repo, tools, filter, setFilter, localRevision }: {
  project: string;
  repo: string;
  tools: GitTools | null;
  filter: string;
  setFilter(value: string): void;
  localRevision: number;
}) {
  const { act, revision, remoteOn, openPlugin, reloadPlugins, notify } = useStudio();
  const [graph, setGraph] = useState<BranchGraph | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<{ kind: "local" | "remote"; name: string } | null>(null);
  const scroller = useRef<HTMLDivElement>(null);
  const graphRequest = useRef(0);
  const load = useCallback(async () => {
    const request = ++graphRequest.current;
    try {
      const result = await api.branchGraph({ project, path: repo, limit: 400 });
      if (request !== graphRequest.current) return;
      setGraph(result);
      setError(null);
    } catch (e) {
      if (request === graphRequest.current) setError(errorMessage(e));
    }
  }, [project, repo]);
  useEffect(() => {
    void load();
    return () => { ++graphRequest.current; };
  }, [load, revision, localRevision]);

  const remote = useRepositoryRemote(project, repo, remoteOn, localRevision);
  const wasRefreshing = useRef(false);
  useEffect(() => {
    const refreshing = remote.status?.active && remote.status.refreshing;
    if (wasRefreshing.current && remote.status?.active && !refreshing) {
      void load();
      void reloadPlugins();
    }
    wasRefreshing.current = !!refreshing;
  }, [load, reloadPlugins, remote.status]);

  const { rows, width } = useMemo(() => layout(graph?.commits ?? []), [graph]);
  const tips = useMemo(() => {
    const map = new Map<string, Branch[]>();
    for (const b of graph?.branches ?? []) map.set(b.sha, [...(map.get(b.sha) ?? []), b]);
    return map;
  }, [graph]);
  const branch = selected?.kind === "local" ? graph?.branches.find((b) => b.name === selected.name) : undefined;
  const status = remote.status?.active && remote.status.remote ? remote.status : null;
  const plugin = status?.remote?.plugin ?? null;
  const comparisons = useMemo(() => new Map(status?.branches.map((b) => [b.name, b])), [status]);
  const pulls = useMemo(() => new Map(Object.entries(status?.pulls ?? {})), [status]);
  const remoteBranches = status?.remote_only ?? [];
  const selectedRemote = selected?.kind === "remote" ? remoteBranches.find((b) => b.name === selected.name) : undefined;
  const selectedBranch = selectedRemote ?? branch;
  const target = selectedRemote ? null : branch?.worktree?.path ?? (branch?.current || !branch ? graph?.repository : null) ?? null;
  const shown = (graph?.branches ?? []).filter((b) => !filter || b.name.includes(filter) || b.item?.name.includes(filter) || pullMatches(pulls.get(b.name), filter));

  function remotePullName(branch: RemoteBranch) {
    // Prefer the longest match for Git remote names containing slashes.
    const owner = status?.remotes.reduce((match, entry) => (
      entry.name.length > match.length && branch.name.startsWith(`${entry.name}/`) ? entry.name : match
    ), "");
    return owner && owner === status?.remote?.name ? branch.name.slice(owner.length + 1) : null;
  }
  const shownRemote = remoteBranches.filter((b) => {
    const name = remotePullName(b);
    return !filter || b.name.includes(filter) || (name !== null && pullMatches(pulls.get(name), filter));
  });
  const detailName = selectedRemote ? remotePullName(selectedRemote) : branch?.name ?? null;

  function pick(b: Branch | RemoteBranch, kind: "local" | "remote" = "local") {
    setSelected({ kind, name: b.name });
    const r = rows.findIndex((row) => row.commit.sha === b.sha);
    if (r >= 0) scroller.current?.scrollTo({ top: Math.max(0, r * ROW - 120), behavior: "smooth" });
  }

  function openPull(pull: Pull) {
    void api.openUrl({ url: pull.url }).catch((e) => notify(errorMessage(e)));
  }

  return (
    <>
      {remoteOn && remote.status?.active !== false && (
        <RemoteBar status={remote.status} error={remote.error} busy={remote.busy} onRead={remote.read} onRefresh={remote.refresh} onConfigure={openPlugin} />
      )}
      {error && <p className="inline-error">{error}</p>}
      {!graph && !error && (
        <p className="field-hint scan-hint">
          <Spinner size={12} /> 正在读取分支和工作区…
        </p>
      )}
      {graph && (
        <div className="branches-body">
          <aside className="branch-list" aria-label="分支列表">
            <input className="input" value={filter} onChange={(e) => setFilter(e.target.value)} placeholder={`筛选 ${graph.branches.length} 个分支`} aria-label="筛选分支" />
            <ul>
              {shown.map((b) => {
                const comparison = comparisons.get(b.name);
                const pull = pulls.get(b.name);
                const worktree = b.worktree && (
                  <span className={`src src-${b.worktree.source}`}>
                    {b.worktree.source} 工作区
                    {b.worktree.changed + b.worktree.untracked > 0 && ` · ${b.worktree.changed} 改动 · ${b.worktree.untracked} 未跟踪`}
                  </span>
                );
                return (
                  <li key={b.name} className={pull ? "branch-with-pull" : undefined}>
                    <button type="button" className={`branch-row${branch?.name === b.name ? " on" : ""}`} onClick={() => pick(b)}>
                      <span className="branch-name">
                        <GitBranch size={12} />
                        <code title={b.name}>{b.name}</code>
                        {b.current && <span className="tag">当前</span>}
                      </span>
                      {b.item && <span className="branch-item">事项：{b.item.name}</span>}
                      <span className="branch-meta">
                        {!b.current && <span>{status ? "相对 HEAD " : ""}领先 {b.ahead} · 落后 {b.behind}</span>}
                        {comparison && <span>{!b.current && "· "}{upstreamText(comparison)}</span>}
                        {!status && worktree}
                      </span>
                      {status && worktree && <span className="branch-meta">{worktree}</span>}
                    </button>
                    {pull && <PullBadge pull={pull} plugin={plugin} onOpen={openPull} />}
                  </li>
                );
              })}
              {shownRemote.length > 0 && (
                <RemoteOnly repo={repo} branches={shownRemote} selected={selectedRemote?.name} plugin={plugin}
                  getPull={(b) => {
                    const name = remotePullName(b);
                    return name === null ? undefined : pulls.get(name);
                  }}
                  onPick={(b) => pick(b, "remote")} onOpen={openPull}
                />
              )}
            </ul>
          </aside>
          <section className="graph-pane">
            <div className="graph-toolbar">
              <span className="muted">{selectedBranch ? <code>{selectedBranch.name}</code> : "整个仓库"}</span>
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
              ) : selectedRemote ? (
                !rows.some((row) => row.commit.sha === selectedRemote.sha) &&
                  <span className="muted">这个远端分支的提交不在本地提交图中</span>
              ) : (
                <span className="muted">这个分支没有检出到工作区，选中仓库或有工作区的分支后可用外部工具打开</span>
              )}
            </div>
            {status && selectedBranch && (
              <RemoteDetail
                comparison={branch ? comparisons.get(branch.name) : undefined}
                pull={detailName === null ? undefined : pulls.get(detailName)}
                checked={status.pulls_checked && detailName !== null}
                plugin={plugin}
                onOpen={openPull}
              />
            )}
            <div className="graph-scroll" ref={scroller}>
              <div className="graph" style={{ height: rows.length * ROW }}>
                <Lanes rows={rows} width={width} highlight={selectedBranch?.sha ?? null} />
                <ol className="graph-rows" style={{ marginLeft: width * LANE + 12 }}>
                  {rows.map((row) => (
                    <li key={row.commit.sha} className={`graph-row${selectedBranch?.sha === row.commit.sha ? " on" : ""}`} style={{ height: ROW }}>
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
                外部工具：已检测到 {tools.tools.length} 个。查看支持的工具和自定义方法：
                <button type="button" className="branch-link" onClick={() => openPlugin("branches")}>插件 › 分支管理</button>
              </p>
            )}
          </section>
        </div>
      )}
    </>
  );
}

function useRepositoryRemote(project: string, path: string, enabled: boolean, localRevision: number) {
  const [status, setStatus] = useState<RemoteStatus | null>(null);
  const [error, setError] = useState<RemoteRequestError | null>(null);
  const [requesting, setRequesting] = useState(false);
  const version = useRef(0);
  const invokingRefresh = useRef(false);
  const refreshing = status?.active && status.refreshing;

  const read = useCallback(async () => {
    if (!enabled || invokingRefresh.current) return;
    const request = ++version.current;
    try {
      const result = await api.remoteStatus({ project, path });
      if (request !== version.current) return;
      setStatus(result);
      setError(null);
    } catch (e) {
      if (request === version.current) setError({ action: "read", message: `读取远端信息失败：${errorMessage(e)}` });
    }
  }, [enabled, project, path]);

  useEffect(() => () => {
    ++version.current;
    invokingRefresh.current = false;
  }, [enabled]);
  useEffect(() => {
    if (enabled) {
      void read();
    } else {
      setStatus(null);
      setError(null);
      setRequesting(false);
    }
  }, [enabled, read, localRevision]);
  usePoll(() => {
    if (refreshing) return read();
  }, 1500, [refreshing, read]);

  async function refresh() {
    if (!enabled || !status?.active || !status.remote || refreshing || invokingRefresh.current) return;
    invokingRefresh.current = true;
    const request = ++version.current;
    setRequesting(true);
    setError(null);
    try {
      const result = await api.remoteRefresh({ project, path });
      if (request === version.current) setStatus(result);
    } catch (e) {
      if (request === version.current) setError({ action: "refresh", message: `刷新远端失败：${errorMessage(e)}` });
    } finally {
      if (request === version.current) {
        invokingRefresh.current = false;
        setRequesting(false);
      }
    }
  }

  return { status: enabled ? status : null, error, busy: requesting || !!refreshing, read, refresh };
}

function RemoteBar({ status, error, busy, onRead, onRefresh, onConfigure }: {
  status: ActiveRemote | null;
  error: RemoteRequestError | null;
  busy: boolean;
  onRead(): Promise<void>;
  onRefresh(): Promise<void>;
  onConfigure(id: PluginId | null): void;
}) {
  const [now, setNow] = useState(Date.now);
  const apiError = busy ? null : status?.api_error;
  const resetAt = apiError?.reset_at ?? null;
  useEffect(() => {
    const remaining = resetAt === null ? 0 : resetAt * 1000 - Date.now();
    const timer = window.setTimeout(() => setNow(Date.now()), remaining > 0 ? Math.min(remaining, 60_000) : 60_000);
    return () => window.clearTimeout(timer);
  }, [now, resetAt]);

  const requestError = error && (
    <div className="branch-remote-error" role="status">
      <span className="inline-error">{error.message}</span>
      <Button size="sm" onClick={() => void (error.action === "read" ? onRead() : onRefresh())}>重试</Button>
    </div>
  );
  if (!status) return requestError ?? <p className="field-hint scan-hint" role="status"><Spinner size={12} /> 正在读取远端信息…</p>;
  const { remote } = status;
  if (!remote) return requestError ?? <p className="branch-remote-bar muted">此仓库没有配置远端</p>;

  const { plugin } = remote;
  const Icon = plugin === "github" ? Github : plugin === "gitlab" ? Gitlab : Globe;
  const noun = plugin === "gitlab" ? "MR" : "PR";
  const otherRemotes = status.remotes.filter((entry) => entry.name !== remote.name);
  const updated = status.refreshed_at === null ? null : new Date(status.refreshed_at * 1000);
  const requiresConfig = apiError && ["auth", "forbidden", "not_found"].includes(apiError.kind);
  const limited = apiError?.kind === "rate_limited" && (resetAt === null || resetAt * 1000 > Math.max(now, Date.now()));
  const retryTitle = limited && resetAt !== null ? `恢复时间：${new Date(resetAt * 1000).toLocaleString()}` : undefined;
  const fetchError = busy ? null : status.fetch_error;

  return (
    <div className="branch-remote">
      <div className="branch-remote-bar">
        <div className="branch-remote-summary">
          <Icon size={15} aria-label={plugin ? forgeName[plugin] : undefined} aria-hidden={plugin === null} />
          <code>{remoteAddress(remote)}</code>
          {otherRemotes.length > 0 && (
            <span className="muted" title={otherRemotes.map(remoteAddress).join("\n")}>
              另有 {otherRemotes.length} 个远端
            </span>
          )}
          {!plugin && (
            <span className="muted">
              未配置与 {remote.host ?? remote.name} 对应的插件
              <button type="button" className="branch-link" onClick={() => onConfigure(null)}>打开插件</button>
            </span>
          )}
          {plugin && remote.badge === "needs_config" && (
            <span className="muted">
              {forgeName[plugin]} 未填写访问令牌，{noun} 信息不可用
              <button type="button" className="branch-link" onClick={() => onConfigure(plugin)}>去配置</button>
            </span>
          )}
          {updated
            ? <time className="muted" dateTime={updated.toISOString()} title={updated.toLocaleString()}>上次刷新 {relative(updated.toISOString())}</time>
            : <span className="muted">尚未刷新远端，领先落后基于本地已有的远端引用</span>}
          {busy && <Spinner size={12} />}
        </div>
        <Button variant="secondary" size="sm" disabled={busy} onClick={() => void onRefresh()}>
          {busy ? "正在刷新…" : "刷新远端"}
        </Button>
      </div>
      {requestError}
      {fetchError && (
        <div className="branch-remote-error" role="status">
          <span className="inline-error">
            git fetch 失败：{fetchErrorLine(fetchError.message)}
            {fetchError.auth && "。Studio 用系统 git 凭据拉取，不会用插件令牌改写 git 配置"}
          </span>
          <Button size="sm" onClick={() => void onRefresh()}>重试</Button>
        </div>
      )}
      {apiError && (
        <div className="branch-remote-error" role="status">
          <span className="inline-error">{errorText(apiError, plugin, remote.path ?? undefined)}</span>
          {requiresConfig && plugin ? <Button size="sm" onClick={() => onConfigure(plugin)}>去配置</Button>
            : apiError.kind !== "cert" && (
              <span title={retryTitle}>
                <Button size="sm" disabled={limited} onClick={() => void onRefresh()}>重试</Button>
              </span>
            )}
        </div>
      )}
    </div>
  );
}

function PullBadge({ pull, plugin, onOpen }: { pull: Pull; plugin: ForgePluginId | null; onOpen(pull: Pull): void }) {
  const label = pullLabel(pull, plugin);
  const others = `另有 ${pull.others} 个 ${plugin === "gitlab" ? "MR" : "PR"}`;
  return (
    <button type="button" className={`branch-pull branch-pull-${pull.state}`} title={pull.title} onClick={() => onOpen(pull)}>
      {label}
      {pull.others > 0 && <span title={others} aria-label={others}> +{pull.others}</span>}
    </button>
  );
}

function RemoteDetail({ comparison, pull, checked, plugin, onOpen }: {
  comparison?: RemoteBranchComparison;
  pull?: Pull;
  checked: boolean;
  plugin: ForgePluginId | null;
  onOpen(pull: Pull): void;
}) {
  if (!comparison && !pull && !checked) return null;
  return (
    <div className="graph-remote">
      {comparison && <span className="muted">{upstreamText(comparison, true)}</span>}
      {pull ? (
        <div className="graph-remote-pull">
          <span className="graph-remote-title" title={pull.title}>{pullNumber(pull, plugin)} {pull.title}</span>
          <span className="muted">{pullStateText[pull.state]}{pull.others > 0 && ` · 另有 ${pull.others} 个`}</span>
          <Button variant="ghost" size="sm" icon={<ExternalLink size={13} />} onClick={() => onOpen(pull)}>在浏览器打开</Button>
        </div>
      ) : checked && <span className="muted">没有对应的 {plugin === "gitlab" ? "MR" : "PR"}</span>}
    </div>
  );
}

function RemoteOnly({ repo, branches, selected, plugin, getPull, onPick, onOpen }: {
  repo: string;
  branches: RemoteBranch[];
  selected?: string;
  plugin: ForgePluginId | null;
  getPull(branch: RemoteBranch): Pull | undefined;
  onPick(branch: RemoteBranch): void;
  onOpen(pull: Pull): void;
}) {
  const [expanded, setExpanded] = useState(() => expandedRemoteRepos.has(repo));
  function toggle() {
    if (expanded) expandedRemoteRepos.delete(repo);
    else expandedRemoteRepos.add(repo);
    setExpanded(!expanded);
  }
  return (
    <li className="branch-remote-only">
      <Button variant="ghost" size="sm" icon={expanded ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
        aria-expanded={expanded} onClick={toggle}>
        仅远端 {branches.length}
      </Button>
      {expanded && (
        <ul aria-label="仅远端分支">
          {branches.map((branch) => {
            const pull = getPull(branch);
            return (
              <li key={branch.name} className={pull ? "branch-with-pull" : undefined}>
                <button type="button" className={`branch-row${selected === branch.name ? " on" : ""}`} onClick={() => onPick(branch)}>
                  <span className="branch-name">
                    <GitBranch size={12} className="muted" />
                    <code title={branch.name}>{branch.name}</code>
                  </span>
                  <span className="muted">远端分支，本地没有</span>
                </button>
                {pull && <PullBadge pull={pull} plugin={plugin} onOpen={onOpen} />}
              </li>
            );
          })}
        </ul>
      )}
    </li>
  );
}
