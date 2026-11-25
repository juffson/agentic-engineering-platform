# 技术方案：Token 优化 · 精简阶段基础上下文与重复读取

依据：`spec.md`（R1–R9、E1–E12、AC-1–AC-14）、`interaction.md`（§2–§8）。本文确定仓库边界、接口、数据、变更文件、兼容性和测试计划；开发按 `task.json` 的 T1–T8 顺序进行。

## 0. 仓库边界

| 位置 | 是否改动 | 说明 |
|---|---|---|
| `src/`（`aep` 库与 `aep` 运行器） | 改 | 启动参数、会话复用判定、用量计数、提示词 |
| `studio/src-tauri/`（Tauri 后端 API） | 改 | 新字段、对比统计、续接上限设置 |
| `studio/src/`（React 前端） | 改 | 界面按 `interaction.md` |
| `tests/`、`studio/src-tauri/tests/`、`studio/tests/` | 改 / 新增 | 集成、API、UI 测试 |
| `desktop/`（GPUI，旧版） | **不改** | 决策 #58 |
| ZCode 桌面任务通道（`src/zcode_desktop*.rs`） | **不改** | R7 结论见 §6 |
| 推送 / 合并 / 发布 | 不做 | |

## 1. 实测核实（§8 与 PRD「待技术方案确认」）

全部在本机实测，Claude Code 2.1.331、codex-cli 0.172.0，2026-11-24。

### 1.1 Claude 最小启动参数（待确认 1）

在一个带项目 `CLAUDE.md`（含标记词）的临时 git 目录中，用 `-p --output-format stream-json --verbose --tools Read,Grep,Glob,Bash` 加以下参数运行，读取 init 事件和首个 assistant 事件的 usage：

| 参数组合 | MCP | 工具 | skills | 插件 | `memory_paths` | 项目 CLAUDE.md | 用户 CLAUDE.md（CodeGraph 段） | 首轮上下文 |
|---|---|---|---|---|---|---|---|---|
| 现状（取自本项目真实执行 2c29ee9b…） | 32 | 342 | 81 | 9 | — | 有 | 有 | 284K |
| `--strict-mcp-config --setting-sources project,local --disable-slash-commands` | 0 | 4 | 0 | 5（均为 `@builtin`） | 有（自动记忆仍开） | 有 | **无** | 7.5K |
| 上一行 + `--settings '{"autoMemoryEnabled":false}'` | 0 | 4 | 0 | 5（`@builtin`） | **无** | 有 | 无 | 3.6K（极短问题） |
| 上一行的环境变量等价 `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` | 0 | 4 | 0 | 5 | 无 | 有 | 无 | 6.8K |

结论：

- **选用组合**（集中在 `src/launch.rs::claude_minimal()`，所有 Claude 调用共用）：
  `--strict-mcp-config --setting-sources project,local --disable-slash-commands --settings {"autoMemoryEnabled":false}`。
  不传 `--mcp-config`（E1：项目 `.mcp.json` 同样不加载）。
- `--setting-sources project,local` 已经让用户级 `~/.claude/CLAUDE.md` 不再加载（实测问答确认），不需要别的手段。
- 不用 `--bare`：认证只认 `ANTHROPIC_API_KEY` / apiKeyHelper，OAuth 登录的用户会失败。
- 不用 `--safe-mode`：会同时关掉项目 CLAUDE.md、项目权限。
- 不用 `--restricted`：会去掉 Bash 并忽略项目设置。
- 剩下的 5 个插件 `cc-plugin-sec-default / agents-md / telemetry / privacy / plugin-authoring` 都是 `@builtin`，无法关闭，按 AC-1 写进 evidence 的 `launch.builtin_plugins`。
- 自动记忆选 `--settings` 而不是环境变量：参数出现在 argv 中，单元测试可直接断言，且 `logged()` 现在不传额外环境变量。

