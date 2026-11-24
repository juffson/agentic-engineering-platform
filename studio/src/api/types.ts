// Contract between the Tauri backend (studio/src-tauri/src/api.rs) and the React UI.
// Every command is invoked as `invoke(name, args)`; args use camelCase keys.

export type Client = "Codex" | "Claude" | "ZCode" | "Custom";
export type Sandbox = "Standard" | "Network" | "Full";
export type Role = "Plan" | "Build" | "Test" | "Accept" | "Deliver";
export type Template = "Software" | "Game" | "Legacy";
export type Group = "active" | "delivered" | "shelved";
/** Evidence status of an attempt as written by the runner. */
export type RunStatus =
  | "initializing"
  | "running"
  | "claimed"
  | "succeeded"
  | "failed"
  | "cancelled"
  | string;

export interface Profile {
  client: Client;
  executable: string;
  model: string;
  check_command: string;
}

export interface WorkSummary {
  id: string;
  name: string;
  kind: "Feature" | "Release";
  group: Group;
  stage: number; // index of current stage; == stage_count when delivered
  stage_count: number;
  stage_name: string; // "已交付" when delivered
  /** Status of the current stage's latest attempt, "" if none. */
  status: RunStatus;
  running: boolean; // an agent is running for this item
  autopilot: boolean; // autopilot is running for this item
}

export interface ProjectSummary {
  id: string;
  name: string;
  repositories: string[];
  work: WorkSummary[]; // newest first
}

export interface Snapshot {
  home: string;
  active: string | null; // active project id
  selected: string | null; // selected work id in the active project
  projects: ProjectSummary[];
  clients: { id: Client; label: string; models: string[] }[];
  sandboxes: { id: Sandbox; label: string }[];
  templates: { id: Template; label: string; stages: string[] }[];
}

export interface AttemptSummary {
  id: string;
  status: RunStatus;
  client: Client;
  model: string; // "" means default
  started_at: string | null;
  finished_at: string | null;
  /** Message that opened this turn: the brief, or the follow-up text. */
  prompt: string;
  follow_up: boolean;
  subtask: string | null; // e.g. "T2 表头"
  error: string | null;
  route_back: { stage: number; name: string; question: string } | null;
  committed: boolean; // platform committed worktree changes after this run
  head_sha: string | null;
}

export type StageState = "done" | "current" | "upcoming" | "skipped";

export interface StageDetail {
  index: number;
  name: string;
  role: Role;
  brief: string; // what the stage is responsible for
  state: StageState;
  skippable: boolean;
  skipped_reason: string | null;
  note: string; // stage note / brief written by the user
  profile: Profile;
  sandbox: Sandbox;
  attempts: AttemptSummary[]; // oldest first
  current_attempt: string | null;
}

export interface Rework {
  from: number;
  to: number;
  reason: string;
  at: string;
}

export interface AutopilotState {
  running: boolean;
  status: string | null; // running | done | stopped | failed
  message: string | null;
  stage: number | null;
  until: number | null;
}

export interface DeliveryInfo {
  run: string;
  archived_at: string;
  merged: { target: string; sha: string; at: string } | null;
}

export interface WorkDetail {
  project: string;
  id: string;
  name: string;
  kind: "Feature" | "Release";
  template: Template;
  template_label: string;
  stage: number;
  delivered: boolean;
  shelved: { at: string; memory: number | null } | null;
  repositories: string[];
  branch: string | null; // aep-stage/<id> when a worktree exists
  worktrees: string[];
  /** The item's local branches; `merged` = another non-item branch contains its tip. */
  branches: { name: string; merged: boolean }[];
  stages: StageDetail[];
  reworks: Rework[];
  running: string | null; // attempt id of the running agent
  autopilot: AutopilotState;
  deliveries: DeliveryInfo[];
  memory_draft: string | null; // only for delivered, unarchived items
}

export type Entry =
  | { kind: "message"; text: string }
  | { kind: "thinking"; text: string }
  | { kind: "command"; command: string; output: string; exit: number | null }
  | { kind: "edit"; files: string[] }
  | { kind: "tool"; name: string; detail: string; output: string; failed: boolean }
  | { kind: "error"; text: string }
  | { kind: "notice"; text: string }
  | { kind: "text"; text: string };

