// In-memory implementation of the Studio contract for browser development and
// screenshot tests. Agent runs are simulated on a clock: each running attempt
// reveals one more transcript entry every `stepMs`, then finishes with its
// scripted outcome. Mutations update this state so every flow is clickable.
import type {
  DraftStatus,
  TidyStatus,
  BranchGraph,
  SyncPlan,
  ImportFound,
  Api,
  AttemptSummary,
  AttemptView,
  Client,
  Entry,
  MemoryNote,
  MergePreview,
  Profile,
  ProjectSummary,
  Role,
  Sandbox,
  Snapshot,
  StageDetail,
  Template,
  WorkDetail,
  WorkHistory,
  HistoryNote,
  HistoryRound,
  ItemHandoff,
  ZCodeModel,
  Tokens,
  UsageAnalysis,
  ProjectUsage,
  CompareRow,
  CompareSide,
  Launch,
  SessionLabel,
  SessionPreview,
  ForgePluginId,
  PluginId,
  PluginInfo,
  Pull,
  RemoteError,
  RemoteErrorKind,
  RemoteStatus,
  TestResult,
} from "./types";
import { parseTokens } from "../lib/format";
import { defaultHost, normalizeHost, pluginBadge } from "../lib/remote";

type StageDef = { name: string; role: Role; brief: string; skippable: boolean };

const TEMPLATES: Record<Template, { label: string; stages: StageDef[] }> = {
  Software: {
    label: "软件开发",
    stages: [
      { name: "产品需求", role: "Plan", brief: "澄清目标用户、使用场景与验收标准，产出需求说明。", skippable: false },
      { name: "交互设计", role: "Plan", brief: "给出界面结构、交互流程以及空态、错误态等边界状态。", skippable: true },
      { name: "技术方案", role: "Plan", brief: "确定实现路径、数据结构、接口变更与主要风险。", skippable: false },
      { name: "开发实现", role: "Build", brief: "按技术方案在事项工作区实现功能，并补充单元测试。", skippable: false },
      { name: "测试验证", role: "Test", brief: "运行测试与检查命令；发现问题时退回负责的阶段。", skippable: false },
      { name: "独立验收", role: "Accept", brief: "以使用者视角独立验收，逐条核对验收标准。", skippable: true },
      { name: "交付归档", role: "Deliver", brief: "整理变更说明，提交改动并准备合并。", skippable: false },
    ],
  },
  Game: {
    label: "游戏开发",
    stages: [
      { name: "玩法设计", role: "Plan", brief: "明确核心玩法循环、数值框架与目标体验。", skippable: false },
      { name: "技术方案", role: "Plan", brief: "确定引擎模块、资源管线与性能预算。", skippable: false },
      { name: "开发实现", role: "Build", brief: "实现玩法与配套系统。", skippable: false },
      { name: "试玩测试", role: "Test", brief: "自动化冒烟与试玩检查。", skippable: false },
      { name: "交付归档", role: "Deliver", brief: "整理构建与变更说明。", skippable: false },
    ],
  },
  Legacy: {
    label: "存量改造",
    stages: [
      { name: "现状调研", role: "Plan", brief: "梳理现有代码路径、依赖与已知问题。", skippable: false },
      { name: "改造方案", role: "Plan", brief: "给出渐进式改造步骤与回滚方案。", skippable: false },
      { name: "开发实现", role: "Build", brief: "按步骤改造并保持行为一致。", skippable: false },
      { name: "回归测试", role: "Test", brief: "全量回归，确认无行为变化。", skippable: false },
      { name: "交付归档", role: "Deliver", brief: "整理变更说明，提交并准备合并。", skippable: false },
    ],
  },
};

const CLIENTS: Snapshot["clients"] = [
  { id: "Codex", label: "Codex", models: ["gpt-5-codex", "gpt-5", "o4-mini"] },
  { id: "Claude", label: "Claude", models: ["claude-opus-4-1", "claude-sonnet-4-5"] },
  { id: "ZCode", label: "ZCode", models: ["glm-4.6", "glm-4.5-air"] },
  { id: "Custom", label: "自定义", models: [] },
];

const SANDBOXES: Snapshot["sandboxes"] = [
  { id: "Standard", label: "默认沙箱" },
  { id: "Network", label: "允许网络" },
  { id: "Full", label: "完全权限" },
];

const EXECUTABLES: Record<Client, string> = { Codex: "codex", Claude: "claude", ZCode: "zcode", Custom: "" };

function profile(client: Client, model = ""): Profile {
  return { client, executable: EXECUTABLES[client], model, check_command: "" };
}

interface MockAttempt {
  work: string;
  stage: number;
  summary: AttemptSummary;
  entries: Entry[];
  script: Entry[];
  startMs: number;
  stepMs: number;
  outcome: Pick<AttemptSummary, "status" | "error" | "route_back">;
  tokens: [number, number];
  result: string | null;
  /** Tokens the run may use before it is stopped (what was left of the budgets). */
  limit: number | null;
  /** Context of the first and the last model call; null first = interrupted before one (E12). */
  firstContext: number | null;
  lastContext: number;
  calls: number;
  /** Codex without a rollout: usage only when the run ends (R8). */
  noRollout: boolean;
}

const BUILTIN_PLUGINS = ["cc-plugin-sec-default", "agents-md", "telemetry", "privacy", "plugin-authoring"];
const DEFAULT_RESUME_LIMIT = 150_000;

/** The launch record a run gets: Claude reports its init, Codex only the first call; old runs none. */
function mockLaunch(client: Client, legacy: boolean, firstContext: number | null): Launch | null {
  if (legacy) return null;
  if (client === "Claude")
    return { diet: 1, minimal: true, not_minimized: [], model_source: null, version: "2.1.331", mcp_servers: 0, mcp_tools: 0, tools: 4, skills: 0, plugins: 5, builtin_plugins: BUILTIN_PLUGINS, first_context: firstContext };
  if (client === "Codex") return { diet: 1, first_context: firstContext };
  return null;
}

const median = (v: number[]) => {
  if (!v.length) return null;
  const s = [...v].sort((a, b) => a - b);
  const m = Math.floor(s.length / 2);
  return s.length % 2 ? s[m] : Math.round((s[m - 1] + s[m]) / 2);
};
const avg = (v: number[]) => (v.length ? Math.round(v.reduce((a, b) => a + b, 0) / v.length) : null);

interface MockWork {
  project: string;
  detail: WorkDetail;
  createdMs: number;
  autopilotRan: Set<number>;
  autopilotLoops: number;
  budgets?: Record<number, number>;
  budgetTotal?: number | null;
}

/** A mock run's tokens so far, split the way the real CLIs report them (mostly cache reads). */
function mockTokens(a: MockAttempt): Tokens {
  const progress = a.summary.status === "running" ? a.entries.length / Math.max(1, a.entries.length + a.script.length) : 1;
  const input = Math.round(a.tokens[0] * progress);
  return {
    input: Math.round(input * 0.15),
    cache_read: input - Math.round(input * 0.15),
    cache_write: 0,
    output: Math.round(a.tokens[1] * progress),
    reasoning: 0,
  };
}
const sumTokens = (t: Tokens) => t.input + t.cache_read + t.cache_write + t.output;
const isLiveStatus = (s: string) => s === "running" || s === "initializing";
/** A run from before the minimal launch: ~250K base context re-read by every call. */
const legacyRun = (n: number) => ({ legacy: true, tokens: [5_200_000 + ((n * 733_000) % 1_900_000), 52_000] as [number, number] });

const HOME = "/Users/demo/.aep";
const REPOS = ["/Users/demo/code/quote-terminal", "/Users/demo/code/quote-gateway"];
const FORGES = ["github", "gitlab"] as const;

const MOCK_ERRORS = new Map<string, RemoteErrorKind>([
  ["bad", "auth"], ["forbidden", "forbidden"], ["cert", "cert"], ["limit", "rate_limited"],
  ["slow", "timeout"], ["offline", "unreachable"], ["missing", "not_found"], ["other", "other"],
]);

function mockCredential(token: string) {
  return {
    hint: token.slice(-4),
    outcome: MOCK_ERRORS.get(token.replace(/-token$/, "")) ?? null,
    fetchFailure: token === "fetch-fail-token",
  };
}

function mockError(kind: RemoteErrorKind, host: string, at = Math.floor(Date.now() / 1000)): RemoteError {
  const codes: Partial<Record<RemoteErrorKind, number>> = { auth: 401, forbidden: 403, rate_limited: 429, not_found: 404, other: 500 };
  return { kind, message: "示例服务错误", host, status: codes[kind] ?? null, reset_at: kind === "rate_limited" ? at + 600 : null, at };
}

function mockHost(input: string, plugin: ForgePluginId) {
  const normalized = normalizeHost(input, plugin);
  if (!normalized.url) throw new Error(normalized.error);
  return normalized.url;
}

let seq = 1000;
const nextId = (prefix: string) => `${prefix}${(seq++).toString(36)}${Math.random().toString(36).slice(2, 6)}`;

function iso(ms: number): string {
  return new Date(ms).toISOString();
}

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

class MockStore implements Api {
  projects: { id: string; name: string; repositories: string[] }[] = [];
  works = new Map<string, MockWork>();
  order: string[] = []; // newest first
  attempts = new Map<string, MockAttempt>();
  memoryNotes = new Map<string, MemoryNote[]>();
  active: string | null = null;
  selected = new Map<string, string | null>();
  handoffInbox = new Map<string, ItemHandoff[]>();
  repoNotes = new Map<string, MemoryNote[]>();
  zcodeCalls = 0;
  picks = 0;
  private branchPluginEnabled = true;
  private forgePlugins: Record<ForgePluginId, { enabled: boolean; host: string; credential: ReturnType<typeof mockCredential> | null; error: RemoteError | null }> = {
    github: { enabled: true, host: `https://${defaultHost.github}`, credential: mockCredential("mock-a1b2"), error: null },
    gitlab: { enabled: false, host: `https://${defaultHost.gitlab}`, credential: null, error: null },
  };
  private mockRemoteAt = Math.floor(Date.now() / 1000) - 600;
  private mockRefresh = new Map<string, number>();

  constructor() {
    seed(this);
  }

  // ---------- helpers ----------

  work(project: string, work: string): MockWork {
    const w = this.works.get(work);
    if (!w || w.project !== project) throw new Error(`找不到事项 ${work}`);
    return w;
  }

  stage(w: MockWork, index: number): StageDetail {
    const s = w.detail.stages[index];
    if (!s) throw new Error(`阶段 ${index + 1} 不存在`);
    return s;
  }

  resumeLimit = new Map<string, number | null>();

  /** The mock's `session_reuse::decide` for adjacent stages (design §3.2), simplified. */
  decide(w: MockWork, stageIndex: number): SessionPreview | null {
    const stage = this.stage(w, stageIndex);
    const client = stage.profile.client;
    if (client !== "Claude" && client !== "Codex") return null;
    const prev = [...w.detail.stages.slice(0, stageIndex)].reverse().find((s) => s.current_attempt && s.state !== "skipped");
    const run = prev?.current_attempt ? this.attempts.get(prev.current_attempt) : undefined;
    if (!prev || !run) return { kind: "fresh", reason: "no_prior" };
    const src = run.summary;
    const base = { stage: prev.index, stage_name: prev.name, run: src.id };
    if (!src.launch && !src.session) return { ...base, kind: "fresh", reason: "session_unavailable" };
    if (src.client !== client) return { ...base, kind: "fresh", reason: "client_differs", prior: src.client };
    if (src.model.trim() !== stage.profile.model.trim()) return { ...base, kind: "fresh", reason: "model_differs", prior: src.model };
    if (prev.sandbox !== stage.sandbox) return { ...base, kind: "fresh", reason: "permission_differs", prior: prev.sandbox };
    if (src.status === "budget_exhausted") return { ...base, kind: "fresh", reason: "prior_budget_stopped" };
    if (src.status === "running") return { ...base, kind: "fresh", reason: "session_busy" };
    if (src.status !== "succeeded") return { ...base, kind: "fresh", reason: "prior_not_passed" };
    const threshold = this.resumeLimit.get(w.project) ?? DEFAULT_RESUME_LIMIT;
    if (run.lastContext > threshold) return { ...base, kind: "fresh", reason: "context_over_threshold", context: run.lastContext, threshold };
    return { ...base, kind: "reused", context: run.lastContext, threshold };
  }

  /** The label a new (non follow-up) run gets; no label for the first run, as the real one. */
  session(w: MockWork, stageIndex: number): SessionLabel | null {
    const d = this.decide(w, stageIndex);
    if (!d) return null;
    if (d.kind === "reused") return { kind: "reused", stage: d.stage, stage_name: d.stage_name ?? undefined, run: d.run };
    return { kind: "fresh", reason: d.reason, context: d.context ?? null, threshold: d.threshold ?? null, prior: d.prior ?? null };
  }