**用户级设置不再生效的影响（E2）**：本机 `~/.claude/settings.json` 的 `permissions.allow` 只有 `mcp__codegraph__*` 和两个个人 skill 脚本，阶段都不依赖；`hooks`、`enabledPlugins` 正是要去掉的。**唯一会改变行为的是 `model` 键**：阶段模型留空时，原来用用户设置里的默认模型（本机为 `opus`），最小启动后会变成 Claude Code 内置默认。处理：`profile.model` 为空时，`claude_minimal()` 只读取 `~/.claude/settings.json` 的 `model` 字符串，有值就显式传 `--model`，evidence 写 `launch.model_source: "user_settings"`。（`Project::reserve` 已经先用项目预设模型填空值，这里只兜底。）

**版本过旧（E3）**：`claude_minimal(bin)` 首次调用时运行一次 `<bin> --help`，按「可执行文件路径 + 修改时间」缓存在进程内；`--help` 中找不到的参数不传，写进 `launch.not_minimized: ["--disable-slash-commands", …]`，`launch.minimal = not_minimized.is_empty()`。`--help` 本身失败时按全部支持处理（避免误判），并记录 `launch.probe_error`。

### 1.2 Claude 续接相关

- `--resume <id>` 时 `result.usage` 和 stdout 里的 assistant 事件**只含本次调用**（核对 d0b27203 的两次复核：result 总量 ≈ 本次各调用之和，首轮上下文 284K 来自被续接的历史）。所以 Claude 的「本阶段窗口用量」天然成立（AC-8）。
- `--resume <id> --fork-session`：生成新会话 ID，历史一致，**缓存仍命中**（实测首个调用 `cache_read 3785 / cache_creation 116`）。相邻阶段复用一律加 `--fork-session`，每个阶段有自己的会话 ID，前一阶段日后「继续对话」不会混入后面阶段的轮次。
- `--add-dir` 在续接时可以追加（实测续接时新加的目录在 init `additional_directories` 中，且能读取）。待确认 4 的结论：**每个阶段按自己的需要传 `--add-dir`（目前只有验收 / 交付的前序回执目录），续接时照常追加，不需要随会话保留。**

### 1.3 Codex resume 用量语义（R4，待确认 2）

对本机 7 次 `exec resume` 执行（a1236f34、7e8d7916 两个事项），逐一对比 stdout 的 `turn.completed.usage` 和 rollout（`~/.codex/sessions/**/<thread>.jsonl`）：

| 执行 | turn.completed input | rollout 中本次窗口前的 `total_token_usage.input` | 窗口末 `total_token_usage.input` | 窗口内 `last_token_usage` 之和 | 调用数 |
|---|---|---|---|---|---|
| b578ba7c | 7,337,462 | 7,161,016 | 7,337,462 | 176,446 | 1 |
| 601da926 | 3,087,261 | 2,712,902 | 3,087,261 | 374,359 | 3 |
| 6a7d2d61 | 12,169,148 | 7,337,462 | 12,169,148 | 4,831,686 | 23 |

结论：**`exec resume` 的 `turn.completed.usage` 是整条线程的累计值**（等于 rollout 窗口末的 `total_token_usage`），本次真实用量 = 窗口内 `last_token_usage` 之和 = 累计值之差，三者完全吻合。PRD 里「1 次调用记了 7.3M」就是 b578ba7c。

修正规则（`src/usage.rs::codex`）：

1. 有 rollout：**一律**以窗口内 `last_token_usage` 之和为准（不再只在没有 `turn.completed` 时才用）。同一事项的执行由 `workflow.lock` 串行，窗口内不会混入别的执行（AC-9 误差 0）。
2. 没有 rollout，但本次是新线程：`turn.completed` 之和（与现状相同）。
3. 没有 rollout，且本次是续接（`follow_up.resumed_session` 或 `reused_session` 非空）：用 `turn.completed` 累计值减去来源执行记录的 `codex_session_total`（新增字段，见 §3.3）；来源没有这个字段（历史记录）时保留原值并标 `count_maybe_high: true`（E11）。

