# 技术方案：可编排的阶段流程

事项：工作流 · 可编排的阶段流程 · 阶段：技术方案 · 2026-11-25
依据：`prd.md`（F1–F7、E1–E14、AC1–AC17）、`interaction.md`（§3–§8、§10 的 5 个待定项）。
拆分：`task.json`（T1–T8，按依赖顺序）。

## 0. 结论

- 新增模块 `src/flow.rs`（`aep::flow`）：`Flow` / `FlowStage`、三套内置流程、校验、AEP-SKIP / AEP-BACK 解析。它是唯一允许接触 `LEGACY/SOFTWARE/GAME` 的地方。
- 事项继续保存 `template`。只有用自定义流程创建的事项才多存一份 `flow` 快照。内置流程的事项数据和今天**逐字节相同**，所以 AC4 不需要迁移。
- `Request` 新增 `flow`（完整阶段定义），执行器优先读它。旧 `request.json` 没有这个字段时退回 `template`（满足 AC5、AC13）。
- `Work::flow()` / `Work::stages()` / `Request::flow()` 取代所有 `template.stages()` 调用（共约 90 处，§4 逐文件列出）。
- 快照或模板无法识别时，`Library::open` 做预处理，把这个事项标为「流程无法读取」，其余数据照常加载（E11）。
- Studio：`ProjectSummary.flows`、`WorkDetail.flow`、4 个新命令。即时校验在前端 `lib/flow.ts` 里，保存时以后端校验为准。两边用同一份用例夹具对齐（§6）。

## 1. 仓库边界

只改本仓库，不涉及其他仓库。不改 `desktop/`（已删除）、`src/zcode_desktop*.rs`、`specs/` 以外的规格文档。

| 区域 | 文件 |
|---|---|
| 流程定义 | 新增 `src/flow.rs`；`src/lib.rs` 加 `pub mod flow;`；`src/workflow.rs` 删掉静态数组和 `Template::{label,stages}` 的定义（移到 flow.rs），`Request` 加字段，删除 `skippable/skips/route_back`（移到 flow.rs） |
| 项目数据 | `src/project.rs`、`src/project/{shelf,transfer,timeline,archive,import,merge}.rs` |
| 执行 / 交接 / 自动运行 | `src/handoff.rs`、`src/autopilot.rs`、`src/session_reuse.rs`、`src/workflow_stage.rs`、`src/zcode_stage.rs`、`src/project_cli/submission.rs` |
| CLI / 草稿 | `src/project_cli.rs`、`src/work_draft.rs` |
| Studio 后端 | `studio/src-tauri/src/api.rs`、`studio/src-tauri/src/lib.rs` |
| Studio 前端 | `studio/src/api/{types,client,mock}.ts`、`studio/src/lib/flow.ts`（新）、`studio/src/components/{Flows.tsx(新),Dialogs.tsx,Sidebar.tsx,WorkPage.tsx,DecisionPanel.tsx,History.tsx}`、`studio/src/state/store.tsx`、`studio/src/App.tsx`、`studio/src/styles/app.css` |
| 测试 | 新增 `tests/flow_baseline.rs`（T1 基线）、`tests/flow_builtin.rs`、`tests/custom_flow.rs`、`tests/fixtures/`；`studio/src-tauri/tests/api.rs` 追加；新增 `studio/tests/flows.spec.ts`。`tests/autopilot.rs` 和 `tests/project_handoff.rs` **不改**（AC1） |

## 2. 数据模型