  addAttempt(
    w: MockWork,
    stageIndex: number,
    opts: {
      prompt: string;
      followUp?: boolean;
      script: Entry[];
      outcome?: MockAttempt["outcome"];
      startMs?: number;
      stepMs?: number;
      done?: boolean;
      result?: string | null;
      tokens?: [number, number];
      subtask?: string | null;
      /** A run from before the minimal launch: no launch record, no session label. */
      legacy?: boolean;
      firstContext?: number | null;
      lastContext?: number;
      calls?: number;
      noRollout?: boolean;
      maybeHigh?: boolean;
    },
  ): MockAttempt {
    const stage = this.stage(w, stageIndex);
    const id = nextId("a");
    const startMs = opts.startMs ?? Date.now();
    const stepMs = opts.stepMs ?? 900;
    const outcome = opts.outcome ?? { status: "succeeded", error: null, route_back: null };
    const summary: AttemptSummary = {
      id,
      status: "running",
      client: stage.profile.client,
      model: stage.profile.model,
      started_at: iso(startMs),
      finished_at: null,
      prompt: opts.prompt,
      follow_up: opts.followUp ?? false,
      subtask: opts.subtask ?? null,
      error: null,
      route_back: null,
      committed: false,
      head_sha: null,
      tokens: 0,
      session: null,
      launch: null,
      continuable: stage.profile.client === "Claude" || stage.profile.client === "Codex",
      count_maybe_high: !!opts.maybeHigh,
    };
    const legacy = !!opts.legacy;
    const firstContext = opts.firstContext !== undefined ? opts.firstContext : legacy ? 250_000 + Math.round(Math.random() * 12_000) : 8_000 + Math.round(Math.random() * 5_000);
    summary.launch = mockLaunch(stage.profile.client, legacy, firstContext);
    if (!legacy) summary.session = opts.followUp ? (stage.profile.client === "Claude" || stage.profile.client === "Codex" ? { kind: "continued", run: stage.current_attempt ?? undefined } : null) : this.session(w, stageIndex);
    const budget = this.budget(w, stageIndex);
    if (budget.exhausted) throw new Error(budget.exhausted);
    const attempt: MockAttempt = {
      work: w.detail.id,
      stage: stageIndex,
      summary,
      entries: [],
      script: opts.script,
      startMs,
      stepMs,
      outcome,
      tokens: opts.tokens ?? [18000 + Math.round(Math.random() * 30000), 2400 + Math.round(Math.random() * 4000)],
      result: opts.result ?? null,
      limit: budget.remaining,
      firstContext,
      lastContext: opts.lastContext ?? (legacy ? 290_000 : 60_000 + Math.round(Math.random() * 40_000)),
      calls: opts.calls ?? (legacy ? 38 : 21),
      noRollout: !!opts.noRollout,
    };
    if (opts.done) {
      attempt.entries = [...opts.script];
      attempt.script = [];
      this.finish(attempt, startMs + (opts.script.length + 2) * stepMs);
    } else {
      w.detail.running = id;
    }
    stage.attempts.push(summary);
    stage.current_attempt = id;
    this.attempts.set(id, attempt);
    return attempt;
  }

  finish(attempt: MockAttempt, at: number) {
    const s = attempt.summary;
    s.status = attempt.outcome.status;
    s.error = attempt.outcome.error;
    s.route_back = attempt.outcome.route_back;
    s.finished_at = iso(at);
    s.tokens = sumTokens(mockTokens(attempt));
    if (s.status === "succeeded") {
      s.committed = true;
      s.head_sha = Math.random().toString(16).slice(2, 12);
    }
    const w = this.works.get(attempt.work);
    if (w && w.detail.running === s.id) w.detail.running = null;
  }

  /** Advance simulated time: reveal transcript entries, finish runs, drive autopilot. */
  tick() {
    const now = Date.now();
    for (const a of this.attempts.values()) {
      if (a.summary.status !== "running") continue;
      const due = Math.floor((now - a.startMs) / a.stepMs);
      while (a.entries.length < due && a.script.length > 0) {
        a.entries.push(a.script.shift()!);
      }
      a.summary.tokens = sumTokens(mockTokens(a));
      if (a.limit !== null && a.summary.tokens >= a.limit) {
        a.outcome = { status: "budget_exhausted", error: `Token 预算已用完：这一轮用了 ${Math.round(a.summary.tokens / 1000)}k，本轮可用 ${Math.round(a.limit / 1000)}k。追加预算后可在原会话继续`, route_back: null };
        // Freeze the run at what it had used when it was stopped.
        const t = mockTokens(a);
        a.tokens = [t.input + t.cache_read, t.output];
        a.script = [];
        this.finish(a, now);
        const w = this.works.get(a.work);
        if (w?.detail.autopilot.running) this.stopAutopilot(w, "stopped", `「${w.detail.stages[a.stage]?.name}」的 Token 预算已用完，追加预算后可继续对话，或退回 / 人工放行`);
        continue;
      }
      if (a.script.length === 0 && now - a.startMs >= (a.entries.length + 1) * a.stepMs) {
        this.finish(a, now);
      }
    }
    for (const w of this.works.values()) this.driveAutopilot(w);
  }

  driveAutopilot(w: MockWork): void {
    const d = w.detail;
    const ap = d.autopilot;
    if (!ap.running || d.running) return;
    const until = ap.until ?? d.stages.length - 1;
    const stage = d.stages[d.stage];
    if (!stage) return this.stopAutopilot(w, "done", "全部阶段已完成");
    const latest = stage.attempts[stage.attempts.length - 1];
    const ranHere = w.autopilotRan.has(d.stage);
    if (ranHere && latest && latest.status === "succeeded") {
      if (d.stage >= until) {
        return this.stopAutopilot(w, "done", `已运行到「${stage.name}」，等待你确认`);
      }
      this.doAdvance(w);
      return this.driveAutopilot(w);
    }
    if (ranHere && latest && latest.status === "failed") {
      if (latest.route_back && w.autopilotLoops < 2) {
        w.autopilotLoops += 1;
        const to = latest.route_back.stage;
        this.doSendBack(w, d.stage, to, latest.route_back.question);
        w.autopilotRan.add(to);
        ap.stage = to;
        ap.message = `「${stage.name}」发现问题，已退回「${d.stages[to].name}」修复（第 ${w.autopilotLoops} 次）`;
        return;
      }
      return this.stopAutopilot(w, "failed", `「${stage.name}」未通过，需要人工处理`);
    }
    if (ranHere && latest && latest.status === "cancelled") {
      return this.stopAutopilot(w, "stopped", "运行已被取消");
    }
    // Run the current stage.
    w.autopilotRan.add(d.stage);
    ap.stage = d.stage;
    ap.message = `正在运行「${stage.name}」（${d.stage + 1}/${d.stages.length}）`;
    this.startRun(w, d.stage, stage.note, 1200);
  }

  stopAutopilot(w: MockWork, status: string, message: string) {
    const ap = w.detail.autopilot;
    ap.running = false;
    ap.status = status;
    ap.message = message;
  }

  startRun(w: MockWork, stageIndex: number, prompt: string, stepMs = 800): MockAttempt {
    const stage = this.stage(w, stageIndex);
    return this.addAttempt(w, stageIndex, {
      prompt,
      script: genericScript(stage, prompt),
      stepMs,
      result: `「${stage.name}」已完成。`,
    });
  }

  doAdvance(w: MockWork) {
    const d = w.detail;
    const cur = d.stages[d.stage];
    if (cur) cur.state = "done";
    let next = d.stage + 1;
    while (next < d.stages.length && d.stages[next].state === "skipped") next += 1;
    d.stage = next;
    if (next >= d.stages.length) {
      d.delivered = true;
      d.deliveries.unshift({ run: nextId("r"), archived_at: iso(Date.now()), merged: null });
      d.memory_draft = memoryDraftFor(d);
    } else {
      d.stages[next].state = "current";
    }
  }

  doSendBack(w: MockWork, from: number, to: number, reason: string): string {
    const d = w.detail;
    d.stages.forEach((s, i) => {
      if (i === to) s.state = "current";
      else if (i > to && i <= from && s.state !== "skipped") s.state = "upcoming";
    });
    d.stage = to;
    d.reworks.push({ from, to, reason, at: iso(Date.now()) });
    const target = d.stages[to];
    const prompt = `「${d.stages[from].name}」退回：${reason}`;
    const attempt = this.addAttempt(w, to, {
      prompt,
      followUp: target.attempts.length > 0,
      script: fixScript(reason),
      stepMs: 900,
    });
    return attempt.summary.id;
  }

  summary(w: MockWork) {
    const d = w.detail;
    const stage = d.stages[d.stage];
    const latest = stage?.attempts[stage.attempts.length - 1];
    const group = d.shelved ? "shelved" : d.delivered ? "delivered" : "active";
    return {
      id: d.id,
      name: d.name,
      kind: d.kind,
      group,
      stage: d.stage,
      stage_count: d.stages.length,
      stage_name: d.delivered ? "已交付" : stage?.name ?? "",
      status: latest?.status ?? "",
      running: d.running !== null,
      autopilot: d.autopilot.running,
    } as const;
  }

  // ---------- Api ----------

  snapshot(): Snapshot {
    this.tick();
    const projects: ProjectSummary[] = this.projects.map((p) => ({
      ...p,
      repositories: [...p.repositories],
      work: this.order
        .map((id) => this.works.get(id)!)
        .filter((w) => w.project === p.id)
        .map((w) => this.summary(w)),
    }));
    return {
      home: HOME,
      active: this.active,
      selected: this.active ? this.selected.get(this.active) ?? null : null,
      projects,
      clients: clone(CLIENTS),
      sandboxes: clone(SANDBOXES),
      templates: (Object.keys(TEMPLATES) as Template[]).map((id) => ({
        id,
        label: TEMPLATES[id].label,
        stages: TEMPLATES[id].stages.map((s) => s.name),
      })),
    };
  }

  openProject({ project }: { project: string }) {
    if (!this.projects.some((p) => p.id === project)) throw new Error("项目不存在");
    this.active = project;
  }

  selectWork({ project, work }: { project: string; work: string }) {
    this.work(project, work);
    this.selected.set(project, work);
  }

  createProject({ name, repositories }: { name: string; repositories: string[] }) {
    if (!name.trim()) throw new Error("项目名称不能为空");
    const id = nextId("p");
    this.projects.push({ id, name: name.trim(), repositories: repositories.filter(Boolean) });
    this.memoryNotes.set(id, []);
    this.active = id;
    return id;
  }

  addRepository({ project, path }: { project: string; path: string }) {
    const p = this.projects.find((x) => x.id === project);
    if (!p) throw new Error("项目不存在");
    const clean = path.trim();
    if (!clean.startsWith("/")) throw new Error(`仓库路径需要是绝对路径：${clean || "(空)"}`);
    if (p.repositories.includes(clean)) throw new Error("该仓库已在项目中");
    p.repositories.push(clean);
  }

  createWork(a: { project: string; name: string; template: Template; release: boolean; repositories: string[] }): string {
    if (!a.name.trim()) throw new Error("事项名称不能为空");
    if (a.repositories.length === 0) throw new Error("至少选择一个仓库");
    const w = newWork(a.project, a.name.trim(), a.template, a.release ? "Release" : "Feature", a.repositories, Date.now());
    this.works.set(w.detail.id, w);
    this.order.unshift(w.detail.id);
    this.selected.set(a.project, w.detail.id);
    return w.detail.id;
  }

  workDetail({ project, work }: { project: string; work: string }): WorkDetail {
    this.tick();
    const w = this.work(project, work);
    const d = w.detail;
    const fallback = this.projectBudget.get(project) ?? null;
    d.stages.forEach((st, i) => {
      st.budget = { limit: w.budgets?.[i] ?? fallback, own: w.budgets?.[i] ?? null, used: st.attempts.reduce((n, a) => n + (a.tokens ?? 0), 0), suggested: this.suggested(project, st.name) };
      st.session_preview = i === d.stage && !d.running ? this.preview(w, i) : null;
    });
    d.budget = {
      total_limit: w.budgetTotal ?? null,
      total_used: d.stages.reduce((n, st) => n + st.budget.used, 0),
      stage_default: fallback,
    };
    return clone(d);
  }

  projectBudget = new Map<string, number | null>();

  /** Median of passed runs after the diet × 1.5, rounded up to 10K (R9). */
  suggested(project: string, stageName: string): StageDetail["budget"]["suggested"] {
    const totals: number[] = [];
    for (const a of this.attempts.values()) {
      const w = this.works.get(a.work);
      if (!w || w.project !== project || !a.summary.launch || a.summary.status !== "succeeded") continue;
      if (w.detail.stages[a.stage]?.name !== stageName) continue;
      totals.push(a.summary.tokens);
    }
    const m = median(totals);
    if (m === null) return null;
    return { value: Math.ceil((m * 1.5) / 10_000) * 10_000, runs: totals.length, median: m };
  }

  preview(w: MockWork, stageIndex: number): SessionPreview | null {
    const d = this.decide(w, stageIndex);
    return d && !(d.kind === "fresh" && d.reason === "no_prior") ? d : null;
  }