Codex 与 `--strict-mcp-config` 等价的手段：本次不做（PRD：首轮约 20K，不是重点）。

### 1.4 ZCode 能否继续（R7，§8-4）

ZCode 内置 app-server（`/Applications/ZCode.app/Contents/Resources/glm/zcode.cjs`）的协议里有 `session/resume`，参数 `{sessionId, workspace?, …}`（schema `ifn`，从会话库加载已有会话），之后可以照常 `session/subscribe` + `session/send`。结论：

- **app-server 通道（普通 CLI 模型）：支持继续。** 被预算停下或继续对话时，用原 `session.json` 里的 `session` 走 `session/resume` + `session/send`，只发新消息。
- **桌面任务通道（桌面套餐模型，`zcode_desktop`）：本次不支持**，保持「追加预算并重跑」并在界面说明（`interaction.md` §4.7 第二行）。
- ZCode **不参与**相邻阶段复用（R3 只对 Codex / Claude）。

### 1.5 续接上限的位置与默认值（待确认 3、§8-2）

- **项目级**：`Project.resume_limit: Option<u64>`，空值 = 默认 150K，下限 20K，与 `stage_budget` 同卡设置（`interaction.md` §3.2 用「项目」文案）。
- 150K 合适：最小启动后首轮约 7–15K，一个阶段正常结束时多在 60–120K；超过 150K 后每次调用都要重读 ≥150K，开新会话 + 交接摘要（约 5–10K）更省。
- 下限 20K：低于这个值几乎每次都会开新会话，复用失去意义。

### 1.6 会话预告能否廉价算出（§8-1）

能。判定只需读：当前阶段的配置、前一个实际运行阶段的 `request.json` / `evidence.json` 和它的「最后一次调用上下文」（新记录在 evidence 的 `last_context`，旧记录从已缓存的 `usage.json` 取，见 §3.3）。不启动 CLI，不读会话文件。运行器和 Studio API 调用同一个纯函数 `aep::session_reuse::decide`，保证预告与实际一致（实际运行时多一步 E4 回退）。

## 2. 方案概览

```
reserve(stage)                         运行器 perform()                            invoke()
  ├─ request.resume_limit ← 项目设置       ├─ 有 follow_up？                              ├─ Claude: launch::claude_minimal(bin, model)
  └─ request.prior_runs（已有）             │    └─ session_reuse::decide(Continue)  ──┐     │          + --resume <sid> [--fork-session]
                                           ├─ 否则 Codex/Claude：                      │     ├─ Codex: exec resume <sid>（已有）
                                           │    └─ session_reuse::decide(Adjacent) ──┤     └─ 记录 launch / reused_session /
                                           ├─ Plan::Resume{sid,fork,source} → 短提示词  │        fresh_session_reason / last_context /
                                           └─ Plan::Fresh{reason} → 完整提示词 + 交接摘要 ┘        codex_session_total
```

## 3. 接口与数据

### 3.1 新模块 `src/launch.rs`（库，`pub mod launch;`）

```rust
pub struct ClaudeLaunch {
    pub args: Vec<String>,          // 追加到 claude 命令行的参数
    pub minimal: bool,
    pub not_minimized: Vec<String>, // E3
    pub model_source: Option<&'static str>, // "user_settings" 或 None
}
/// `model`：profile.model（可空）。只读 `<bin> --help`（缓存）和 ~/.claude/settings.json 的 "model"。
pub fn claude_minimal(bin: &Path, model: &str) -> ClaudeLaunch;
/// 从 stream-json 的 init 和首个 assistant 事件取 AC-1 需要的数字。
pub fn claude_init_stats(stdout: &str) -> Option<serde_json::Value>;
// → {"mcp_servers":0,"mcp_tools":0,"tools":4,"skills":0,"plugins":5,
//    "builtin_plugins":["cc-plugin-sec-default",…],"first_context":9800,"version":"2.1.331"}
pub const DIET: u32 = 1; // 「优化后」标记版本
```