export interface AttemptView {
  id: string;
  status: RunStatus;
  entries: Entry[];
  input_tokens: number;
  output_tokens: number;
  model: string | null;
  error: string | null;
  result: string | null; // final answer text (result.txt)
  /** The ZCode desktop task this attempt dispatched, if any. */
  zcode: { waiting: boolean; tools: { name: string; detail: string }[]; session: string | null; openable: boolean } | null;
  dir: string; // attempt directory, for "reveal"
  workspace: string | null;
}

export interface MemoryNote {
  id: number;
  kind: string;
  author: string;
  body: string;
  created_at: string;
  superseded: boolean;
  /** The older version this note replaced, for project memory version history. */
  replaces?: number | null;
}

export interface MergeBranch {
  name: string;
  status: string; // fast_forward | already_contained | diverged | ...
  reason: string;
  writable: boolean;
  checked_out: string | null;
}

export interface MergePreview {
  target: string;
  branches: MergeBranch[];
  candidate: string;
  stat: string;
  diff: string;
  truncated: boolean;
  digest: string;
  files_changed: number;
  insertions: number;
  deletions: number;
  error: string | null; // when no mergeable candidate exists
}

export interface HistoryNote {
  id: number;
  kind: string; // decision | handoff | review | open_question
  body: string;
}

export interface HistoryAgent {
  role: string;
  client: string;
  model: string | null;
  session: string | null;
  reads: HistoryNote[]; // memory the agent read
  writes: HistoryNote[]; // memory the agent wrote
  handoff: string;
}

export interface HistoryRound {
  run: string;
  stage: number | null;
  stage_name: string;
  status: RunStatus;
  round: number | null;
  attempt: number;
  agents: HistoryAgent[];
  /** Decision carried in from the previous stage, when this run consumed a hand-off. */
  received: { from_stage: number; decision: string; acceptance: string } | null;
}

export interface WorkHistory {
  rounds: HistoryRound[]; // chronological
  merges: { target: string; sha: string; at: string | null; files_changed: number }[];
  errors: string[];
}

export interface ItemHandoff {
  from: string; // work id
  name: string;
  stage: string; // sender's stage name when sent
  note: string;
}

export interface ZCodeModel {
  label: string; // "BigModel · GLM-5.3"
  selection: string; // value to put in profile.model, e.g. "bigmodel/glm-5.3@high"
}

export interface ImportFound {
  repository: string;
  memories: { file: string; title: string; description: string; chars: number }[];
  rules: string[]; // CLAUDE.md / AGENTS.md already in the repo
  claude_sessions: number;
  codex_sessions: number;
  zcode_tasks: number;
  worktrees: {
    path: string;
    branch: string | null;
    ahead: number;
    dirty: number; // changed tracked files
    untracked: number;
    source: "Claude" | "ZCode" | "Git" | string;
  }[];
}

export interface BranchGraph {
  repository: string;
  head: string | null;
  branches: {
    name: string;
    sha: string;
    ahead: number; // vs the repository's checked-out branch
    behind: number;
    current: boolean;
    worktree: { path: string; source: "Claude" | "ZCode" | "AEP" | "Git" | string; changed: number; untracked: number } | null;
    item: { id: string; name: string } | null; // aep-stage/<id> branches
  }[];
  commits: { sha: string; parents: string[]; subject: string; author: string; time: number }[]; // topo order, newest first
  truncated: boolean;
}

/** What a sync changes so the project matches Claude Code memory and the repo's worktrees. */
export interface SyncPlan {
  memories_add: string[]; // file names
  memories_update: string[];
  memories_unchanged: number;
  memories_remove: { id: number; title: string }[]; // retired: kept in history, no longer read
  items_add: { branch: string; path: string }[];
  items_keep: { id: string; name: string; reason: string }[];
  items_remove: { id: string; name: string; reason: string }[]; // never ran
  items_stale: { id: string; name: string; reason: string }[]; // ran, left for a person
}