  budget(w: MockWork, stage: number): { remaining: number | null; exhausted: string | null } {
    const used = (i: number | null) =>
      w.detail.stages.filter((_, j) => i === null || j === i).reduce((n, st) => n + st.attempts.reduce((m, a) => m + (a.tokens ?? 0), 0), 0);
    const stageLimit = w.budgets?.[stage] ?? this.projectBudget.get(w.project) ?? null;
    const totalLimit = w.budgetTotal ?? null;
    const left = [stageLimit !== null ? stageLimit - used(stage) : null, totalLimit !== null ? totalLimit - used(null) : null].filter((x): x is number => x !== null);
    const remaining = left.length ? Math.max(0, Math.min(...left)) : null;
    let exhausted: string | null = null;
    if (totalLimit !== null && used(null) >= totalLimit) exhausted = "事项 Token 总预算已用完，请追加预算";
    else if (stageLimit !== null && used(stage) >= stageLimit) exhausted = "此阶段 Token 预算已用完，请追加预算";
    return { remaining, exhausted };
  }

  setBudget({ project, work, stage, tokens }: { project: string; work: string; stage?: number; tokens: number | null }) {
    const w = this.work(project, work);
    if (stage === undefined || stage === null) {
      w.budgetTotal = tokens || null;
      return;
    }
    w.budgets = { ...(w.budgets ?? {}) };
    if (tokens) w.budgets[stage] = tokens;
    else delete w.budgets[stage];
  }

  setProjectBudget({ project, tokens }: { project: string; tokens: number | null }) {
    this.projectBudget.set(project, tokens || null);
  }

  setResumeLimit({ project, tokens }: { project: string; tokens: string }) {
    const n = parseTokens(tokens);
    if (n !== null && Number.isNaN(n)) throw new Error(`无法识别的 Token 数：${tokens}`);
    if (n !== null && n < 20_000) throw new Error("续接上限至少 20K");
    this.resumeLimit.set(project, n);
  }

  extendBudget({ project, work, stage, extra, message }: { project: string; work: string; stage: number; extra: number; message: string }): string {
    const w = this.work(project, work);
    const st = this.stage(w, stage);
    const used = st.attempts.reduce((n, a) => n + (a.tokens ?? 0), 0);
    const limit = w.budgets?.[stage] ?? this.projectBudget.get(project) ?? null;
    if (limit !== null) w.budgets = { ...(w.budgets ?? {}), [stage]: Math.max(limit, used) + extra };
    if (w.budgetTotal) {
      const total = w.detail.stages.reduce((n, s2) => n + s2.attempts.reduce((m, a) => m + (a.tokens ?? 0), 0), 0);
      if (total + extra > w.budgetTotal) w.budgetTotal = Math.max(w.budgetTotal, total) + extra;
    }
    // A CLI that cannot continue the original task re-runs the stage (ZCode desktop, R7).
    const last = st.current_attempt ? this.attempts.get(st.current_attempt) : undefined;
    if (last && !last.summary.continuable) return this.runStage({ project, work, stage });
    return this.followUp({ project, work, stage, message: message.trim() || "预算已追加。请从中断的地方继续完成本阶段的任务。" });
  }

  usageAnalysis({ attempt }: { project: string; work: string; attempt: string }): UsageAnalysis {
    const a = this.attempts.get(attempt);
    if (!a) throw new Error(`找不到运行记录 ${attempt}`);
    const t = mockTokens(a);
    const kinds = ["读取", "命令", "编辑", "回答"];
    const first = a.firstContext ?? 9000;
    const calls = Array.from({ length: 12 }, (_, i) => ({ kind: kinds[i % 4], detail: i % 4 === 1 ? "cargo test --locked" : i % 4 === 0 ? "src/features/export/csv.ts" : "", context: first + i * 2600, output: 300 + (i % 3) * 120 }));
    const part = (f: number): Tokens => ({ input: Math.round(t.input * f), cache_read: Math.round(t.cache_read * f), cache_write: 0, output: Math.round(t.output * f), reasoning: 0 });
    const by = [0.42, 0.31, 0.17, 0.1].map((f, i) => ({ kind: kinds[i], calls: 3, tokens: part(f), total: sumTokens(part(f)), output_bytes: Math.round(f * 40000) }));
    return {
      summary: { tokens: t, cost_usd: a.summary.client === "Claude" ? 0.42 : null, models: [a.summary.model || "default"], calls: 12 },
      by_kind: by,
      calls,
      heaviest: [{ call: 1, kind: "命令", detail: "cargo test --locked", bytes: 18000, tokens: 4600, carried: 46000 }],
      launch: a.summary.launch,
      reused: a.summary.session?.kind === "reused",
      window_calls: 12,
    };
  }

  projectUsage({ project }: { project: string }): ProjectUsage {
    this.tick();
    const rows = new Map<string, Map<string, { tokens: Tokens; runs: number; cost: number }>>();
    const add = (group: string, key: string, t: Tokens, cost: number) => {
      const g = rows.get(group) ?? new Map();
      rows.set(group, g);
      const r = g.get(key) ?? { tokens: { input: 0, cache_read: 0, cache_write: 0, output: 0, reasoning: 0 }, runs: 0, cost: 0 };
      r.tokens = { input: r.tokens.input + t.input, cache_read: r.tokens.cache_read + t.cache_read, cache_write: 0, output: r.tokens.output + t.output, reasoning: 0 };
      r.runs += 1;
      r.cost += cost;
      g.set(key, r);
    };
    const runs: ProjectUsage["runs"] = [];
    for (const a of this.attempts.values()) {
      const w = this.works.get(a.work);
      if (!w || w.project !== project) continue;
      const t = mockTokens(a);
      const cost = a.summary.client === "Claude" ? sumTokens(t) / 200000 : 0;
      const stage = w.detail.stages[a.stage]?.name ?? "?";
      add("items", w.detail.id, t, cost);
      add("stages", stage, t, cost);
      add("models", `${a.summary.client} · ${a.summary.model || "默认"}`, t, cost);
      add("days", (a.summary.started_at ?? "").slice(0, 10), t, cost);
      runs.push({
        work: w.detail.id, work_name: w.detail.name, attempt: a.summary.id, stage, stage_index: a.stage, client: a.summary.client, model: a.summary.model, status: a.summary.status,
        started_at: a.summary.started_at ?? "", tokens: t, total: sumTokens(t), cost_usd: cost || null, calls: a.calls,
        diet: !!a.summary.launch, reused_from: a.summary.session?.kind === "reused" ? a.summary.session.stage_name ?? null : null, count_maybe_high: a.summary.count_maybe_high, launch: a.summary.launch,
      });
    }
    const list = (group: string) =>
      [...(rows.get(group) ?? new Map()).entries()]
        .map(([key, r]) => ({ key, tokens: r.tokens, total: sumTokens(r.tokens), cost_usd: r.cost, runs: r.runs }))
        .sort((x, y) => y.total - x.total);
    const items = list("items").map((r) => {
      const w = this.works.get(r.key)!;
      return { ...r, name: w.detail.name, shelved: !!w.detail.shelved, budget: w.budgetTotal ?? null };
    });
    const all = list("stages").reduce(
      (acc, r) => ({ tokens: { input: acc.tokens.input + r.tokens.input, cache_read: acc.tokens.cache_read + r.tokens.cache_read, cache_write: 0, output: acc.tokens.output + r.tokens.output, reasoning: 0 }, cost: acc.cost + r.cost_usd, runs: acc.runs + r.runs }),
      { tokens: { input: 0, cache_read: 0, cache_write: 0, output: 0, reasoning: 0 } as Tokens, cost: 0, runs: 0 },
    );
    return {
      total: { tokens: all.tokens, total: sumTokens(all.tokens), cost_usd: all.cost, runs: all.runs },
      stage_budget: this.projectBudget.get(project) ?? null,
      resume_limit: this.resumeLimit.get(project) ?? null,
      compare: this.compare(project),
      maybe_high_runs: runs.filter((r) => r.count_maybe_high).length,
      items,
      stages: list("stages"),
      models: list("models"),
      days: list("days").sort((x, y) => x.key.localeCompare(y.key)),
      runs: runs.sort((x, y) => y.total - x.total).slice(0, 12),
      quota: [{ client: "Claude", at: new Date().toISOString(), windows: { five_hour: 0.41, seven_day: 0.33 } }],
    };
  }

  /** Before / after the minimal launch, Codex and Claude runs only (design §3.7). */
  compare(project: string): ProjectUsage["compare"] {
    const groups = { by_stage: new Map<string, MockAttempt[][]>(), by_model: new Map<string, MockAttempt[][]>() };
    for (const a of this.attempts.values()) {
      const w = this.works.get(a.work);
      if (!w || w.project !== project || isLiveStatus(a.summary.status)) continue;
      if (a.summary.client !== "Claude" && a.summary.client !== "Codex") continue;
      const side = a.summary.launch ? 1 : 0;
      const keys: [Map<string, MockAttempt[][]>, string][] = [
        [groups.by_stage, w.detail.stages[a.stage]?.name ?? "?"],
        [groups.by_model, `${a.summary.client} · ${a.summary.model || "默认模型"}`],
      ];
      for (const [g, key] of keys) {
        const row = g.get(key) ?? [[], []];
        row[side].push(a);
        g.set(key, row);
      }
    }
    const side = (runs: MockAttempt[]): CompareSide => {
      const counted = runs.filter((r) => r.firstContext !== null);
      return {
        runs: runs.length,
        first_context_median: median(counted.map((r) => r.firstContext!)),
        avg_calls: avg(counted.map((r) => r.calls)),
        avg_total: avg(counted.map((r) => sumTokens(mockTokens(r)))),
      };
    };
    const rows = (g: Map<string, MockAttempt[][]>): CompareRow[] =>
      [...g.entries()]
        .map(([key, [before, after]]) => {
          const b = side(before);
          const a = side(after);
          return { key, before: b, after: a, change: b.avg_total && a.avg_total !== null ? a.avg_total / b.avg_total - 1 : null };
        })
        .sort((x, y) => (y.before.avg_total ?? -1) - (x.before.avg_total ?? -1));
    return { by_stage: rows(groups.by_stage), by_model: rows(groups.by_model) };
  }

  attemptView({ project, work, attempt }: { project: string; work: string; attempt: string }): AttemptView {
    this.tick();
    this.work(project, work);
    const a = this.attempts.get(attempt);
    if (!a) throw new Error(`找不到运行记录 ${attempt}`);
    const progress = a.entries.length / Math.max(1, a.entries.length + a.script.length);
    return {
      id: a.summary.id,
      status: a.summary.status,
      entries: clone(a.entries),
      input_tokens: Math.round(a.tokens[0] * (a.summary.status === "running" ? progress : 1)),
      output_tokens: Math.round(a.tokens[1] * (a.summary.status === "running" ? progress : 1)),
      usage: { tokens: mockTokens(a), cost_usd: a.summary.client === "Claude" ? 0.42 : null, models: [a.summary.model || "default"], calls: 12 },
      token_limit: a.limit,
      model: a.summary.model || null,
      error: a.summary.error,
      result: a.summary.status === "succeeded" ? a.result : null,
      zcode:
        a.summary.client === "ZCode"
          ? {
              waiting: a.summary.status === "running",
              tools: a.summary.status === "running" ? [{ name: "Bash", detail: "cargo test --locked" }] : [],
              session: `sess_${a.summary.id}`,
              openable: true,
            }
          : null,
      dir: `${HOME}/runs/${work}/${a.summary.id}`,
      workspace: `${HOME}/worktrees/${work}`,
      live_usage: !(a.summary.client === "Codex" && a.noRollout),
      launch: a.summary.launch,
      usage_window: a.summary.status === "running" ? null : { calls: a.calls },
    };
  }

  updateStage(a: { project: string; work: string; stage: number; note?: string; profile?: Profile; sandbox?: Sandbox }) {
    const s = this.stage(this.work(a.project, a.work), a.stage);
    if (a.note !== undefined) s.note = a.note;
    if (a.profile) s.profile = { ...a.profile, executable: a.profile.executable || EXECUTABLES[a.profile.client] };
    if (a.sandbox) s.sandbox = a.sandbox;
  }

  runStage({ project, work, stage }: { project: string; work: string; stage: number }): string {
    const w = this.work(project, work);
    if (w.detail.running) throw new Error("已有 Agent 在运行，请先停止");
    if (stage !== w.detail.stage) throw new Error("只能运行当前阶段");
    const s = this.stage(w, stage);
    return this.startRun(w, stage, s.note).summary.id;
  }

  followUp({ project, work, stage, message }: { project: string; work: string; stage: number; message: string }): string {
    const w = this.work(project, work);
    if (w.detail.running) throw new Error("Agent 正在运行，稍后再继续对话");
    if (!message.trim()) throw new Error("请输入要继续说明的内容");
    const a = this.addAttempt(w, stage, {
      prompt: message.trim(),
      followUp: true,
      script: followUpScript(message.trim()),
      stepMs: 700,
      result: "已按补充说明调整。",
    });
    return a.summary.id;
  }