- `mcp_tools` = init `tools` 中以 `mcp__` 开头的个数。
- 使用方：`workflow_stage::invoke`（Claude 分支）、`memory_tidy::ask`（Claude 分支，同时给 `CLI 起草新事项` 用）、`agent.rs` 的独立审查（保留已有 `--tools "" --no-session-persistence`，追加最小参数）。

### 3.2 新模块 `src/session_reuse.rs`（库）

```rust
pub enum Mode { Continue, Adjacent }   // 本阶段继续对话 / 相邻阶段复用
pub struct Side<'a> {                  // 一次执行的可比较属性
    pub client: Client, pub model: &'a str, pub sandbox: Sandbox,
    pub permission: &'static str,      // "read" | "write" | "gate"（由阶段角色推出，同 perform 的 write/gate）
    pub workspace: Option<&'a Path>,
}
pub struct Source<'a> {                // 被续接的那次执行
    pub stage: usize, pub stage_name: &'a str, pub run: &'a str,
    pub side: Side<'a>, pub status: &'a str,
    pub session: Option<&'a str>, pub resumable: bool,
    pub last_context: Option<u64>,
}
pub enum Plan {
    Resume { session: String, fork: bool, source_stage: Option<usize>, source_run: String, context: Option<u64> },
    Fresh  { reason: Reason, context: Option<u64> },
}
#[serde(rename_all = "snake_case")]
pub enum Reason { NoPrior, ContextOverThreshold, ClientDiffers, ModelDiffers, PermissionDiffers,
                  WorkdirDiffers, SessionUnavailable, PriorBudgetStopped, SessionBusy,
                  ChainBroken, ClientNotReusable, PriorNotPassed }
pub fn decide(mode: Mode, current: &Side, source: Option<&Source>, threshold: u64) -> Plan;
pub const DEFAULT_LIMIT: u64 = 150_000; pub const MIN_LIMIT: u64 = 20_000;
/// 从执行目录读出 Source（request.json + evidence.json + last_context）；给运行器和 API 共用。
pub fn source(runs: &Path, stage: usize, stage_name: &str, run: &str) -> Result<Source<'static>>;
```

判定顺序（第一条命中即返回）：

1. 当前 CLI 不是 Codex / Claude → `ClientNotReusable`（界面不显示，`interaction.md` §2.1）。
2. 没有来源 → `NoPrior`。
3. `Adjacent` 时：来源 CLI 不同 → 来源阶段是被跳过阶段之前的、CLI 不同的阶段时 `ChainBroken`，否则 `ClientDiffers`（E6：`prior_runs` 本来就不含被跳过的阶段，取 `prior_runs.last()` 即「再前一个实际运行的阶段」）；模型（`trim` 后相等，空 = 空）不同 → `ModelDiffers`；`sandbox` 或 `permission` 不同 → `PermissionDiffers`（E8）；工作目录不同 → `WorkdirDiffers`。
4. 来源状态：`budget_exhausted` → `PriorBudgetStopped`（E9，仅 `Adjacent`；`Continue` 时被预算停下正是要继续的场景，放行）；`running`/`initializing` → `SessionBusy`（E10）；`Adjacent` 且不是 `succeeded` → `PriorNotPassed`。
5. 没有会话或 `resumable != true` → `SessionUnavailable`。
6. `last_context > threshold` → `ContextOverThreshold`（R2；`last_context` 未知时不拦）。
7. 否则 `Resume { fork: client == Claude && mode == Adjacent, … }`。

E7（送回后重跑）：`prior_runs` 取的是 `current_attempt`，即该阶段最新一次执行；它必须 `succeeded` 才复用，被送回的那次不会是 `current_attempt` 的最终值。
E10 补充：同一事项的 CLI 由 `workflow.lock` 串行，不同事项不共享会话，所以 `SessionBusy` 只在异常残留时出现。

### 3.3 `evidence.json` 新字段（`interaction.md` §6 定稿）