### 2.1 `aep::flow`

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowStage {
    pub name: String,
    pub brief: String,
    pub role: Role,                       // Role 新增 Serialize/Deserialize，值为 "Plan" 等
    #[serde(default)]
    pub skippable: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub back_to: Vec<String>,             // 空 = 之前的任意阶段
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flow { pub id: String, pub label: String, pub stages: Vec<FlowStage> }

pub fn builtin(t: Template) -> &'static Flow;          // OnceLock<[Flow; 3]>
pub fn builtin_by_id(id: &str) -> Option<&'static Flow>; // "Legacy" | "Software" | "Game"
pub fn template_id(t: Template) -> &'static str;       // 取代 api.rs / work_draft.rs 里的两份同名函数
impl Template { pub fn label(self) -> &'static str; pub fn stages(self) -> &'static [FlowStage]; }
```

- 内置流程由原静态数组生成：`id = template_id(t)`，`label = Template::label`，`skippable = i > 0 && role == Plan`，`back_to = []`（F2）。
- `Template::stages()` 保留，供 `tests/` 里已有的调用继续编译（`tests/support/work_item_merge.rs:119`）。`src/` 和 `studio/src-tauri/src/` 不再调用它（AC14）。
- `pub use crate::flow::FlowStage as Stage;` 留在 `workflow.rs`，`workflow_stage.rs` 里的 `&Stage` 签名不用改。`name` / `brief` 从 `&'static str` 变成 `String`，调用方改用 `&s.name`，或者把返回类型从 `&'static str` 放宽为 `&str`。
- `Profile::for_stage(&FlowStage)`、`Sandbox::for_role(Role)` 的语义不变。

**Flow 上的方法**（取代 workflow.rs 里同名的自由函数）：

```rust
impl Flow {
    pub fn skippable(&self, i: usize) -> bool;  // i > 0 && stages[i].skippable
    pub fn skips(&self, from: usize, result: &str) -> Vec<(usize, String)>; // 解析规则同今天（E8）
    pub fn route_back(&self, from: usize, result: &str) -> Back;
    pub fn back_targets(&self, i: usize) -> Vec<usize>; // back_to 为空时返回 0..i
}
pub enum Back { None, To { stage: usize, question: String }, Rejected { name: String, question: String, allowed: Vec<String> } }
```

`route_back` 取最后一行 `AEP-BACK:`，名称解析和今天一样（`：`/`:`，去掉 `「」<>`）。在当前阶段之前从后往前找同名阶段：

- 找不到时返回 `None`，和今天一致；
- 找到且属于 `back_targets(from)` 时返回 `To`；
- 找到但不在允许范围内时返回 `Rejected`（E9）。

**校验**（F3、E1–E7）：

```rust
pub struct Issue { pub stage: Option<usize>, pub field: &'static str, pub message: String }
pub struct Validation { pub errors: Vec<Issue>, pub warnings: Vec<Issue> }
pub fn validate(flow: &Flow, taken_labels: &[&str]) -> Validation;
```

- `field` 的取值：`label`、`stages`、`name`、`role`、`skippable`、`back_to`。
- `message` 的文案与 `interaction.md §4.3` 逐字一致，阶段序号从 1 开始写成「第 N 个阶段」。
- `taken_labels` 由调用方传入：项目里其他流程的 label，加上三套内置流程的 label（含「经典开发流程」）。
- 阶段名在校验前要先 `trim`。为此提供 `Flow::normalized()`，保存时写入的是 trim 后的值。
- 名称长度按字符计（`chars().count() <= 20`）。

### 2.2 项目里的流程

```rust
// Project 新增
#[serde(default, skip_serializing_if = "Vec::is_empty")] pub flows: Vec<ProjectFlow>,
#[serde(default, skip_serializing_if = "Vec::is_empty")] pub broken_flows: Vec<Broken>,

#[derive(Clone, Serialize, Deserialize)]
pub struct ProjectFlow { #[serde(flatten)] pub flow: Flow, pub revision: u64, pub updated_at: String }
#[derive(Clone, Serialize, Deserialize)]
pub struct Broken { pub error: String, pub raw: serde_json::Value }
```

| 方法 | 行为 |
|---|---|
| `Project::flow_choices() -> Vec<&Flow>` | 内置 Software、Game（不含 Legacy），然后是项目流程，按 label 排序 |
| `Project::save_flow(flow: Flow) -> Result<(String, Vec<Issue>)>` | `id` 为空：新建，id 为 `flow-<uuid 前 12 位>`，revision = 1。否则按 id 覆盖，revision + 1；id 不存在时报「这套流程已被删除」。校验有错误时 `bail!`，把所有 `errors[].message` 用 `\n` 连起来；成功时返回 warnings。内置 id 不能保存（「内置流程不能修改」） |
| `Project::copy_flow(id, label: Option<&str>) -> Result<String>` | 来源可以是内置流程或项目流程。label 缺省为「<原名> 副本」，重名时依次追加「 2」「 3」 |
| `Project::delete_flow(id) -> Result<usize>` | 返回仍在使用它的未归档事项数，用于 toast 和 CLI 输出。`broken_flows` 按 `raw.id` 删除 |
| `Project::flow_in_use(id) -> usize` | 统计 `shelved.is_none()` 且 `flow_id() == id` 的事项 |

### 2.3 事项快照

```rust
// Work 新增（template 字段保持不变）
#[serde(default, skip_serializing_if = "Option::is_none")] pub flow: Option<WorkFlow>,
#[serde(default, skip_serializing_if = "Option::is_none")] pub flow_error: Option<BrokenWork>,

#[derive(Clone, Serialize, Deserialize)]
pub struct WorkFlow { #[serde(flatten)] pub flow: Flow, pub source_revision: u64 }
#[derive(Clone, Serialize, Deserialize)]
pub struct BrokenWork { pub error: String, pub template: Value, pub flow: Value }
```

- 用内置流程创建的事项 `flow = None`，`template` 存对应的枚举值，和今天一样。
- 用自定义流程创建的事项 `flow = Some(..)`，`template = Software`。这个值只是占位，有快照时不会被读取。
- 访问器：

```rust
impl Work {
    pub fn flow(&self) -> Result<&Flow>;        // flow_error → Err(error)；快照 → &snapshot；否则 builtin(template)
    pub fn stages(&self) -> &[FlowStage];       // 出错时返回 &[]
    pub fn flow_id(&self) -> &str;
    pub fn stage_name(&self) -> &str;           // 原为 &'static str；完成时为「已交付」，出错时为「流程无法读取」
}
```

- `delivered()`：`flow_error.is_none() && stage >= stages().len()`（E12；出错的事项不会显示为已完成）。
- `skippable(i)` 改为调用 `self.flow().is_ok_and(|f| f.skippable(i))`。
- `profile(i)` 改用 `stages().get(i)`，出界时返回 `Profile::for_stage` 的 Codex 默认值，不 panic。
- 所有推进类写操作（`finish_stage`、`skip_stage`、`send_back`、`confirm_transfer`、启动执行）先 `work.flow()?`，出错就拒绝，错误文案是「这个事项的流程数据无法读取：…」。
- `add_work(name, kind, template)` 签名保留，测试在用。新增 `add_work_flow(name, kind, flow_id: &str)`：
  - 内置 id（包括 `Legacy`，供 CLI 使用）走原逻辑；
  - 项目流程拷贝成快照，`source_revision = revision`；
  - 找不到 id 时报「流程「X」已被删除，请重新选择」；
  - 两条路径都按 `stages()` 初始化 `notes` 和 CLI 预设 `profiles`。

### 2.4 加载时隔离损坏数据（E11）

`Library::open` 先把文件解析成 `serde_json::Value`，对每个 `projects[].work[]` 做两项检查：

1. 能否把 `template` 反序列化为 `Template`（缺省值是 Legacy，和今天一样）；
2. 如果有 `flow`，能否反序列化为 `WorkFlow`，并且 `validate(&flow, &[]).errors` 为空（label 唯一性在快照里不检查）。

任一项失败，就把原值移到 `flow_error = {error, template, flow}`，`template` 改写为 `"Legacy"`，并删除 `flow`。`projects[].flows[]` 中反序列化或校验失败的项移到 `broken_flows`。处理完再按现在的方式反序列化成 `Library`。保存时写回 `flow_error` 和 `broken_flows`，原值不丢。

这一步只在加载时做，不改变 `projects.json` 正常情况下的格式。

### 2.5 执行请求（F6）

```rust
// Request 新增
#[serde(default, skip_serializing_if = "Option::is_none")] pub flow: Option<Flow>,
impl Request { pub fn flow(&self) -> &Flow { self.flow.as_ref().unwrap_or_else(|| flow::builtin(self.template)) } }
```

- `Work::request(..)`（`handoff.rs:316` 附近）总是写 `flow: Some(work.flow()?.clone())`，内置流程也写（AC13：执行器只看请求）。`template` 照常写入，旧代码读取时不受影响。
- 执行器、交接、门禁里所有 `req.template.stages()` 改为 `req.flow().stages`。
- 旧 `request.json` 没有 `flow` 字段时默认 `None`，回退到 `builtin(template)`（AC5）。
- 时间线 / 证据 / 续聊读的是事项的流程，不是请求里的，所以旧请求不影响它们。

### 2.6 Evidence 新字段

| 字段 | 写入方 | 内容 |
|---|---|---|
| `route_back_rejected` | `workflow_stage.rs` 解析结果处（原 1364 行） | `{"name","question","allowed":[阶段名…]}`。阶段不失败，继续走门禁和 `AEP: PASS` 检查（E9） |

`align_base` 合并冲突时平台自己生成的 `route_back`（原 612 行）不是 Agent 发出的 AEP-BACK，**不受 `back_to` 限制**。操作者手动退回（`Work::send_back`）也不受限制（interaction §6）。

## 3. 接口

### 3.1 CLI（`src/project_cli.rs`，沿用 `aep project <kebab>` 平铺子命令）

| 命令 | 说明 |
|---|---|
| `aep project flows` | JSON：`[{id,label,builtin,stages:[…],in_use,revision,warnings}]`，另附 `broken:[{error,id?,label?}]` |
| `aep project flow-show <id\|label>` | 单个流程的 JSON |
| `aep project flow-save --file <path>` | 文件内容是 `Flow` 的 JSON，`id` 可省略。失败时退出码非零，stderr 逐行输出错误信息；成功时输出 `{"id","revision","warnings":[…]}` |
| `aep project flow-copy <id\|label> [--label <名称>]` | 输出 `{"id"}` |
| `aep project flow-delete <id\|label>` | 输出 `{"deleted":id,"in_use":N}` |
| `aep project add-work <name> --flow <id\|label>` | `--flow` 和 `--template` 互斥；不传 `--flow` 时行为和今天相同 |
| `aep project show`（已有，476 行） | 增加 `"flow":{"id","label"}`；`stages` 改为从 `work.stages()` 读取 |

`<id|label>` 先按 id 精确匹配，再按 label 精确匹配，范围包括内置流程。

### 3.2 Studio 后端（`api.rs` + `lib.rs` 注册）

| 命令 | 签名 | 说明 |
|---|---|---|
| `snapshot` | 原有 | 每个 project 增加 `flows`（同 CLI 结构；内置流程 `builtin:true`、`in_use` 统计、项目流程带 `warnings`）和 `broken_flows`。`work[]` 增加 `flow_error: bool`。顶层 `templates` 保留到 T8 后删除（前端改完即可删） |
| `save_flow` | `(project, flow: Value) -> {id, warnings: string[]}` | 失败时把错误文本原样传给前端 |
| `copy_flow` | `(project, id, label?: string) -> string` | |
| `delete_flow` | `(project, id) -> number` | 返回 `in_use` |
| `create_work` | 原签名不变，`template` 参数改为接受任意流程 id | `"Software"`/`"Game"` 照常可用，所以 `studio/src-tauri/tests/api.rs` 不用改；未知 id 报错（不再静默回退到 Software） |
| `work_detail` | 原有 | 增加 `flow: {id, label, builtin, source_state: "same"\|"changed"\|"deleted"\|null}` 和 `flow_error: string\|null`。出错时 `stages: []`。`template`/`template_label` 保留，`template_label` 改为显示 flow.label |
| attempt summary | 原有（约 370 行） | 增加 `route_back_rejected`（evidence 原样透传或 null） |
| `draft_start` | 原有 | `job.json` 增加 `flows: [{id,label,stages:[name]}]`（来自 `flow_choices()`） |

`source_state` 的计算：内置流程为 null。快照的 id 在 `project.flows` 中不存在时为 `deleted`；存在且 `revision == source_revision` 时为 `same`；否则为 `changed`。这就是 interaction §10 第 4 项的答案：用 revision 计数，不用内容哈希。

### 3.3 前端契约（`studio/src/api/types.ts`）

```ts
export type FlowId = string;                  // 取代 Template 联合类型；保留 `export type Template = FlowId` 兼容别名
export interface FlowStage { name: string; brief: string; role: Role; skippable: boolean; back_to: string[] }
export interface FlowDef { id: FlowId; label: string; builtin: boolean; stages: FlowStage[]; in_use: number; revision: number; warnings: string[] }
// ProjectSummary: flows: FlowDef[]; broken_flows: { id: string | null; label: string | null; error: string }[]
// WorkSummary: flow_error: boolean
// WorkDetail: flow: { id; label; builtin; source_state: "same" | "changed" | "deleted" | null } | null; flow_error: string | null
// AttemptSummary: route_back_rejected: { name: string; question: string; allowed: string[] } | null
// DraftItem: fallback_from?: string | null
// Api: saveFlow / copyFlow / deleteFlow
```

### 3.4 草稿（`work_draft.rs`）

- `prompt(request, notes, existing, flows: &[DraftFlow])`：`DraftFlow { id, label, stages }`，内容取自 `job.json`，旧 job 没有这个字段时用内置 Software 和 Game。提示里把「从下面的模板里选」改成「流程」，列出所有 id 和阶段名。
- `parse(answer, known: &[&str])`：id 不在 `known` 里时改成 `Software`，并设置 `DraftItem.fallback_from = Some(原 id)`（`#[serde(default, skip_serializing_if)]`）。前端在这条草稿下面提示（interaction §5.2）。

## 4. 调用方改造清单（F5、AC14）

规则：拿得到 `Work` 的地方用 `work.stages()` / `work.flow()?`，拿得到 `Request` 的地方用 `req.flow()`。按角色判断的逻辑一律不改。

| 文件 | 位置（当前行号） | 改法 |
|---|---|---|
| `src/project.rs` | 346–367、458、491、578、586 | 改走 §2.3 的访问器；`add_work` 委托给 `add_work_flow` |
| `src/project/shelf.rs` | 26、48 | `delivered()` 按 §2.3；`memory_draft` 用 `stages()` |
| `src/project/transfer.rs` | 104、265、303、312–318、332、356、404 | `stages()`；推进类操作先 `flow()?` |
| `src/project/timeline.rs` | 307、368 | `stages()`。阶段越界时保持现在的 `context("阶段不存在")` |
| `src/project/archive.rs` | 115 | `stages()` |
| `src/project/import.rs` | 395 | `flow::builtin(Template::Software)` |
| `src/project/merge.rs` | 786 | 测试代码，不改 |
| `src/handoff.rs` | 82、88、165、183、208、217–226、325–335、399、449 | `req.flow()`。label 用 `flow.label`。「问题必须由前序阶段（…）」列出的是 `back_targets(req.stage)` 的阶段名；内置流程时这个列表等于之前的全部阶段，文本不变（AC3）。请求里写入 `flow` |
| `src/autopilot.rs` | 129、365、465 | `work.stages()`；门禁失败时退回最近的 Build，逻辑不变（E14、AC10） |
| `src/session_reuse.rs` | 54 | `stages()` |
| `src/workflow_stage.rs` | 408、612、1344–1366、1472、1627 | `req.flow()`；`skips` / `route_back` 改用 Flow 的方法；`Back::Rejected` 写入 evidence（§2.6） |
| `src/zcode_stage.rs` | 288 | `req.flow()` |
| `src/project_cli/submission.rs` | 74 | `req.flow()` |
| `src/project_cli.rs` | 85–90、343、476、527、668、776–784 | §3.1 |
| `src/work_draft.rs` | 35–105 | §3.4 |
| `studio/src-tauri/src/api.rs` | 23–29、127、158–160、316–328、399–421、471、626、743、794、828、1352、1450、1566、1749、1772、1858 | §3.2；`template_id` 改用 `flow::template_id` |

核对 AC14 用的命令：

```
rg -n "\.stages\(\)|\bLEGACY\b|\bSOFTWARE\b|\bGAME\b" src studio/src-tauri/src --glob '!src/flow.rs'
```

期望只剩 `flow.stages` 这类字段访问。`.stages()` 方法调用和静态数组引用都应该是 0。

## 5. 前端

| 组件 | 改动（对应 interaction 章节） |
|---|---|
| `lib/flow.ts`（新） | `validateFlow(flow, takenLabels) -> {errors: Issue[], warnings: Issue[]}`，规则和文案与 Rust 一致；`ROLE_LABEL` / `ROLE_HINT`（§4.2）；`copyLabel`、`defaultNewFlow`（§3.1）；按行身份维护的 `rename/remove` 辅助函数，处理 back_to 的跟随改名和删除（§4.1） |
| `components/Flows.tsx`（新） | `FlowsPage`（§3）+ `FlowEditor` 抽屉（§4）+ 删除确认（§4.5）。抽屉有 dirty 标记，离开前确认（ESC、关闭、切换页面或项目时都会经过这个确认） |
| `store.tsx` / `App.tsx` / `Sidebar.tsx` | `Page` 加 `"flows"`、新增底部链接（§2）；事项行在 `flow_error` 时显示错误点 |
| `Dialogs.tsx` | `ManualWork`：「流程」下拉框用 optgroup 分组，预览里带角色和可跳过标签，加「定制流程…」链接（§5.1）。`DraftWork`：分组下拉框，显示 `fallback_from` 提示（§5.2） |
| `WorkPage.tsx` | 头部显示 `flow.label`，悬停文字按 `source_state` 变化（§6）；`flow_error` 时显示错误卡，隐藏阶段条并禁用运行按钮 |
| `DecisionPanel.tsx` / `History.tsx` | 显示 `route_back_rejected` 的说明和「退回被拒：X」 |
| `mock.ts` | 保存项目流程、快照和 `route_back_rejected` 示例；mock 的 save 调用 `validateFlow`，失败时抛出同样的文本。种子数据加一套「后端开发」，并加一个 `flow_error` 事项，供截图和测试使用 |
| `app.css` | 流程页和编辑器的样式，沿用现有的 card、drawer、tag 等 token，不新增颜色 |

## 6. 前后端校验的一致性（interaction §10 第 3 项）

选择「前端复刻规则 + 后端兜底」。不选「每次编辑都调后端」：mock 环境本来也要一份 TS 实现，而且 Tauri 每次往返都要做去抖处理和竞态处理。

为防止两边漂移：

- `tests/fixtures/flow_cases.json` 列出用例 `[{name, flow, taken_labels, errors:[{stage,field,message}], warnings:[…]}]`，E1–E6 每条规则至少一个拒绝用例（`errors` 非空，指出具体阶段或字段）；E7 只有警告用例：`errors` 为空、`warnings` 非空，表示可以保存（AC11）。E7 不设拒绝用例。
- `tests/flow_builtin.rs` 读这个文件，逐条断言 Rust 的 `validate` 结果。
- `studio/tests/flows.spec.ts` 用 Vite 的 `?raw`/JSON import 读同一个文件，在浏览器里调用 `validateFlow` 逐条断言（Playwright 的 `page.evaluate`，或者让 mock 把它挂到 `window.__aepValidateFlow`，仅 mock 下暴露）。

## 7. 兼容性

| 情形 | 处理 |
|---|---|
| 旧 `projects.json`（没有 flow 字段） | serde 默认 `None`，按 `template` 解析，格式不变（AC4） |
| 新数据被旧版本读取 | 新字段旧版本不认识，会被忽略。自定义流程事项会被当成 Software 显示，属于降级。Studio 和 `aep` 一起打包，这种情况只在回退版本时出现，可以接受 |
| 旧 `request.json` | `flow` 缺省时回退到 template（AC5） |
| 已完成或已收起的旧事项 | `stage >= len` 仍判定为完成（E12） |
| 内置流程的交接文本 | 逐字不变，有快照测试（AC3） |
| `Template` clap ValueEnum / serde 名称 | 不变 |
| 前端 `Template` 类型 | 改为 `string` 的别名，原有调用继续编译 |
| 正在执行的事项 | 快照已冻结，编辑项目流程不影响它（E13） |

## 8. 测试计划

| AC | 测试 | 命令 |
|---|---|---|
| AC1 | 全量回归，`tests/autopilot.rs`、`tests/project_handoff.rs` 不改 | `cargo test --locked` |
| AC2 | `tests/flow_baseline.rs::builtin_equivalence`：对比基线文件 `tests/fixtures/builtin_flows.json`。基线由 T1 在**改造前**的代码上生成（设置 `AEP_BLESS=1` 时写入，否则比较），逐项包含 name、brief、role、skippable、默认 CLI、默认沙箱。测试只用改造后仍然保留的 API：`Template::stages()`、`s.name/brief/role`、`Profile::for_stage`、`Sandbox::for_role`、`Work::skippable`。T2 新增的 `tests/flow_builtin.rs` 再用新 API 对比同一个文件 | `cargo test --locked --test flow_baseline` |
| AC3 | `tests/flow_baseline.rs::handoff_snapshot`：三套内置流程的每个阶段，用固定的 Request（从 JSON 构造）生成 `handoff::header + handoff::rules`，和 `tests/fixtures/handoff/<template>-<i>.txt` 比较。快照同样由 T1 在改造前生成 | 同上 |
| AC4 | `tests/flow_baseline.rs::legacy_library`：`tests/fixtures/legacy_projects.json`（改造前格式，含进行中、已交付、已收起三种事项）用 `Library::open` 打开，检查阶段名、完成判断、能 `finish_stage` 推进到下一阶段，再保存一次，确认没有写入 `flow`/`flow_error` 字段 | 同上 |
| AC5 | `tests/flow_baseline.rs::legacy_request`：只有 `template` 和 `stage` 的 `request.json` 能被 `Request` 解析，`handoff::header` 能用它生成文本。T4 加上 `flow()` 返回内置流程的断言 | 同上 |
| AC6 | `tests/custom_flow.rs::save_and_reload`：CLI `flow-copy Software` → `flow-save` 存「后端开发」→ 重新 `Library::open` 后仍在。`studio/src-tauri/tests/api.rs` 里 `save_flow` 和 `snapshot` 的往返测试 | `cargo test --locked --test custom_flow`；`cargo test --locked --manifest-path studio/src-tauri/Cargo.toml flow` |
| AC7 | `custom_flow::runs_to_delivery`：用 `tests/autopilot.rs` 同样的假执行器（`Profile.executable` 指向测试脚本），7 个阶段自动运行到交付归档并合入。断言「安全审查」的 request 里 role 是 Test、sandbox 是 Network、门禁生效、工作区没有变化 | 同上 |
| AC8 | `custom_flow::skip_rules`：`AEP-SKIP: 技术方案：…` 被采纳，`AEP-SKIP: 安全审查：…` 被忽略。另有 Flow 的单测 | 同上 |
| AC9 | `custom_flow::back_rules`：`AEP-BACK: 开发实现` 退回到开发实现；把 `back_to` 改成 `["产品需求"]` 后同样的输出不退回，evidence 里有 `route_back_rejected` | 同上 |
| AC10 | `custom_flow::autopilot_gate_failure`：安全审查失败后退回开发实现 | 同上 |
| AC11 | `flow_builtin::validation_cases`（夹具）+ `flows.spec.ts` 的同源断言 | 两条命令 |
| AC12 | `custom_flow::snapshot_frozen`：改名一个阶段、删除整套流程之后，事项的 `stages()`、长度和推进都不变；`source_state` 依次为 changed、deleted | `cargo test` 两处 |
| AC13 | `custom_flow::request_self_contained`：新 request 里有 `flow`。从项目里删掉这套流程后，执行器（`aep project run`/`workflow_stage` 入口）仍能跑完这个阶段 | 同上 |
| AC14 | §4 的 rg 命令输出为空（放在 T7 的验证里） | `rg …` |
| AC15/16 | `studio/tests/flows.spec.ts`：复制 → 编辑（删除、添加、改角色、上移下移）→ 即时错误 → 保存 → 新建事项选择 → 阶段条 7 格 → 删除确认；内置流程只读 | `npm --prefix studio run test:ui -- flows` |
| AC17 | 前端类型检查和构建 | `npm --prefix studio run build` |

后端 e2e 需要先 `cargo build --bin aep`（项目决策 #58）。

## 9. 子任务顺序（详见 task.json）

1. **T1** 生成基线夹具和测试（不改实现）
2. **T2** `flow.rs` 和校验。`name`/`brief` 改为 `String` 后，`src/` 里所有因此编译失败的调用方都在 T2 一并做类型适配，已知有 `autopilot.rs`、`work_draft.rs`、`project/timeline.rs`。这些地方只改类型，不改读取来源。`studio/src-tauri` 的编译适配在 T6 做
3. **T3** 项目和事项数据：快照、加载隔离、项目流程增删改
4. **T4** Request 自包含，执行器、交接、门禁改读 flow，E9
5. **T5** 剩余读取方（autopilot、transfer、timeline 等）和 CLI、草稿，以及端到端测试
6. **T6** Studio 后端接口
7. **T7** 前端契约、mock、新建事项对话框、事项页，以及 AC14 核对
8. **T8** 流程页和编辑器，Playwright 测试

## 10. 风险

- **`stage_name()` 返回类型变化**会波及使用它的 `format!` 和 `json!`。返回 `&str` 后编译器会逐处指出，属于机械改动。
- **`Library::open` 预处理**会多一次 JSON 解析。`projects.json` 通常不到 1 MB，可以忽略。
- **前后端校验漂移**由 §6 的同源夹具兜底。