  cancel({ project, work, attempt }: { project: string; work: string; attempt: string }) {
    const w = this.work(project, work);
    const a = this.attempts.get(attempt);
    if (!a || a.summary.status !== "running") throw new Error("该运行已结束");
    a.outcome = { status: "cancelled", error: "已被用户停止", route_back: null };
    a.script = [];
    a.entries.push({ kind: "notice", text: "收到停止请求，Agent 已退出。" });
    this.finish(a, Date.now());
    if (w.detail.autopilot.running) this.stopAutopilot(w, "stopped", "运行被手动停止");
  }

  advance({ project, work, stage }: { project: string; work: string; stage: number }) {
    const w = this.work(project, work);
    const d = w.detail;
    if (stage !== d.stage) throw new Error("只能确认当前阶段");
    if (d.running) throw new Error("Agent 仍在运行");
    const s = this.stage(w, stage);
    const latest = s.attempts[s.attempts.length - 1];
    if (!latest || latest.status !== "succeeded") throw new Error(`「${s.name}」还没有成功的运行，不能确认`);
    this.doAdvance(w);
  }

  sendBack(a: { project: string; work: string; from: number; to: number; reason?: string }): string {
    const w = this.work(a.project, a.work);
    if (w.detail.running) throw new Error("Agent 仍在运行");
    if (a.to >= a.from) throw new Error("只能退回到更早的阶段");
    return this.doSendBack(w, a.from, a.to, a.reason?.trim() || "测试未通过");
  }

  waive({ project, work, stage, reason }: { project: string; work: string; stage: number; reason: string }) {
    const w = this.work(project, work);
    if (!reason.trim()) throw new Error("人工放行需要填写理由");
    if (stage !== w.detail.stage) throw new Error("只能放行当前阶段");
    const s = this.stage(w, stage);
    const latest = s.attempts[s.attempts.length - 1];
    if (latest) {
      latest.status = "succeeded";
      latest.error = null;
      latest.route_back = null;
      const a = this.attempts.get(latest.id);
      a?.entries.push({ kind: "notice", text: `人工放行：${reason.trim()}` });
    }
    this.doAdvance(w);
  }

  skip({ project, work, stage, reason }: { project: string; work: string; stage: number; reason: string }) {
    const w = this.work(project, work);
    const s = this.stage(w, stage);
    if (!s.skippable) throw new Error(`「${s.name}」不能跳过`);
    if (stage < w.detail.stage) throw new Error("已完成的阶段不能跳过");
    if (stage > w.detail.stage && s.state === "skipped") {
      // Skipping a later stage again restores it.
      s.state = "upcoming";
      s.skipped_reason = null;
      return;
    }
    if (stage === w.detail.stage && w.detail.running) throw new Error("Agent 仍在运行");
    s.state = "skipped";
    s.skipped_reason = reason.trim() || "无需此阶段";
    if (stage === w.detail.stage) {
      if (w.detail.running) throw new Error("Agent 仍在运行");
      let next = stage + 1;
      while (next < w.detail.stages.length && w.detail.stages[next].state === "skipped") next += 1;
      w.detail.stage = next;
      if (next < w.detail.stages.length) w.detail.stages[next].state = "current";
    }
  }

  reopen({ project, work, stage }: { project: string; work: string; stage: number }) {
    const w = this.work(project, work);
    const d = w.detail;
    if (d.running) throw new Error("Agent 仍在运行");
    if (d.delivered) throw new Error("事项已交付，不能重新打开阶段");
    if (stage >= d.stage) throw new Error("只能重新打开已完成的阶段");
    d.stages.forEach((s, i) => {
      if (i === stage) s.state = "current";
      else if (i > stage && s.state !== "skipped") s.state = "upcoming";
    });
    d.reworks.push({ from: d.stage, to: stage, reason: "手动重新打开", at: iso(Date.now()) });
    d.stage = stage;
  }

  autopilotStart({ project, work, until }: { project: string; work: string; until: number }) {
    const w = this.work(project, work);
    const d = w.detail;
    if (d.delivered) throw new Error("事项已交付");
    if (until < d.stage || until >= d.stages.length) throw new Error("目标阶段无效");
    d.autopilot = { running: true, status: "running", message: "准备自动运行…", stage: d.stage, until };
    w.autopilotRan = new Set(d.running ? [d.stage] : []);
    w.autopilotLoops = 0;
    this.driveAutopilot(w);
  }

  autopilotStop({ project, work }: { project: string; work: string }) {
    const w = this.work(project, work);
    if (!w.detail.autopilot.running) throw new Error("自动运行未在进行");
    this.stopAutopilot(w, "stopped", "已手动停止自动运行，当前阶段的 Agent 会继续完成本轮");
  }

  archive({ project, work, memory, removeWorktree }: { project: string; work: string; memory: string; removeWorktree: boolean }) {
    const w = this.work(project, work);
    const d = w.detail;
    if (!d.delivered) throw new Error("只有已交付的事项可以归档");
    if (memory.length > 1000) throw new Error("记忆内容超过 1000 字");
    let id: number | null = null;
    if (memory.trim()) id = this.addMemory({ project, text: memory.trim() });
    d.shelved = { at: iso(Date.now()), memory: id };
    d.memory_draft = null;
    if (removeWorktree) {
      d.worktrees = [];
      d.branches = d.branches.filter((b) => !b.merged);
    }
  }

  branchGraph({ project, path }: { project: string; path: string; limit: number }): BranchGraph {
    if (!this.projects.some((p) => p.id === project)) throw new Error("项目不存在");
    const now = Math.floor(Date.now() / 1000);
    const c = (sha: string, parents: string[], subject: string, hoursAgo: number, author = "林舟") => ({ sha: sha.padEnd(40, "0"), parents: parents.map((p) => p.padEnd(40, "0")), subject, author, time: now - hoursAgo * 3600 });
    const commits = [
      c("a7", ["a6"], "导出对账单：补齐表头", 1, "Claude"),
      c("f3", ["a5"], "分组拖拽：持久化排序", 2, "Codex"),
      c("a6", ["a5"], "导出对账单：CSV 编码", 3, "Claude"),
      c("m4", ["m3", "b2"], "Merge work item: 行情快照导出 CSV", 5),
      c("b2", ["b1"], "快照导出：分页", 7, "ZCode"),
      c("a5", ["m3"], "导出对账单：骨架", 8, "Claude"),
      c("m3", ["m2"], "升级 quote-gateway 客户端", 20),
      c("b1", ["m2"], "快照导出：接口", 26, "ZCode"),
      c("m2", ["m1"], "首页布局调整", 40),
      c("m1", [], "init", 80),
    ];
    const tip = (s: string) => s.padEnd(40, "0");
    return {
      repository: path,
      head: "main",
      branches: [
        { name: "main", sha: tip("m4"), ahead: 0, behind: 0, current: true, worktree: null, item: null },
        { name: "aep-stage/7f3a9c21", sha: tip("f3"), ahead: 2, behind: 1, current: false, worktree: { path: `${HOME}/worktrees/7f3a9c21`, source: "AEP", changed: 1, untracked: 0 }, item: { id: "w2", name: "自选股分组支持拖拽排序" } },
        { name: "feat/export-statement", sha: tip("a7"), ahead: 3, behind: 1, current: false, worktree: { path: `${HOME}/.claude/worktrees/export`, source: "Claude", changed: 0, untracked: 74 }, item: null },
        { name: "fix/snapshot-paging", sha: tip("b2"), ahead: 0, behind: 2, current: false, worktree: null, item: null },
        ...(path === REPOS[0] ? [{ name: "feat/local-notes", sha: tip("m3"), ahead: 0, behind: 1, current: false, worktree: null, item: null }] : []),
      ],
      commits,
      truncated: false,
    };
  }

  gitTools() {
    return { tools: [{ id: "app:Fork", label: "Fork" }, { id: "app:Visual Studio Code", label: "VS Code（Git Graph 扩展）" }, { id: "tui:lazygit", label: "lazygit" }], config: `${HOME}/plugins/git-tools.json` };
  }

  revealGitToolsConfig(): "file" {
    console.info("在访达中选中 git-tools.json", this.gitTools().config);
    return "file";
  }

  openGitTool({ id }: { id: string; path: string }) {
    if (!id) throw new Error("找不到这个 git 工具");
  }

  plugins(): PluginInfo[] {
    const branches: PluginInfo = { id: "branches", enabled: this.branchPluginEnabled, badge: "enabled", host: null, token: null, error: null };
    const forges = FORGES.map((id): PluginInfo => {
      const { enabled, host, credential, error } = this.forgePlugins[id];
      return { id, enabled, host, token: { set: !!credential, hint: credential?.hint ?? null }, badge: "disabled", error };
    });
    return [branches, ...forges].map((plugin) => ({ ...plugin, badge: pluginBadge(plugin, this.branchPluginEnabled) }));
  }

  setPluginEnabled({ plugin, enabled }: { plugin: PluginId; enabled: boolean }) {
    if (plugin === "branches") this.branchPluginEnabled = enabled;
    else this.forgePlugins[plugin].enabled = enabled;
    return this.plugins();
  }

  savePlugin({ plugin, host, token }: Parameters<Api["savePlugin"]>[0]) {
    const config = this.forgePlugins[plugin];
    const url = mockHost(host, plugin);
    if (token !== null && !/^[a-z\d_.-]{8,512}$/i.test(token)) throw new Error("访问令牌格式无效");
    config.host = url;
    if (token !== null) config.credential = mockCredential(token);
    config.error = null;
    return this.plugins();
  }

  clearPluginToken({ plugin }: Parameters<Api["clearPluginToken"]>[0]) {
    const config = this.forgePlugins[plugin];
    config.credential = null;
    config.error = null;
    return this.plugins();
  }

  testPlugin({ plugin, host, token }: Parameters<Api["testPlugin"]>[0]): TestResult {
    const hostname = new URL(mockHost(host, plugin)).hostname;
    const config = this.forgePlugins[plugin];
    const credential = token === null ? config.credential : mockCredential(token);
    const kind = hostname === "offline" || hostname.endsWith(".invalid") ? "unreachable" : credential ? credential.outcome : "auth";
    const error = kind === null ? null : mockError(kind, hostname);
    if (token === null && (!kind || kind === "auth" || kind === "forbidden" || kind === "cert")) config.error = error;
    return error ? { ok: false, error } : { ok: true, account: "octocat" };
  }

  remoteStatus({ project, path }: Parameters<Api["remoteStatus"]>[0]): RemoteStatus {
    if (!this.branchPluginEnabled || !FORGES.some((id) => this.forgePlugins[id].enabled)) return { active: false };
    if (!this.projects.some((p) => p.id === project && p.repositories.includes(path))) throw new Error("仓库不属于这个项目");
    const github = path === REPOS[0];
    const remote = github
      ? { name: "origin", url: "git@github.com:demo/quote-terminal.git", host: "github.com", path: "demo/quote-terminal" }
      : path === REPOS[1]
        ? { name: "origin", url: "ssh://git@gitlab.example.com:2222/g/sub/r.git", host: "gitlab.example.com", path: "g/sub/r" }
        : null;
    const plugin = remote && FORGES.find((id) => this.forgePlugins[id].enabled && new URL(this.forgePlugins[id].host).hostname === remote.host);
    const credential = plugin ? this.forgePlugins[plugin].credential : null;
    const deadline = this.mockRefresh.get(path);
    const refreshing = deadline !== undefined && Date.now() < deadline;
    const refreshedAt = deadline !== undefined && !refreshing ? Math.floor(deadline / 1000) : null;
    const issue = refreshedAt && credential?.outcome && remote ? mockError(credential.outcome, remote.host, refreshedAt) : null;
    const pull = (number: number, state: Pull["state"], title: string, others = 0): Pull => ({
      number, state, title, others, created_at: this.mockRemoteAt,
      url: `https://github.com/demo/quote-terminal/pull/${number}`,
    });
    return {
      active: true, repository: path, remotes: remote ? [remote] : [],
      remote: remote && { name: remote.name, host: remote.host, path: remote.path, plugin: plugin ?? null, badge: this.plugins().find((p) => p.id === plugin)?.badge ?? null },
      refreshing, refreshed_at: refreshedAt ?? (github ? this.mockRemoteAt : null),
      branches: github ? [
        { name: "main", upstream: "origin/main", present: true, ahead: 0, behind: 0, same_name: null },
        { name: "aep-stage/7f3a9c21", upstream: "origin/aep-stage/7f3a9c21", present: true, ahead: 2, behind: 1, same_name: null },
        { name: "feat/export-statement", upstream: "origin/feat/export-statement", present: false, ahead: null, behind: null, same_name: null },
        { name: "fix/snapshot-paging", upstream: null, present: false, ahead: null, behind: null, same_name: "origin/fix/snapshot-paging" },
        { name: "feat/local-notes", upstream: null, present: false, ahead: null, behind: null, same_name: null },
      ] : [],
      remote_only: github ? ["origin/release/2.4", "origin/feat/盘口优化", "origin/infra-metrics"].map((name, i) => ({ name, sha: `c${i}`.padEnd(40, "0") })) : [],
      pulls: github && credential ? {
        "aep-stage/7f3a9c21": pull(123, "open", "自选股分组支持拖拽排序", 2),
        "feat/export-statement": pull(124, "draft", "导出对账单"),
        main: pull(120, "merged", "行情终端本周变更"),
        "fix/snapshot-paging": pull(119, "closed", "修正快照分页"),
      } : {},
      pulls_checked: github && !!credential,
      fetch_error: refreshedAt && credential?.fetchFailure ? { message: "示例：git fetch 失败", auth: false } : null,
      api_error: issue,
    };
  }