| 字段 | 类型 | 何时写 | 说明 |
|---|---|---|---|
| `launch` | object | Codex / Claude 每次执行 | `{"diet":1,"minimal":bool,"not_minimized":[],"model_source":null,"mcp_servers":0,"mcp_tools":0,"tools":4,"skills":0,"plugins":5,"builtin_plugins":[…],"first_context":9800}`；Codex 只有 `diet` 和 `first_context`。`diet` 是 R6 的分界标记 |
| `reused_session` | object\|null | 相邻阶段复用时 | `{"stage":0,"name":"产品需求","run":"f41a…","session":"…","forked":true}` |
| `fresh_session_reason` | string\|null | 开新会话时 | §3.2 的 `Reason`（snake_case） |
| `resume_context` / `resume_threshold` | number | 做过阈值判断时 | 被续接会话最后一次调用的上下文 / 当时阈值 |
| `follow_up.resumed_session` | 已有 | | 不变 |
| `last_context` | number | 结束时 | 本次最后一次调用的上下文，供下次判定 |
| `usage_window` | object | 结束时 | `{"calls":21}`，本次窗口内调用数（AC-8） |
| `codex_session_total` | object | Codex 结束时 | 最后一个 `turn.completed.usage` 原样（累计值），供下次续接做差 |

`usage.json`（`usage::Summary` 缓存）新增 `rev: u32`（当前 2）、`first_context: Option<u64>`、`count_maybe_high: bool`。`usage::summary()` 读到 `rev < 2` 的缓存时重算一次（一次性，53 次执行成本可忽略），这同时完成 E11 的历史重算。

### 3.4 提示词

`src/handoff.rs` 拆出三段：`header(req)`（项目 / 阶段 / 职责 / 本次说明 + `stage_transfer.context()`）、`memory_body(...)`（已确认决策、项目记忆、仓库记忆、事项交接、前序执行行）、`rules(req, stage)`（「以当前决策为准…AEP-HANDOFF…specs 规则…验收命令」）。

- 新会话：`header + memory_body + rules + READING`（与现状相同，多一段读取约定）。
- 相邻复用（R3）：`header + rules + READING + 「本次档案 <dir>」`，不含记忆正文和前序产出；首行仍是 `阶段：<名>（…）`，方便测试替身识别。`stage_transfer.context()` 是上一阶段的交接决策摘要（约 0.5K），保留，因为它可能含人工结论。
- 超阈值 / 会话丢失改新会话（R2、E4、E5）：`header + memory_body + rules + READING + "## 上一轮答复"（上一轮 result.txt，前 8000 字）+ "## 交接摘要"（来源执行的 AEP-HANDOFF 行和 evidence.json 路径）`。继续对话时再加「## 继续的要求」（现有）。
- 继续对话且照常 resume（现有）：只发消息，不变。

`READING`（R5，`pub const READING: &str`）：

```
读取约定：先用 rg 或 codegraph 定位，再按行范围读取（Read 的 offset/limit 或 sed -n 'a,bp'）；不要 cat 整个大文件；命令输出可能很长时用 head/tail 截断，或写到文件后按需读取。
```

### 3.5 `Request` / `Project` 新字段

- `workflow::Request.resume_limit: Option<u64>`（`serde(default)`），`Project::reserve` 写入 `project.resume_limit.unwrap_or(DEFAULT_LIMIT)`。
- `project::Project.resume_limit: Option<u64>`（`serde(default, skip_serializing_if = Option::is_none)`）。
- 旧 `request.json` 没有该字段 → 运行器用默认 150K。

### 3.6 ZCode 继续（R7）

- `aep zcode-stage` 新增可选参数 `--resume <session>`（`src/main.rs`）。
- `zcode_stage::app_server`：有 `resume` 时发 `session/resume {sessionId, workspace}` 代替 `session/create`，其余（subscribe / send / 回执）不变；`session/resume` 返回错误时向 stderr 写 `zcode session resume failed` 并以非零退出。
- `workflow_stage::perform`：`Continue` 模式下，若客户端是 ZCode 且上一轮 `native_session.transport == "app-server"`，把会话作为 resume 传入；`session_missing()` 增加匹配 `zcode session resume failed`，失败时走已有的「完整上下文 + 上一轮答复」回退。桌面通道照旧。
- evidence：`follow_up.resumed_session` 同 Claude / Codex；API 的 `attempt_summary` 增加 `continuable: bool`（Claude / Codex 恒真；ZCode 仅 app-server 有会话时为真），前端据此选 §4.7 的哪一行。