/** Commands. Each returns a Promise of the given type; errors reject with a message string. */
export interface Api {
  snapshot(): Snapshot;
  openProject(a: { project: string }): void;
  selectWork(a: { project: string; work: string }): void;
  createProject(a: { name: string; repositories: string[] }): string; // new project id
  addRepository(a: { project: string; path: string }): void;
  createWork(a: {
    project: string;
    name: string;
    template: Template;
    release: boolean;
    repositories: string[];
  }): string; // new work id
  workDetail(a: { project: string; work: string }): WorkDetail;
  attemptView(a: { project: string; work: string; attempt: string }): AttemptView;
  /** Save any of note/profile/sandbox for a stage. */
  updateStage(a: {
    project: string;
    work: string;
    stage: number;
    note?: string;
    profile?: Profile;
    sandbox?: Sandbox;
  }): void;
  /** Run the current stage. If it is being re-entered after a send-back, the backend resumes
   *  its own session with a re-check message; `message` is appended to that as extra notes. */
  runStage(a: { project: string; work: string; stage: number; message?: string }): string; // attempt id
  followUp(a: { project: string; work: string; stage: number; message: string }): string;
  cancel(a: { project: string; work: string; attempt: string }): void;
  /** Confirm the current stage and move to the next. */
  advance(a: { project: string; work: string; stage: number }): void;
  /** Send the item back from `from` to `to`, and start the fix in `to` immediately. */
  sendBack(a: { project: string; work: string; from: number; to: number; reason?: string }): string;
  waive(a: { project: string; work: string; stage: number; reason: string }): void;
  skip(a: { project: string; work: string; stage: number; reason: string }): void;
  reopen(a: { project: string; work: string; stage: number }): void;
  autopilotStart(a: { project: string; work: string; until: number }): void;
  autopilotStop(a: { project: string; work: string }): void;
  archive(a: { project: string; work: string; memory: string; removeWorktree: boolean }): void;
  unarchive(a: { project: string; work: string }): void;
  /** Save a new version of a project memory note (agents read only the newest); restoring an
   *  old version is revising the newest with its text. Returns the new note id. */
  reviseMemory(a: { project: string; id: number; text: string }): number;
  /** Branches, worktrees and the recent commit graph of one of the project's repositories. */
  branchGraph(a: { project: string; path: string; limit: number }): BranchGraph;
  /** Installed git tools (Fork, VS Code, lazygit…) plus plugins from `config`. */
  gitTools(): { tools: { id: string; label: string }[]; config: string };
  openGitTool(a: { id: string; path: string }): void;
  /** Take a project out of Studio (its repositories are untouched). */
  removeProject(a: { project: string }): void;
  /** What Claude Code, Codex and ZCode already hold for a repository folder. */
  importScan(a: { path: string }): ImportFound;
  /** Import chosen Claude memory files (by file name) and existing branches (each becomes an
   *  item starting at 开发实现) into a project. */
  importApply(a: {
    project: string;
    path: string;
    memories: string[];
    branches: { branch: string; path: string }[];
  }): { notes: number; items: string[] };
  /** Preview a sync of one repository's Claude memory and worktree branches. */
  importSyncPlan(a: { project: string; path: string }): SyncPlan;
  /** Apply it: add/update/retire imported memory, create/remove imported items. */
  importSync(a: { project: string; path: string; memories: boolean; items: boolean }): SyncPlan;
  /** System folder chooser; a folder inside a git repo resolves to its root. null = cancelled. */
  pickFolder(a: { prompt: string }): string | null;
  /** Delete an item that never ran (no attempts, no worktree). */
  deleteWork(a: { project: string; work: string }): void;
  /** Bring up the ZCode desktop task an attempt dispatched (e.g. to approve a tool). */
  zcodeOpen(a: { project: string; work: string; attempt: string }): void;
  /** Remove a delivered item's worktrees and merged branches; `force` also deletes unmerged ones. */
  cleanup(a: { project: string; work: string; force: boolean }): { worktrees: number; deleted: string[]; kept: string[] };
  mergePreview(a: { project: string; work: string; target?: string }): MergePreview;
  mergeConfirm(a: { project: string; work: string; target: string; digest: string }): void;
  memories(a: { project: string }): MemoryNote[];
  addMemory(a: { project: string; text: string }): number;
  reveal(a: { path: string }): void;
  /** Every run of the item across stages, with memory reads/writes. */
  history(a: { project: string; work: string }): WorkHistory;
  /** Hand-offs other items sent to this one. */
  handoffs(a: { project: string; work: string }): ItemHandoff[];
  /** Send (or replace) a short note (1–500 chars) from `work` to another item `to`. */
  sendHandoff(a: { project: string; work: string; to: string; note: string }): void;
  /** Models the local ZCode app offers; slow on first call (starts ZCode), then cached. */
  zcodeModels(a: { refresh?: boolean }): ZCodeModel[];
  /** Repository-scoped shared memory (stages read it too). Notes reuse MemoryNote. */
  repoMemories(a: { path: string }): MemoryNote[];
  addRepoMemory(a: { path: string; text: string }): number;
}