  remoteRefresh(args: Parameters<Api["remoteRefresh"]>[0]): RemoteStatus {
    const status = this.remoteStatus(args);
    if (!status.active || !status.remote || status.refreshing) return status;
    this.mockRefresh.set(args.path, Date.now() + 1000);
    return this.remoteStatus(args);
  }

  openUrl({ url }: Parameters<Api["openUrl"]>[0]): null {
    if (!url.startsWith("https://") || !new URL(url).hostname) throw new Error("链接地址无效");
    return null;
  }

  drafts = new Map<string, DraftStatus & { polls: number }>();

  draftStart({ project, client, request }: { project: string; client: "Codex" | "Claude"; model: string; request: string; repository?: string }) {
    if (!request.trim()) throw new Error("请描述要做的事");
    if (this.drafts.get(project)?.status === "running") throw new Error("已有一次起草在进行");
    const id = String(Date.now());
    this.drafts.set(project, { id, status: "running", error: null, client, request, draft: null, polls: 0 });
    return id;
  }

  draftStatus({ project }: { project: string }): DraftStatus {
    const d = this.drafts.get(project);
    if (!d) return { id: null, status: null, error: null, client: null, request: null, draft: null };
    if (d.status === "running" && ++d.polls >= 2) {
      d.status = "succeeded";
      d.draft = {
        summary: "导出和表头是两件可以分开交付的事，拆成两个事项。",
        items: [
          { name: "对账单 · 导出 CSV", release: false, template: "Software", brief: "目标：用户可把对账单导出为 CSV。范围：src/features/statement 下新增导出按钮与服务端接口；不做 Excel。验收：金额按分转元、UTF-8 BOM。" },
          { name: "对账单 · 空账户表头", release: false, template: "Software", brief: "目标：空账户导出时也带表头。待确认：是否需要提示「无数据」。" },
        ],
      };
    }
    const { polls: _polls, ...status } = d;
    return clone(status);
  }

  tidy = new Map<string, TidyStatus & { polls: number }>();

  tidyStart({ project, client, model, instruction }: { project: string; client: "Codex" | "Claude"; model: string; instruction: string }) {
    if (this.tidy.get(project)?.status === "running") throw new Error("已有一次整理在进行");
    const id = String(Date.now());
    this.tidy.set(project, { id, status: "running", error: null, client, model, instruction, applied: null, proposal: null, polls: 0 });
    return id;
  }

  tidyStatus({ project }: { project: string }): TidyStatus {
    const t = this.tidy.get(project);
    if (!t) return { id: null, status: null, error: null, client: null, model: null, instruction: null, applied: null, proposal: null };
    if (t.status === "running" && ++t.polls >= 2) {
      const current = (this.memoryNotes.get(project) ?? []).filter((n) => !n.superseded);
      const zustand = current.find((n) => n.body.includes("zustand"));
      const gateway = current.find((n) => n.body.includes("quote-gateway"));
      const timeline = current.find((n) => n.body.includes("分时图"));
      t.status = "succeeded";
      t.proposal = {
        summary: "合并两条前端约定，改写一条含糊的笔记，移出已完成的下线记录。",
        ops: [
          ...(zustand && gateway
            ? [{ op: "merge" as const, ids: [gateway.id, zustand.id], text: "前端约定：行情只走 quote-gateway 的 WebSocket；状态用 zustand 切片，不加全局单例。", why: "两条都是前端架构约定" }]
            : []),
          ...(timeline ? [{ op: "retire" as const, id: timeline.id, why: "旧分时图已下线，后续事项不需要" }] : []),
        ],
      };
    }
    const { polls: _polls, ...status } = t;
    return clone(status);
  }

  tidyApply({ project, job, ops }: { project: string; job: string; ops: number[] }) {
    const t = this.tidy.get(project);
    if (!t || t.id !== job || !t.proposal) throw new Error("整理方案不存在");
    for (const i of ops) {
      const op = t.proposal.ops[i];
      if (!op) throw new Error("方案里没有这个操作");
      if (op.op === "rewrite") this.reviseMemory({ project, id: op.id, text: op.text });
      if (op.op === "retire") this.retire(project, op.id);
      if (op.op === "merge") {
        this.reviseMemory({ project, id: op.ids[0], text: op.text });
        for (const id of op.ids.slice(1)) this.retire(project, id);
      }
    }
    t.applied = ops;
    return ops.length;
  }

  private retire(project: string, id: number) {
    const n = (this.memoryNotes.get(project) ?? []).find((x) => x.id === id);
    if (!n || n.superseded) throw new Error(`记忆 #${id} 在提出方案后已被修改，请重新整理`);
    n.superseded = true;
  }

  removeProject({ project }: { project: string }) {
    if (!this.projects.some((p) => p.id === project)) throw new Error("项目不存在");
    this.projects = this.projects.filter((p) => p.id !== project);
    for (const [id, w] of [...this.works]) if (w.project === project) this.works.delete(id);
    this.order = this.order.filter((id) => this.works.has(id));
    if (this.active === project) this.active = this.projects[0]?.id ?? null;
  }

  importScan({ path }: { path: string }): ImportFound {
    const repository = path.trim().replace(/\/$/, "");
    if (!repository.startsWith("/")) throw new Error("不是 Git 仓库");
    return {
      repository,
      memories: [
        { file: "money-in-cents.md", title: "money-in-cents", description: "金额统一用分存储", chars: 120 },
        { file: "release-flow.md", title: "release-flow", description: "发版前先跑回归和打包", chars: 340 },
        { file: "android-build.md", title: "android-build", description: "Android 构建需要 JDK 17", chars: 210 },
      ],
      rules: ["CLAUDE.md", "AGENTS.md"],
      claude_sessions: 19,
      codex_sessions: 3,
      zcode_tasks: 38,
      worktrees: [
        { path: `${HOME}/.zcode/worktrees/8ab3/app`, branch: "chore/code-structure-review", ahead: 4, dirty: 0, untracked: 0, source: "ZCode" },
        { path: `${repository}/.claude/worktrees/fix1448`, branch: "fix/1448", ahead: 2, dirty: 3, untracked: 74, source: "Claude" },
        { path: "/tmp/pr1932wt", branch: null, ahead: 0, dirty: 0, untracked: 0, source: "Git" },
      ],
    };
  }