### 3.7 Studio API（`studio/src-tauri/src/api.rs` + `studio/src/api/types.ts`）

| 接口 | 变化 |
|---|---|
| `attempt_summary` → `AttemptSummary` | 加 `session: {kind:"reused"\|"continued"\|"fresh", stage?:number, stage_name?:string, run?:string, reason?:Reason, context?:number, threshold?:number, prior?:string} \| null`；`launch: Launch \| null`；`continuable: boolean`；`count_maybe_high: boolean` |
| `work_detail` → `StageDetail` | 加 `session_preview: {kind:"reused"\|"fresh", stage_name?, reason?, context?, threshold?, prior?} \| null`（仅当前阶段、无运行中执行时，按 §1.6 计算）；`budget.suggested: {value:number, runs:number, median:number} \| null`（R9） |
| `attempt_view` → `AttemptView` | 加 `live_usage: boolean`（Codex 且找不到 rollout 时为 false，R8）；`launch`、`usage_window` |
| `usage_analysis` → `UsageAnalysis` | 加 `launch`、`reused: boolean`、`window_calls: number` |
| `project_usage` → `ProjectUsage` | 加 `resume_limit: number\|null`；`compare: {by_stage: CompareRow[], by_model: CompareRow[]}`，`CompareRow = {key, before: Side, after: Side, change: number\|null}`，`Side = {runs, first_context_median: number\|null, avg_calls: number\|null, avg_total: number\|null}`；`runs[]` 每行加 `diet: boolean`、`reused_from: string\|null`、`count_maybe_high: boolean`；`stages/items` 等汇总不变，另给 `maybe_high_runs: number` |
| 新命令 `set_resume_limit(project, tokens: string)` | 解析同 `set_stage_budget`（支持 `150k`），空 = 恢复默认，`< 20K` 报错「续接上限至少 20K」 |

对比计算（R6 / AC-11）：遍历项目内所有 Codex / Claude 执行（跳过 `mode == external`），`launch.diet` 存在归「后」，否则归「前」；`first_context` 为空的执行（E12）不计入首轮中位数和平均值；`change = after.avg_total / before.avg_total − 1`，任一侧无样本时为 null。建议值（R9）：同项目、同阶段名、`diet` 存在且 `succeeded` 的执行总量中位数 × 1.5，向上取整到 10K。

### 3.8 前端（`studio/src/`）

按 `interaction.md` 实现，文件：

- `lib/format.ts`：`sessionLabel(session)`、`launchLabel(launch)` 两个格式函数，所有位置共用（§2.1、§2.2）。
- `components/Usage.tsx`：对比卡（§3.1）、续接上限输入（§3.2）、「计数可能偏高」tag（§3.3）、运行行标签（§3.4）。
- `components/Budget.tsx`：`UsageBreakdown` 启动配置行（§4.3）、`StageBudget` 建议值与「采用」（§4.5）、`ExtendBudget` 按 `continuable` 切换文案（§4.7）。
- `components/Transcript.tsx`：`turn-meta` 会话标签、复用来源链接（§4.2）；`TurnStatus` Codex 无会话日志说明（§4.6）。
- `components/WorkPage.tsx`：阶段条「↻ 复用」（§4.4）。
- `components/StageConfig.tsx`：会话预告（§4.1）。
- `components/DecisionPanel.tsx`：ZCode 不支持继续时的说明（§4.7）。
- `components/AutopilotDrawer.tsx`：阶段行「↻ 复用」/ 预告关键字（§5）。
- `api/client.ts`、`api/types.ts`、`api/mock.ts`：新字段与 `setResumeLimit`；mock 提供四种对比卡状态和各类会话标签的样例。
- `styles/app.css`：新增的几个 class（复用现有色板，不新增颜色）。

## 4. 变更文件清单

| 文件 | 子任务 | 变化 |
|---|---|---|
| `src/launch.rs`（新） | T1 | 最小启动参数、`--help` 探测缓存、init 统计 |
| `src/lib.rs` | T1、T4 | `pub mod launch; pub mod session_reuse;` |
| `src/workflow_stage.rs` | T1、T3、T4、T5 | Claude 拼参数改用 `claude_minimal`；记录 `launch`、`last_context`、`usage_window`、`codex_session_total`；`perform` 接入 `session_reuse::decide`；ZCode resume |
| `src/memory_tidy.rs` | T1 | `ask` 的 Claude 分支加最小参数 |
| `src/agent.rs` | T1 | 独立审查加最小参数 |
| `src/handoff.rs` | T2 | 拆 `header / memory_body / rules`，`READING`，复用短提示词、交接摘要提示词 |
| `src/usage.rs` | T3 | Codex 窗口计数、续接做差、`Summary.rev / first_context / count_maybe_high` |
| `src/session_reuse.rs`（新） | T4 | 判定函数与 `source()` |
| `src/workflow.rs` | T4 | `Request.resume_limit` |
| `src/project.rs` | T4 | `Project.resume_limit`，`reserve` 写入请求 |
| `src/zcode_stage.rs`、`src/main.rs` | T5 | `session/resume`、`--resume` 参数 |
| `tests/stage_token_diet.rs`（新） | T6 | 运行器端到端（测试替身 claude / codex） |
| `studio/src-tauri/src/api.rs`、`studio/src-tauri/src/lib.rs`、`studio/src-tauri/tests/api.rs`、`studio/src/api/types.ts`、`studio/src/api/client.ts` | T7 | API 字段、对比、`set_resume_limit` |
| `studio/src/**`（见 §3.8）、`studio/tests/ui.spec.ts` | T8 | 界面与 UI 测试 |

## 5. 兼容性

- **旧执行记录**：所有新字段都可缺省。缺 `launch` → 「优化前」；缺会话字段 → 不显示标签；缺 `last_context` → 从 `usage.json`（rev 2 重算后有 `per_call` 末项）取，再缺就不做阈值拦截。
- **`usage.json` 重算**：rev 变化只触发一次；Codex 历史执行总量会**下降**（例如 b578ba7c 7.3M → 0.18M），项目 / 事项 / 阶段已用预算随之变小——这是更正，不是回归；`budget.json`（被停当时的记录）不改。无 rollout 的历史续接标 `count_maybe_high`，原始数值保留。
- **`request.json`**：旧请求无 `resume_limit` → 默认 150K。新字段 `serde(default)`，旧版运行器读新请求会忽略未知字段（`Request` 未设 `deny_unknown_fields`）。
- **Claude Code 版本**：参数探测保证旧版本照常运行（E3）。
- **用户设置**：hooks、用户插件、用户 MCP、用户 CLAUDE.md、自动记忆不再影响阶段；模型默认值通过 §1.1 兜底保持不变。需要 MCP 的个人工作流不受影响（只影响 AEP 启动的子进程）。
- **会话文件**：复用不删除、不修改旧会话；Claude 复用用 fork，旧会话保持原样。Codex 无 fork，后续阶段会接在同一线程上——因此前一阶段日后「继续对话」时，Codex 线程里会包含后续阶段的轮次（沙箱、权限相同，已由判定保证），可接受；计数按窗口，不受影响。
- **自动运行**（S9）：走同一 `perform`，无额外交互。
- **GPUI `desktop/`**：不改；它不读新字段，也不受影响。

## 6. 风险与回退