  importSyncPlan({ project, path }: { project: string; path: string }): SyncPlan {
    const imported = this.order.map((id) => this.works.get(id)!).filter((w) => w.project === project && /\//.test(w.detail.name));
    const seen = new Set<string>();
    const remove: SyncPlan["items_remove"] = [];
    const keep: SyncPlan["items_keep"] = [];
    for (const w of [...imported].reverse()) {
      const name = w.detail.name;
      if (seen.has(name)) remove.push({ id: w.detail.id, name, reason: "重复导入" });
      else if (name === "fix/1448") remove.push({ id: w.detail.id, name, reason: "原工作区已删除" });
      else keep.push({ id: w.detail.id, name, reason: "工作区仍在" });
      seen.add(name);
    }
    const found = this.importScan({ path });
    return {
      memories_add: ["android-build.md"],
      memories_update: ["release-flow.md"],
      memories_unchanged: 1,
      memories_remove: [{ id: 99, title: "old-deploy" }],
      items_add: found.worktrees.filter((w) => w.branch && w.branch !== "fix/1448" && !seen.has(w.branch)).map((w) => ({ branch: w.branch!, path: w.path })),
      items_keep: keep,
      items_remove: remove,
      items_stale: [],
      items_skip: [{ branch: "feature/merged-already", reason: "分支已合并，没有未完成的改动" }],
    };
  }

  importSync(a: { project: string; path: string; memories: boolean; items: boolean }): SyncPlan {
    const plan = this.importSyncPlan(a);
    if (a.items) {
      for (const r of plan.items_remove) this.deleteWork({ project: a.project, work: r.id });
      for (const b of plan.items_add) this.createWork({ project: a.project, name: b.branch, template: "Software", release: false, repositories: [a.path] });
    }
    return plan;
  }

  importApply(a: { project: string; path: string; memories: string[]; branches: { branch: string; path: string }[] }) {
    if (!this.projects.some((p) => p.id === a.project)) throw new Error("项目不存在");
    for (const m of a.memories) this.addMemory({ project: a.project, text: `【导入自 Claude 记忆：${m.replace(/\.md$/, "")}】` });
    const items = a.branches.map((b) => {
      const existing = [...this.works.values()].find((w) => w.project === a.project && w.detail.name === b.branch);
      return existing ? existing.detail.id : this.createWork({ project: a.project, name: b.branch, template: "Software", release: false, repositories: [a.path] });
    });
    return { notes: a.memories.length, items };
  }

  pickFolder(_: { prompt: string }): string | null {
    return `${HOME}/code/picked-${++this.picks}`;
  }

  deleteWork({ project, work }: { project: string; work: string }) {
    const d = this.work(project, work).detail;
    if (d.stages.some((s) => s.attempts.length > 0)) throw new Error("这个事项已经运行过，不能删除；交付后可以归档");
    this.works.delete(work);
    this.order = this.order.filter((id) => id !== work);
    if (this.selected.get(project) === work) {
      this.selected.set(project, this.order.find((id) => this.works.get(id)?.project === project) ?? null);
    }
  }

  zcodeOpen({ project, work, attempt }: { project: string; work: string; attempt: string }) {
    this.work(project, work);
    if (!this.attempts.get(attempt)) throw new Error(`找不到运行记录 ${attempt}`);
  }

  cleanup({ project, work, force }: { project: string; work: string; force: boolean }) {
    const d = this.work(project, work).detail;
    if (!d.delivered) throw new Error("只有已交付的事项可以清理工作区和分支");
    const worktrees = d.worktrees.length;
    const deleted = d.branches.filter((b) => b.merged || force).map((b) => b.name);
    const kept = d.branches.filter((b) => !b.merged && !force).map((b) => b.name);
    d.worktrees = [];
    d.branches = d.branches.filter((b) => kept.includes(b.name));
    return { worktrees, deleted, kept };
  }

  unarchive({ project, work }: { project: string; work: string }) {
    const d = this.work(project, work).detail;
    if (!d.shelved) throw new Error("事项未归档");
    d.shelved = null;
    d.memory_draft = memoryDraftFor(d);
  }

  mergePreview({ project, work, target }: { project: string; work: string; target?: string }): MergePreview {
    const d = this.work(project, work).detail;
    if (!d.delivered) throw new Error("事项尚未交付");
    const merged = d.deliveries[0]?.merged ?? null;
    const branches = [
      {
        name: "main",
        status: merged?.target === "main" ? "already_contained" : "fast_forward",
        reason: merged?.target === "main" ? "已包含此交付" : "可快进合并",
        writable: true,
        checked_out: REPOS[0],
      },
      {
        name: "develop",
        status: "fast_forward",
        reason: "可快进合并",
        writable: true,
        checked_out: null,
      },
      {
        name: "release/2.3",
        status: "diverged",
        reason: "目标分支已分叉，需要先在仓库中变基",
        writable: false,
        checked_out: null,
      },
    ];
    const t = target ?? "main";
    const branch = branches.find((b) => b.name === t);
    if (!branch) throw new Error(`分支 ${t} 不存在`);
    const base: MergePreview = {
      target: t,
      branches,
      candidate: d.branch ?? "",
      stat: "",
      diff: "",
      truncated: false,
      digest: "",
      files_changed: 0,
      insertions: 0,
      deletions: 0,
      error: null,
    };
    if (branch.status === "already_contained") {
      return { ...base, error: `「${t}」已经包含这次交付，无需再次合并。` };
    }
    if (branch.status === "diverged") {
      return { ...base, error: `「${t}」与交付分支已分叉（领先 3 个提交），无法快进合并。请先在仓库中将交付分支变基到 ${t}。` };
    }
    return {
      ...base,
      stat: SAMPLE_STAT,
      diff: SAMPLE_DIFF,
      digest: `sha256:${d.id}${t}`.padEnd(24, "0"),
      files_changed: 4,
      insertions: 128,
      deletions: 17,
    };
  }

  mergeConfirm({ project, work, target, digest }: { project: string; work: string; target: string; digest: string }) {
    const d = this.work(project, work).detail;
    if (!digest.startsWith("sha256:")) throw new Error("预览已过期，请刷新后再合并");
    if (target === "release/2.3") throw new Error("目标分支已分叉，无法合并");
    const delivery = d.deliveries[0];
    if (!delivery) throw new Error("没有可合并的交付");
    delivery.merged = { target, sha: Math.random().toString(16).slice(2, 14), at: iso(Date.now()) };
    d.branches = d.branches.map((b) => ({ ...b, merged: true }));
  }

  memories({ project }: { project: string }): MemoryNote[] {
    return clone(this.memoryNotes.get(project) ?? []);
  }

  addMemory({ project, text }: { project: string; text: string }): number {
    if (!text.trim()) throw new Error("记忆内容不能为空");
    const notes = this.memoryNotes.get(project) ?? [];
    const id = notes.reduce((m, n) => Math.max(m, n.id), 0) + 1;
    notes.push({ id, kind: "note", author: "你", body: text.trim(), created_at: iso(Date.now()), superseded: false });
    this.memoryNotes.set(project, notes);
    return id;
  }

  reviseMemory({ project, id, text }: { project: string; id: number; text: string }): number {
    const notes = this.memoryNotes.get(project) ?? [];
    const old = notes.find((n) => n.id === id);
    if (!old) throw new Error("记忆不存在");
    if (old.superseded) throw new Error("这条记忆已有更新的版本，请在最新版本上修改");
    if (old.body === text.trim()) throw new Error("内容没有变化");
    old.superseded = true;
    const next = notes.reduce((m, n) => Math.max(m, n.id), 0) + 1;
    notes.push({ id: next, kind: old.kind, author: "manual", body: text.trim(), created_at: iso(Date.now()), superseded: false, replaces: id });
    return next;
  }

  repoMemories({ path }: { path: string }): MemoryNote[] {
    if (!this.projects.some((p) => p.repositories.includes(path))) throw new Error(`仓库未在任何项目中：${path}`);
    return clone(this.repoNotes.get(path) ?? []);
  }

  addRepoMemory({ path, text }: { path: string; text: string }): number {
    if (!this.projects.some((p) => p.repositories.includes(path))) throw new Error(`仓库未在任何项目中：${path}`);
    if (!text.trim()) throw new Error("记忆内容不能为空");
    if (text.length > 1000) throw new Error("记忆内容超过 1000 字");
    const notes = this.repoNotes.get(path) ?? [];
    const id = notes.reduce((m, n) => Math.max(m, n.id), 0) + 1;
    notes.push({ id, kind: "note", author: "你", body: text.trim(), created_at: iso(Date.now()), superseded: false });
    this.repoNotes.set(path, notes);
    return id;
  }

  history({ project, work }: { project: string; work: string }): WorkHistory {
    this.tick();
    const d = this.work(project, work).detail;
    const notes = this.memoryNotes.get(project) ?? [];
    const projectReads: HistoryNote[] = notes
      .filter((n) => !n.superseded)
      .slice(-2)
      .map((n) => ({ id: n.id, kind: n.kind === "convention" || n.kind === "delivery" ? "decision" : n.kind, body: n.body }));
    const flat = d.stages
      .flatMap((s) => s.attempts.map((a, i) => ({ s, a, i })))
      .sort((x, y) => Date.parse(x.a.started_at ?? "") - Date.parse(y.a.started_at ?? ""));
    let noteId = 100;
    const written = new Map<number, HistoryNote[]>();
    const seen = new Set<number>();
    const rounds: HistoryRound[] = flat.map(({ s, a, i }) => {
      const round = 1 + d.reworks.filter((r) => r.to === s.index && Date.parse(r.at) <= Date.parse(a.started_at ?? "")).length;
      const prior = [...written.entries()].filter(([k]) => k < s.index).flatMap(([, v]) => v).slice(-2);
      const writes: HistoryNote[] = [];
      if (a.status === "succeeded" || a.status === "failed") {
        if (a.route_back) writes.push({ id: noteId++, kind: "review", body: `${a.error ?? "未通过"}。需要「${a.route_back.name}」处理：${a.route_back.question}` });
        else if (s.role === "Plan") writes.push({ id: noteId++, kind: "decision", body: HANDOFFS[s.name] ?? `「${s.name}」结论已写入 docs/work/${s.name}.md。` });
        else if (s.role === "Build") writes.push({ id: noteId++, kind: "handoff", body: "实现集中在 src/features 下；新增用例覆盖排序回滚与停牌日场景，可直接运行 pnpm vitest run 验证。" });
        if (s.role === "Plan" && i === 0) writes.push({ id: noteId++, kind: "open_question", body: "多端同时修改排序时是否需要冲突提示？暂按最后写入为准。" });
      }
      written.set(s.index, [...(written.get(s.index) ?? []), ...writes]);
      const first = !seen.has(s.index);
      seen.add(s.index);
      const prev = [...d.stages.slice(0, s.index)].reverse().find((x) => x.state !== "skipped");
      return {
        run: `r-${a.id.slice(-6)}`,
        stage: s.index,
        stage_name: s.name,
        status: a.status,
        round,
        attempt: i + 1,
        agents: [
          {
            role: s.role,
            client: a.client,
            model: a.model || null,
            session: `ses_${a.id.slice(-8)}`,
            reads: [...projectReads, ...prior],
            writes,
            handoff: a.status === "succeeded" ? HANDOFFS[s.name] ?? `「${s.name}」已完成，产出见工作区。` : a.status === "failed" ? a.error ?? "" : "",
          },
        ],
        received:
          first && prev && prev.attempts.length > 0
            ? {
                from_stage: prev.index,
                decision: HANDOFFS[prev.name] ?? `「${prev.name}」已完成。`,
                acceptance: ACCEPTANCE[prev.role] ?? "前序阶段产出已确认。",
              }
            : null,
      };
    });
    const merges = d.deliveries
      .filter((x) => x.merged)
      .map((x) => ({ target: x.merged!.target, sha: x.merged!.sha, at: x.merged!.at, files_changed: 4 }));
    const errors = d.id === [...this.works.keys()][0] ? ["读取 r-7f3a 的记忆快照失败：notes.jsonl 第 12 行不是合法 JSON，已跳过该行。"] : [];
    return { rounds, merges, errors };
  }

  handoffs({ project, work }: { project: string; work: string }): ItemHandoff[] {
    this.work(project, work);
    return clone(this.handoffInbox.get(work) ?? []);
  }

  sendHandoff({ project, work, to, note }: { project: string; work: string; to: string; note: string }) {
    const from = this.work(project, work).detail;
    if (to === work) throw new Error("不能交接给自己");
    this.work(project, to);
    const text = note.trim();
    if (!text) throw new Error("交接内容不能为空");
    if (text.length > 500) throw new Error("交接内容最多 500 字");
    const inbox = (this.handoffInbox.get(to) ?? []).filter((h) => h.from !== work);
    inbox.push({ from: work, name: from.name, stage: from.delivered ? "已交付" : from.stages[from.stage]?.name ?? "", note: text });
    this.handoffInbox.set(to, inbox);
  }

  zcodeModels({ refresh }: { refresh?: boolean }): ZCodeModel[] {
    this.zcodeCalls += 1;
    void refresh;
    return [
      { label: "BigModel · GLM-5.3", selection: "bigmodel/glm-5.3@high" },
      { label: "BigModel · GLM-5.3 Air", selection: "bigmodel/glm-5.3-air" },
      { label: "BigModel · GLM-4.6", selection: "bigmodel/glm-4.6" },
      { label: "Kimi · K2 Thinking", selection: "moonshot/kimi-k2-thinking" },
    ];
  }

  reveal({ path }: { path: string }) {
    console.info(`[mock] reveal ${path}`);
  }
}

// ---------- scripts & sample content ----------

const HANDOFFS: Record<string, string> = {
  产品需求: "目标：分组与组内股票都可拖拽排序并跨设备同步。验收以三条标准为准，失败需回滚并提示「排序未保存」。",
  交互设计: "拖拽手柄只在悬停时出现；占位线 2px；键盘 Alt+↑/↓ 可移动。空分组同样可拖。",
  技术方案: "store 新增 reorderGroups（乐观更新 + 失败回滚）；列表用 dnd-kit；持久化走 PUT /v1/preferences/watchlist，防抖 500ms。",
  开发实现: "T1、T2 已完成并有测试覆盖；T3 等接口字段确认。",
  测试验证: "lint、类型检查与单测全部通过。",
  独立验收: "按验收标准逐条核对通过。",
  交付归档: "变更说明已整理并提交。",
};

const ACCEPTANCE: Record<string, string> = {
  Plan: "产出文档已由你确认，作为下一阶段的输入。",
  Build: "实现已确认，测试阶段需覆盖新增路径。",
  Test: "测试通过，进入独立验收。",
  Accept: "验收通过，可以交付。",
};

function genericScript(stage: StageDetail, prompt: string): Entry[] {
  const intro = prompt.trim() ? `收到补充说明：「${prompt.trim().slice(0, 40)}」。` : "";
  switch (stage.role) {
    case "Plan":
      return [
        { kind: "thinking", text: `先读一下项目记忆和前序阶段的产出，确认「${stage.name}」需要回答的问题。` },
        { kind: "command", command: "ls docs/ && cat docs/memory.md | head -40", output: "architecture.md\nmemory.md\nstyle-guide.md\n# 项目记忆\n- 行情数据统一走 quote-gateway 的 WebSocket 通道\n- 前端状态使用 zustand，禁止新增全局单例", exit: 0 },
        { kind: "command", command: "rg -n \"watchlist\" src --type ts | head -20", output: "src/features/watchlist/store.ts:12:export const useWatchlist = create<WatchlistState>()(\nsrc/features/watchlist/List.tsx:8:import { useWatchlist } from './store'", exit: 0 },
        { kind: "message", text: `${intro}我已梳理现有实现，下面是「${stage.name}」的产出：\n\n## 结论\n\n- 范围限定在现有模块内，不引入新依赖\n- 关键风险：多端同步时的顺序冲突\n\n## 待确认\n\n1. 是否需要兼容旧版客户端？` },
        { kind: "edit", files: [`docs/work/${stage.name}.md`] },
      ];
    case "Build":
      return [
        { kind: "thinking", text: "按技术方案拆成三个子任务，先改 store，再改组件，最后补测试。" },
        { kind: "command", command: "git status --short", output: "", exit: 0 },
        { kind: "edit", files: ["src/features/watchlist/store.ts"] },
        { kind: "command", command: "pnpm vitest run src/features/watchlist", output: " ✓ src/features/watchlist/store.test.ts (9 tests) 41ms\n\n Test Files  1 passed (1)\n      Tests  9 passed (9)", exit: 0 },
        { kind: "message", text: `${intro}实现已完成，相关测试通过。` },
      ];
    case "Test":
      return [
        { kind: "command", command: "pnpm lint", output: "✔ No problems found", exit: 0 },
        { kind: "command", command: "pnpm vitest run", output: " Test Files  42 passed (42)\n      Tests  318 passed (318)\n   Duration  8.12s", exit: 0 },
        { kind: "message", text: "全部检查通过：lint 无告警，318 个用例全部通过。" },
      ];
    case "Accept":
      return [
        { kind: "tool", name: "browser", detail: "打开 http://localhost:5173/watchlist", output: "页面已加载，截图已保存", failed: false },
        { kind: "message", text: "逐条核对验收标准，全部满足。" },
      ];
    default:
      return [
        { kind: "command", command: "git log --oneline -5", output: "c1a2b3d 自选股分组支持拖拽排序\n9f8e7d6 补充排序持久化测试", exit: 0 },
        { kind: "message", text: "已整理交付说明并提交。" },
      ];
  }
}

function followUpScript(message: string): Entry[] {
  return [
    { kind: "thinking", text: `用户补充：${message.slice(0, 60)}。需要确认改动范围。` },
    { kind: "command", command: "rg -n \"persistOrder\" src", output: "src/features/watchlist/store.ts:88:  persistOrder(groupId, ids)", exit: 0 },
    { kind: "edit", files: ["src/features/watchlist/store.ts", "src/features/watchlist/store.test.ts"] },
    { kind: "command", command: "pnpm vitest run src/features/watchlist", output: " Test Files  2 passed (2)\n      Tests  12 passed (12)", exit: 0 },
    { kind: "message", text: `好的，已按你的说明调整：\n\n- ${message.slice(0, 80)}\n- 对应测试已更新并通过` },
  ];
}

function fixScript(reason: string): Entry[] {
  return [
    { kind: "thinking", text: `测试阶段反馈：${reason.slice(0, 80)}` },
    { kind: "command", command: "rg -n \"suspended\" src/features/indicators", output: "src/features/indicators/kdj.ts:31:  // TODO suspended days", exit: 0 },
    { kind: "edit", files: ["src/features/indicators/kdj.ts", "src/features/indicators/kdj.test.ts"] },
    { kind: "command", command: "pnpm vitest run src/features/indicators", output: " Test Files  3 passed (3)\n      Tests  27 passed (27)", exit: 0 },
    { kind: "message", text: "已修复：停牌日不再参与 RSV 计算，沿用前一交易日的 K、D 值。新增 2 个停牌场景用例。" },
  ];
}

function memoryDraftFor(d: WorkDetail): string {
  return `【${d.name}】已交付。\n要点：按「${d.stages.map((s) => s.name).join(" → ")}」完成。实现集中在 src/features 下，排序状态持久化到服务端用户偏好接口；后续同类需求优先复用 usePersistentOrder。`;
}

const BUILD_SCRIPT: Entry[] = [
  { kind: "thinking", text: "先确认技术方案里的三个子任务：T1 store 增加 reorder、T2 列表拖拽交互、T3 服务端偏好持久化。从 T1 开始。" },
  { kind: "command", command: "git status --short && git log --oneline -3", output: "c4d5e6f 技术方案：自选股分组排序\n8a9b0c1 交互设计：拖拽手柄与占位\n2d3e4f5 产品需求：自选股分组排序", exit: 0 },
  { kind: "command", command: "rg -n \"groups\" src/features/watchlist/store.ts", output: "14:  groups: WatchGroup[]\n27:  setGroups: (groups) => set({ groups }),\n41:  moveSymbol(from, to) {", exit: 0 },
  { kind: "command", command: "sed -n '1,80p' src/features/watchlist/store.ts", output: Array.from({ length: 40 }, (_, i) => `${String(i + 1).padStart(3)}  ${i % 5 === 0 ? "// ---" : "  const value = state.groups[" + i + "]"}`).join("\n"), exit: 0 },
  { kind: "message", text: "现有 `moveSymbol` 只处理组内移动。我会新增 `reorderGroups(fromIndex, toIndex)`，并让两者共用一个**乐观更新 + 失败回滚**的辅助函数。" },
  { kind: "edit", files: ["src/features/watchlist/store.ts"] },
  { kind: "edit", files: ["src/features/watchlist/reorder.ts"] },
  { kind: "command", command: "pnpm vitest run src/features/watchlist/store.test.ts", output: " ❯ src/features/watchlist/store.test.ts (8 tests | 1 failed) 52ms\n   × reorderGroups > rolls back when persist fails\n     → expected [ 'A', 'B', 'C' ] to deeply equal [ 'B', 'A', 'C' ]\n\n Test Files  1 failed (1)\n      Tests  1 failed | 7 passed (8)", exit: 1 },
  { kind: "thinking", text: "回滚测试失败：快照在乐观更新之后才取，导致回滚到了新顺序。把快照提前到 set 之前。" },
  { kind: "edit", files: ["src/features/watchlist/reorder.ts"] },
  { kind: "command", command: "pnpm vitest run src/features/watchlist/store.test.ts", output: " ✓ src/features/watchlist/store.test.ts (8 tests) 38ms\n\n Test Files  1 passed (1)\n      Tests  8 passed (8)", exit: 0 },
  { kind: "notice", text: "工作区变更较多，Agent 将在本轮结束后自动提交。" },
  { kind: "message", text: "T1 完成。接下来做 T2：给分组标题加拖拽手柄，拖动时显示占位线。" },
  { kind: "edit", files: ["src/features/watchlist/GroupHeader.tsx", "src/features/watchlist/List.tsx", "src/features/watchlist/list.css"] },
  { kind: "tool", name: "apply_patch", detail: "src/features/watchlist/List.tsx (+42 −6)", output: "Success. Updated the following files:\nM src/features/watchlist/List.tsx", failed: false },
  { kind: "command", command: "pnpm tsc --noEmit", output: "", exit: 0 },
  { kind: "command", command: "pnpm vitest run src/features/watchlist", output: " ✓ src/features/watchlist/store.test.ts (8 tests) 36ms\n ✓ src/features/watchlist/List.test.tsx (5 tests) 112ms\n\n Test Files  2 passed (2)\n      Tests  13 passed (13)", exit: 0 },
  { kind: "notice", text: "工作区变更较多，Agent 将在本轮结束后自动提交。" },
  { kind: "message", text: "## 本轮完成\n\n| 子任务 | 状态 |\n| --- | --- |\n| T1 store 排序与回滚 | ✓ |\n| T2 拖拽手柄与占位线 | ✓ |\n| T3 服务端持久化 | 下一轮 |\n\n改动了 6 个文件，13 个用例全部通过。T3 依赖 `PUT /v1/preferences/watchlist`，建议在下一轮确认接口字段后再做。" },
];

const SAMPLE_STAT = ` src/features/export/csv.ts          | 74 ++++++++++++++++++++++
 src/features/export/csv.test.ts     | 38 +++++++++++
 src/features/snapshot/Toolbar.tsx   | 21 +++++--
 src/features/snapshot/useSnapshot.ts| 12 ++--
 4 files changed, 128 insertions(+), 17 deletions(-)`;

const SAMPLE_DIFF = `diff --git a/src/features/export/csv.ts b/src/features/export/csv.ts
new file mode 100644
index 0000000..3f2a1b9
--- /dev/null
+++ b/src/features/export/csv.ts
@@ -0,0 +1,24 @@
+import type { QuoteRow } from "../snapshot/types";
+
+const HEADER = ["代码", "名称", "最新价", "涨跌幅", "成交量", "成交额"];
+
+/** 把行情快照导出为 CSV，带 BOM 以便 Excel 正确识别中文。 */
+export function toCsv(rows: QuoteRow[]): string {
+  const lines = [HEADER.join(",")];
+  for (const r of rows) {
+    lines.push([r.symbol, quote(r.name), r.last, pct(r.change), r.volume, r.turnover].join(","));
+  }
+  return "\\uFEFF" + lines.join("\\r\\n");
+}
+
+function quote(v: string): string {
+  return /[",\\n]/.test(v) ? \`"\${v.replace(/"/g, '""')}"\` : v;
+}
diff --git a/src/features/snapshot/Toolbar.tsx b/src/features/snapshot/Toolbar.tsx
index 81c2d0e..b7a9e44 100644
--- a/src/features/snapshot/Toolbar.tsx
+++ b/src/features/snapshot/Toolbar.tsx
@@ -12,9 +12,17 @@ export function Toolbar({ rows }: Props) {
   const { refresh, loading } = useSnapshot();
-  return (
-    <div className="toolbar">
-      <Button onClick={refresh} disabled={loading}>刷新</Button>
-    </div>
-  );
+  const exportCsv = () => {
+    const blob = new Blob([toCsv(rows)], { type: "text/csv;charset=utf-8" });
+    download(blob, \`snapshot-\${today()}.csv\`);
+  };
+  return (
+    <div className="toolbar">
+      <Button onClick={refresh} disabled={loading}>刷新</Button>
+      <Button onClick={exportCsv} disabled={rows.length === 0}>导出 CSV</Button>
+    </div>
+  );
 }`;

// ---------- seed ----------

function newWork(project: string, name: string, template: Template, kind: "Feature" | "Release", repositories: string[], createdMs: number): MockWork {
  const id = nextId("w").padEnd(10, "0").slice(0, 10);
  const def = TEMPLATES[template];
  const defaults: [Client, string][] = [
    ["Claude", "claude-opus-4-1"],
    ["Claude", "claude-sonnet-4-5"],
    ["Claude", "claude-opus-4-1"],
    ["Codex", "gpt-5-codex"],
    ["Codex", ""],
    ["Claude", "claude-sonnet-4-5"],
    ["Codex", ""],
  ];
  const stages: StageDetail[] = def.stages.map((s, index) => {
    const [client, model] = s.role === "Build" ? defaults[3] : s.role === "Test" ? defaults[4] : defaults[Math.min(index, defaults.length - 1)];
    return {
      index,
      name: s.name,
      role: s.role,
      brief: s.brief,
      state: index === 0 ? "current" : "upcoming",
      skippable: s.skippable,
      skipped_reason: null,
      note: "",
      profile: profile(client, model),
      sandbox: s.role === "Build" || s.role === "Test" ? "Network" : "Standard",
      attempts: [],
      current_attempt: null,
      budget: { limit: null, own: null, used: 0, suggested: null },
      session_preview: null,
    };
  });
  const short = id.slice(1, 9).padEnd(8, "0");
  return {
    project,
    createdMs,
    autopilotRan: new Set(),
    autopilotLoops: 0,
    detail: {
      project,
      id,
      name,
      kind,
      template,
      template_label: def.label,
      stage: 0,
      delivered: false,
      shelved: null,
      repositories: [...repositories],
      branch: `aep-stage/${short}`,
      worktrees: repositories.map((r) => `${HOME}/worktrees/${short}/${r.split("/").pop()}`),
      branches: [{ name: `aep-stage/${short}`, merged: false }],
      stages,
      reworks: [],
      running: null,
      autopilot: { running: false, status: null, message: null, stage: null, until: null },
      deliveries: [],
      memory_draft: null,
      budget: { total_limit: null, total_used: 0, stage_default: null },
    },
  };
}

const PLAN_MESSAGES: Record<string, string> = {
  产品需求: `## 背景\n\n用户在自选股里平均维护 **4.3 个分组**，目前分组顺序只能按创建时间排列，调整需要删除重建。\n\n## 目标\n\n- 分组可以拖拽排序，组内股票也可以拖拽\n- 排序结果跨设备同步\n\n## 验收标准\n\n1. 拖拽分组标题可调整顺序，松手后立即生效\n2. 刷新页面、换设备登录后顺序保持\n3. 网络失败时回滚并提示「排序未保存」`,
  交互设计: `## 交互要点\n\n- 分组标题左侧出现 \`⋮⋮\` 拖拽手柄，悬停时显示\n- 拖动中显示 2px 占位线，其他分组平滑让位\n- 键盘：聚焦手柄后 \`Alt+↑/↓\` 移动\n\n> 空分组也可以拖拽，折叠状态保持不变。`,
  技术方案: `## 方案\n\n| 模块 | 改动 |\n| --- | --- |\n| store | 新增 \`reorderGroups\`，乐观更新 + 失败回滚 |\n| List | 使用 dnd-kit 的 \`SortableContext\` |\n| 持久化 | \`PUT /v1/preferences/watchlist\`，防抖 500ms |\n\n## 子任务\n\n- **T1** store 排序与回滚\n- **T2** 拖拽手柄与占位线\n- **T3** 服务端偏好持久化\n\n## 风险\n\n多端同时修改时以最后写入为准，接口返回 \`version\` 用于冲突提示。`,
};

function planScript(name: string): Entry[] {
  return [
    { kind: "thinking", text: `阅读项目记忆与前序产出，准备「${name}」。` },
    { kind: "command", command: "cat docs/memory.md | head -30", output: "# 项目记忆\n- 行情数据统一走 quote-gateway 的 WebSocket 通道\n- 前端状态使用 zustand", exit: 0 },
    { kind: "command", command: "rg -n \"watchlist\" src --type ts -l", output: "src/features/watchlist/store.ts\nsrc/features/watchlist/List.tsx\nsrc/features/watchlist/GroupHeader.tsx", exit: 0 },
    { kind: "message", text: PLAN_MESSAGES[name] ?? `「${name}」产出已写入 docs/work。` },
    { kind: "edit", files: [`docs/work/${name}.md`] },
  ];
}

function seed(store: MockStore) {
  const now = Date.now();
  const min = 60_000;
  store.projects = [
    { id: "quote", name: "行情终端", repositories: [...REPOS] },
    { id: "tools", name: "内部工具", repositories: ["/Users/demo/code/devtools"] },
  ];
  store.active = "quote";
  store.memoryNotes.set("quote", [
    { id: 9, kind: "decision", author: "陈一", body: "行情数据统一走 quote-gateway 的 WebSocket 通道，前端不直接请求行情源。", created_at: iso(now - 30 * 24 * 60 * min), superseded: false },
    { id: 10, kind: "note", author: "AEP", body: "前端状态管理使用 zustand（替代旧的 redux store）。", created_at: iso(now - 20 * 24 * 60 * min), superseded: true },
    { id: 11, kind: "convention", author: "林舟", body: "前端状态使用 zustand，禁止新增全局单例；跨组件共享通过 store 切片完成。", created_at: iso(now - 12 * 24 * 60 * min), superseded: false, replaces: 10 },
    { id: 12, kind: "delivery", author: "AEP", body: "【旧版分时图下线】已移除 legacy/timeline 目录及其路由；新分时图组件位于 src/features/chart/intraday，埋点事件名保持不变。", created_at: iso(now - 3 * 24 * 60 * min), superseded: false },
  ]);
  store.memoryNotes.set("tools", []);
  store.repoNotes.set(REPOS[0], [
    { id: 1, kind: "convention", author: "林舟", body: "测试命令：pnpm vitest run；E2E 用 pnpm e2e，需要先启动 mock 行情服务（pnpm mock:quote）。", created_at: iso(now - 40 * 24 * 60 * min), superseded: true },
    { id: 2, kind: "convention", author: "林舟", body: "测试命令：pnpm vitest run；E2E 用 pnpm e2e（已内置 mock 行情服务，无需单独启动）。", created_at: iso(now - 9 * 24 * 60 * min), superseded: false },
    { id: 3, kind: "decision", author: "AEP", body: "src/features 下按业务分目录；跨 feature 的共享逻辑放 src/shared，禁止 feature 之间直接 import。", created_at: iso(now - 4 * 24 * 60 * min), superseded: false },
  ]);
  store.repoNotes.set(REPOS[1], [
    { id: 1, kind: "decision", author: "陈一", body: "网关对外只暴露 WebSocket；新增推送频道需在 channels.yaml 注册并补充限流配置。", created_at: iso(now - 15 * 24 * 60 * min), superseded: false },
  ]);

  const add = (w: MockWork) => {
    store.works.set(w.detail.id, w);
    store.order.push(w.detail.id);
  };
  const done = (w: MockWork, i: number, at: number, extra: Partial<Parameters<MockStore["addAttempt"]>[2]> = {}) => {
    const s = w.detail.stages[i];
    store.addAttempt(w, i, { prompt: s.note, script: s.role === "Plan" ? planScript(s.name) : genericScript(s, ""), startMs: at, stepMs: 9000, done: true, ...extra });
    s.state = "done";
  };

  // 1. Running build stage with a streaming transcript.
  const w1 = newWork("quote", "自选股分组支持拖拽排序", "Software", "Feature", REPOS, now - 3 * 60 * min);
  w1.detail.branch = "aep-stage/7f3a9c21";
  w1.detail.stages[0].note = "参考竞品的分组拖拽，重点是跨设备同步。";
  // Same CLI, model and sandbox as 产品需求: 交互设计 and 技术方案 continue its session.
  w1.detail.stages[1].profile = profile("Claude", "claude-opus-4-1");
  done(w1, 0, now - 170 * min);
  done(w1, 1, now - 150 * min);
  done(w1, 2, now - 120 * min);
  store.addAttempt(w1, 2, {
    prompt: "持久化改成服务端用户偏好接口，不要用 localStorage。",
    followUp: true,
    script: [
      { kind: "thinking", text: "用户要求改为服务端持久化，需要更新方案中的持久化一节。" },
      { kind: "edit", files: ["docs/work/技术方案.md"] },
      { kind: "message", text: "已改为 `PUT /v1/preferences/watchlist` 持久化，防抖 500ms，并补充了冲突处理说明。" },
    ],
    startMs: now - 100 * min,
    stepMs: 9000,
    done: true,
  });
  w1.detail.stages[3].state = "current";
  w1.detail.stage = 3;
  w1.detail.stages[3].note = "先做 T1、T2，T3 等接口确认后再做。";
  store.addAttempt(w1, 3, {
    prompt: w1.detail.stages[3].note,
    script: BUILD_SCRIPT.map((e) => ({ ...e })),
    startMs: now - 6 * 1100,
    stepMs: 1100,
    tokens: [84213, 9120],
    result: "T1、T2 完成。",
    subtask: "T1–T2",
    noRollout: true,
  });
  add(w1);

  // 2. Failed test stage with route_back to 开发实现.
  const w2 = newWork("quote", "K 线图叠加 MACD / KDJ 指标", "Software", "Feature", [REPOS[0]], now - 26 * 60 * min);
  w2.detail.branch = "aep-stage/2b81e0d4";
  w2.detail.stages[1].state = "skipped";
  w2.detail.stages[1].skipped_reason = "复用现有指标面板，无新交互";
  // 产品需求 ended with a long session: 技术方案 starts a new one with the hand-off summary.
  done(w2, 0, now - 25 * 60 * min, { lastContext: 182_000 });
  done(w2, 2, now - 24 * 60 * min);
  done(w2, 3, now - 22 * 60 * min);
  w2.detail.reworks.push({ from: 4, to: 3, reason: "MACD 柱状图颜色与涨跌约定相反", at: iso(now - 20 * 60 * min) });
  store.addAttempt(w2, 3, {
    prompt: "「测试验证」退回：MACD 柱状图颜色与涨跌约定相反",
    followUp: true,
    script: [
      { kind: "edit", files: ["src/features/indicators/macd.tsx"] },
      { kind: "message", text: "已改为红涨绿跌，与全站约定一致。" },
    ],
    startMs: now - 19 * 60 * min,
    stepMs: 9000,
    done: true,
  });
  w2.detail.stages[4].state = "current";
  w2.detail.stage = 4;
  store.addAttempt(w2, 4, {
    prompt: "",
    script: [
      { kind: "thinking", text: "按测试职责：lint、类型检查、单元测试、指标回放对比。" },
      { kind: "command", command: "pnpm lint", output: "✔ No problems found", exit: 0 },
      { kind: "command", command: "pnpm tsc --noEmit", output: "", exit: 0 },
      { kind: "command", command: "pnpm vitest run src/features/indicators", output: " ✓ src/features/indicators/macd.test.ts (14 tests) 61ms\n ❯ src/features/indicators/kdj.test.ts (11 tests | 2 failed) 74ms\n   × KDJ > keeps K/D on suspended days\n     → expected 63.21 to be close to 58.94 (±0.01)\n   × KDJ > 00700.HK replay 2024-03\n     → 3 bars differ from reference (first at 2024-03-12)\n\n Test Files  1 failed | 1 passed (2)\n      Tests  2 failed | 23 passed (25)", exit: 1 },
      { kind: "tool", name: "replay", detail: "对比 00700.HK 2024-03 指标回放", output: "2024-03-12  K 63.21 / ref 58.94\n2024-03-13  K 61.02 / ref 57.30\n2024-03-14  K 59.80 / ref 56.11", failed: true },
      { kind: "message", text: "**未通过。** KDJ 在停牌日仍用当日（无成交）数据计算 RSV，导致之后若干根 K 线的 K、D 值偏高。这属于「开发实现」的逻辑问题，需要退回修复。" },
      { kind: "error", text: "2 个用例失败：kdj.test.ts" },
    ],
    startMs: now - 40 * min,
    stepMs: 20_000,
    done: true,
    outcome: {
      status: "failed",
      error: "KDJ 指标在停牌日计算错误（2 个用例失败）",
      route_back: { stage: 3, name: "开发实现", question: "KDJ 在停牌日不应参与 RSV 计算，应沿用前一交易日的 K、D 值。" },
    },
  });
  add(w2);

  // 2b. Deliver stage failed and routed back to 开发实现.
  const w8 = newWork("quote", "自选股显示港股通标识", "Software", "Feature", [REPOS[0]], now - 4 * 24 * 60 * min);
  w8.detail.branch = "aep-stage/4c5d6e7f";
  w8.detail.stages.forEach((s, i) => {
    if (i >= 6) return;
    if (s.skippable) {
      s.state = "skipped";
      s.skipped_reason = "小改动，无需此阶段";
    } else done(w8, i, now - (3 * 24 * 60 - i * 40) * min);
  });
  w8.detail.stages[6].state = "current";
  w8.detail.stage = 6;
  store.addAttempt(w8, 6, {
    prompt: "",
    script: [
      { kind: "command", command: "git diff --stat main...HEAD", output: " src/features/watchlist/Row.tsx  | 14 ++++++--\n src/features/watchlist/badge.css |  9 +++++\n 2 files changed, 20 insertions(+), 3 deletions(-)", exit: 0 },
      { kind: "command", command: "pnpm build", output: "error TS2322: Type 'string | undefined' is not assignable to type 'string'.\n  src/features/watchlist/Row.tsx:42:9", exit: 1 },
      { kind: "message", text: "构建失败：`Row.tsx` 中 `connectFlag` 可能为空。这是实现问题，交付前需要「开发实现」修复。" },
    ],
    startMs: now - 50 * min,
    stepMs: 20_000,
    done: true,
    outcome: {
      status: "failed",
      error: "生产构建失败（类型错误）",
      route_back: { stage: 3, name: "开发实现", question: "Row.tsx 第 42 行 connectFlag 可能为 undefined，需要给出默认值或收窄类型。" },
    },
  });
  add(w8);

  // 3. Autopilot running.
  const w3 = newWork("quote", "盘前盘后行情推送", "Software", "Feature", REPOS, now - 2 * 24 * 60 * min);
  w3.detail.branch = "aep-stage/c09d55e1";
  done(w3, 0, now - 90 * min);
  w3.detail.stages[1].state = "skipped";
  w3.detail.stages[1].skipped_reason = "纯后端推送，无界面变化";
  w3.detail.stages[2].state = "current";
  w3.detail.stage = 2;
  w3.detail.autopilot = { running: true, status: "running", message: "正在运行「技术方案」（3/7）", stage: 2, until: 4 };
  w3.autopilotRan = new Set([2]);
  store.addAttempt(w3, 2, {
    prompt: "",
    script: [...planScript("技术方案"), ...planScript("技术方案").slice(1, 3)],
    startMs: now - 2 * 2500,
    stepMs: 2500,
  });
  add(w3);

  // 4. Fresh item, nothing run yet.
  const w4 = newWork("quote", "港股暗盘报价", "Software", "Feature", REPOS, now - 30 * min);
  w4.detail.branch = null;
  w4.detail.worktrees = [];
  w4.detail.branches = [];
  add(w4);

  // 5. Delivered, not merged yet.
  const w5 = newWork("quote", "行情快照导出 CSV", "Software", "Feature", [REPOS[0]], now - 5 * 24 * 60 * min);
  w5.detail.branch = "aep-stage/5e6f7a8b";
  w5.detail.stages.forEach((s, i) => {
    if (i === 1) {
      s.state = "skipped";
      s.skipped_reason = "按钮放在现有工具栏，无需交互稿";
    } else done(w5, i, now - (4 * 24 * 60 - i * 60) * min, s.role === "Build" ? { legacy: true, maybeHigh: true, tokens: [7_400_000, 61_000] } : legacyRun(i));
  });
  w5.detail.stage = w5.detail.stages.length;
  w5.detail.delivered = true;
  w5.detail.deliveries = [{ run: "r-5e6f", archived_at: iso(now - 2 * 60 * min), merged: null }];
  w5.detail.memory_draft = "【行情快照导出 CSV】已交付。\n导出逻辑位于 src/features/export/csv.ts，输出带 BOM 的 UTF-8 CSV 以兼容 Excel 中文；字段顺序：代码、名称、最新价、涨跌幅、成交量、成交额。后续新增导出格式时复用 toCsv 的转义逻辑。";
  add(w5);

  // 6. Release, delivered and merged.
  const w6 = newWork("quote", "v2.3 发布", "Software", "Release", REPOS, now - 7 * 24 * 60 * min);
  w6.detail.branch = "aep-stage/91ab3c7d";
  w6.detail.branches = [{ name: "aep-stage/91ab3c7d", merged: true }];
  w6.detail.stages.forEach((s, i) => {
    if (s.skippable) {
      s.state = "skipped";
      s.skipped_reason = "发布事项无需此阶段";
    } else done(w6, i, now - (6 * 24 * 60 - i * 30) * min, legacyRun(i + 2));
  });
  w6.detail.stage = w6.detail.stages.length;
  w6.detail.delivered = true;
  w6.detail.deliveries = [{ run: "r-91ab", archived_at: iso(now - 26 * 60 * min), merged: { target: "main", sha: "a1b2c3d4e5f6", at: iso(now - 25 * 60 * min) } }];
  w6.detail.memory_draft = "【v2.3 发布】包含自选股改版、K 线指标优化与 12 项修复；发布分支 release/2.3。";
  add(w6);

  // 7. Archived.
  const w7 = newWork("quote", "旧版分时图下线", "Legacy", "Feature", [REPOS[0]], now - 10 * 24 * 60 * min);
  w7.detail.branch = "aep-stage/0d1e2f3a";
  w7.detail.stages.forEach((_, i) => done(w7, i, now - (9 * 24 * 60 - i * 90) * min, legacyRun(i + 5)));

  // 8. Another project whose runs all predate the minimal launch.
  const w9 = newWork("tools", "构建缓存清理脚本", "Software", "Feature", ["/Users/demo/code/devtools"], now - 12 * 24 * 60 * min);
  w9.detail.branch = "aep-stage/6a7b8c9d";
  [0, 2].forEach((i) => done(w9, i, now - (11 * 24 * 60 - i * 60) * min, legacyRun(i)));
  w9.detail.stages[1].state = "skipped";
  w9.detail.stages[1].skipped_reason = "命令行脚本，无界面";
  w9.detail.stages[3].state = "current";
  w9.detail.stage = 3;
  add(w9);
  w7.detail.stage = w7.detail.stages.length;
  w7.detail.delivered = true;
  w7.detail.deliveries = [{ run: "r-0d1e", archived_at: iso(now - 3 * 24 * 60 * min), merged: { target: "main", sha: "77aa88bb99cc", at: iso(now - 3 * 24 * 60 * min) } }];
  w7.detail.shelved = { at: iso(now - 3 * 24 * 60 * min), memory: 12 };
  w7.detail.worktrees = [];
  w7.detail.branches = [{ name: "aep-stage/0d1e2f3a", merged: true }];
  add(w7);

  store.handoffInbox.set(w1.detail.id, [
    {
      from: w2.detail.id,
      name: w2.detail.name,
      stage: "测试验证",
      note: "指标面板也会复用自选股分组的 store。reorderGroups 的签名请保持 (fromIndex, toIndex)，回滚时不要清空 selectedSymbol，否则 K 线会跳回默认标的。",
    },
  ]);

  // Newest first in the sidebar.
  store.order.sort((a, b) => store.works.get(b)!.createdMs - store.works.get(a)!.createdMs);
  store.selected.set("quote", w1.detail.id);
}

export function createMockApi(): Api {
  return new MockStore();
}