| 风险 | 处理 |
|---|---|
| 某阶段依赖用户级 MCP（如 codegraph MCP） | 读取约定改用 `codegraph` CLI；阶段失败时 evidence 有 `launch`，便于定位 |
| fork 后首个调用缓存未命中（缓存 1h 过期） | 只多一次 cache write，仍远小于 256K 基础上下文；不做特殊处理 |
| 复用后阶段质量下降（模型带着前序推理） | 门禁、验收、交接流程不变（AC-5）；若操作者不需要复用，可给相邻阶段设不同模型 / 沙箱（预告会说明） |
| Codex rollout 搜索慢 | 已有 `rollout.path` 缓存，不变 |
| ZCode `session/resume` 在新版本改名 | 失败走现有回退，阶段不失败 |

## 7. 测试计划

所有命令在仓库根目录执行（Studio 后端测试需先 `cargo build --bin aep`；UI 测试需先在 `studio/` 执行 `npm install`，pnpm 不在 PATH）。

| 子任务 | 命令 | 覆盖 |
|---|---|---|
| T1 | `cargo test --locked launch` | AC-1（参数、init 统计、E3 探测、模型兜底）、AC-2（`memory_tidy::ask`、审查参数单测） |
| T2 | `cargo test --locked handoff` | AC-10（READING 出现在三种提示词）、复用短提示词不含记忆 / 前序产出、交接摘要含 result 与 AEP-HANDOFF |
| T3 | `cargo test --locked usage` | AC-9（resume 样例 + rollout，误差 0）、E11（无 rollout 标记）、rev 重算、`first_context` |
| T4 | `cargo test --locked session_reuse` | AC-4（≤ / > 阈值两分支）、AC-7（E6–E10 各原因）、默认值与下限 |
| T5 | `cargo test --locked zcode_stage` | AC-13（测试替身 app-server：resume 成功只发新消息；resume 失败回退） |
| T6 | `cargo test --locked --test stage_token_diet` | AC-5、AC-6、AC-7、AC-8：测试替身 claude 记录 argv 与 stdin，走 产品需求 → 交互设计 → 技术方案，断言后两阶段带 `--resume … --fork-session`、提示词不含记忆正文、各有 result/evidence/回执、`reused_session` 正确；改模型后 `model_differs`；超阈值时无 `--resume` 且提示词含上一轮结果 |
| T7 | `cargo test --locked --manifest-path studio/src-tauri/Cargo.toml` | API：对比统计（前/后/E12）、`set_resume_limit` 校验、`session_preview`、`suggested`、`live_usage`、`continuable` |
| T8 | `npx --prefix studio playwright test --config studio/playwright.config.ts` | `interaction.md` §7 列出的 UI 用例（对比卡四态、会话标签三种、Codex 无日志、建议值「采用」、ZCode 按钮文案） |
| 回归（AC-14） | `cargo test --locked`；上面两条 Studio 命令 | 交接、回执、封存现有测试 |

**真实效果验证（AC-1、AC-3，开发实现 / 测试验证阶段执行）**：T1 完成后，用 `bundle` 后的 Studio 对一个新事项跑一次 Claude「产品需求」，读该执行 evidence 的 `launch`（应 MCP 0、skills 0、插件只剩 5 个 builtin、首轮 < 40K）；三阶段都跑完后在「Token 消耗」页对比卡看同阶段同模型平均总量变化（目标 ≤ −60%）。AC-3 依赖真实样本，单元测试无法替代，测试验证阶段应把对比卡数据和执行 ID 写入证据。

## 8. 对 `interaction.md` §8 的答复

1. 会话预告：能廉价算出，保留 §4.1（§1.6）。
2. 续接上限：项目级，默认 150K，下限 20K；§3.2 用「项目」文案（§1.5）。
3. evidence 字段命名：按 §3.3 定稿（与 §6 草案一致，增补 `launch.diet`、`launch.builtin_plugins`、`last_context`、`codex_session_total`、`reused_session.forked`）。
4. ZCode：app-server 通道支持继续，桌面通道不支持（§1.4）；§4.7 两行都会用到，按 `continuable` 选择。
