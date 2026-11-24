# 技术方案：ZCode 桌面双方案 · 实际模型与交接

依据：`spec.md`（FR-1–FR-17、AC-1–AC-12）、`interaction.md`（§2 格式、§3 状态、§4 CLI 结构、§5 ui_debug）。本文定仓库边界、接口、数据、变更文件、兼容性和测试。

## 0. 现状（代码核对结果）

| 位置 | 现在做什么 | 问题 |
|---|---|---|
| `src/zcode_desktop.rs::read_reply` | 从 `model_usage`（`main_turn`、`completed`）取最后一条，校验 provider/model/显式档位，返回 evidence；`converse`、`completed` 把它写入 `session.json` 和 `zcode-model.json` | 没有 `selected`；不符文案为「桌面任务 X 实际使用 … 与本轮选择 … 不符」，不是交互稿 §3.4 文案 |
| `src/zcode_stage.rs::converse`（app-server 分支） | `session/create` 返回后写 `session.json`（只有 `client/transport/session`）和 `zcode-model.json`（`settings.model.current`） | 没有档位、没有 turn、`session.json` 无模型 |
| `src/workflow_stage.rs:589` | 作者 = `<客户端> / <profile.model 或 客户端默认>`；`record["native_session"]` 复制 `session.json` | 用所选不用实际；evidence 无 `actual_model` |
| `src/project/timeline.rs` | `Agent.model` = 所选；`session()` 只认 `transport=="desktop"` | 无实际模型 |
| `src/project_cli.rs::view` | attempt 只有 `model`（所选） | 无 `actual_model` |
| `src/zcode_group/history.rs::row` | 归组历史行 | 无实际模型 |
| `desktop/.../console.rs:349` | 轮次标题模型段 = 所选/「默认模型」；`desktop_dispatch` 仅 `active` 时出现 | 完成后无回执、无「打开对应 ZCode 任务」 |
| `desktop/src/view/relay_page.rs:644` | 只认 `transport=="desktop"`，文本自拼 | 不认 app-server，格式不统一，完成后无打开入口 |
| `src/project/archive.rs:215` | 阶段目录收录 `session.json`，不收 `zcode-model.json` | FR 范围要求阶段目录也收录 |

接力端：`src/agent.rs:321` 只有在接力端**有 ZCode 模型**时才走 `zcode-bridge`（→ `zcode_stage::converse`，写回执到 `zcode-edit`/`zcode-plan`）；无模型时直接跑 ZCode CLI（`--prompt … --attach`），不产生会话回执。

## 1. 待决问题的决定

- **Q1（阶段空模型是否继承项目默认）→ 不继承。** 保持 PRD 范围：阶段 profile 为空即客户端默认，走 app-server（`zcode_stage::run` 已如此），回执 `selected=""`，界面追加「· 客户端默认」。项目默认模型仍只作用于接力。
- **Q2（app-server 是否给 turn 级档位）→ 能取到，但不是从协议消息。** 本机核对 `~/.zcode/cli/db/db.sqlite`：AEP app-server 会话（`sess_<32hex>`）同样写入 `model_usage`，含 `query_source='main_turn'`、`provider_id`、`model_id`、`variant`、`turn_id`。因此 app-server 路径与桌面路径共用同一个「主对话用量」读取；只有在该数据库不存在（无桌面数据的纯 CLI 安装）或等待后仍无该会话记录时，才回退到 `session/create` 的 `settings.model.current`，此时 `reasoningLevel=null`、`source="settings"`。
- **D3 接力端客户端默认（无 ZCode 模型）不改调用方式。** 该路径是 ZCode CLI 直跑、没有会话 ID，无法定位用量记录；改走 bridge 会改变接力实现的调用与输出解析，超出本事项。界面按交互稿 §3.2「本端已结束无回执」显示「未记录实际模型」。S4 的完整回执覆盖阶段执行；接力两端在实际使用中都有模型（项目默认模型作用于接力）。
- **D4 旧 app-server 回执不回读 `zcode-model.json`。** 唯一来源是执行目录的 `session.json`；缺 `providerId`/`modelId` 即视为未记录（S7、FR-16）。避免同一界面出现两种来源。

## 2. 接口

### 2.1 新库模块 `src/zcode_receipt.rs`（`pub mod zcode_receipt;` 加入 `src/lib.rs`）

格式函数只有这一份实现，CLI、Studio、时间线、归组历史、记忆作者共用（交互稿 §6）。

```rust
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActualModel {
    pub transport: String,               // "desktop" | "app-server"
    pub plan: String,                    // desktop_plan(providerId) 或 providerId（E5）
    pub provider_id: String,
    pub model_id: String,
    pub reasoning_level: Option<String>,
    pub selected: Option<String>,        // None = 旧回执无此字段；Some("") = 客户端默认
    pub session: Option<String>,
    pub turn: Option<String>,
}

impl ActualModel {
    /// 从回执 JSON 构造；缺 providerId/modelId 返回 None。plan 现算，不信任文件里的值。
    pub fn from_receipt(v: &Value) -> Option<Self>;
    /// 交互稿 §2：`<方案名> / <modelId> / <档位|默认>[ · 客户端默认]`
    pub fn label(&self) -> String;
    /// 交互稿 §3.6：`ZCode / <方案名> / <modelId>@<档位|默认>[ · 客户端默认]`
    pub fn author(&self) -> String;
    /// 传输 caption：「ZCode 桌面」/「ZCode app-server」
    pub fn transport_label(&self) -> &'static str;
}

/// 读执行目录（阶段 attempt 目录或 zcode-edit/zcode-plan）的 session.json。
/// 文件不存在或缺模型字段 → Ok(None)；JSON 损坏 → Err（调用方显示「实际模型回执无法读取」）。
pub fn read(dir: &Path) -> Result<Option<ActualModel>>;
/// 所选模型字符串：None → ""；否则 `provider/model[@level]`（取 options.reasoningLevel）。
pub fn selected(model: Option<&Value>) -> String;
/// 无回执回退：`<所选或「默认模型」> · 未记录实际模型`
pub fn fallback(selected: &str) -> String;
/// §3.4：`ZCode 实际模型与所选不符：所选 <selected>，实际 <方案名 / modelId / 档位>`
pub fn mismatch(selected: &str, provider: &str, model: &str, level: Option<&str>) -> String;
```

`selected(None)`/空串在 `mismatch` 中显示为「客户端默认」（客户端默认不会触发不符，仅防御）。

### 2.2 回执文件字段（`session.json` 与 `zcode-model.json` 内容相同）

```json
{"transport":"desktop|app-server","client":"ZCode","session":"…","turn":"…","message":"…|null",
 "providerId":"…","modelId":"…","reasoningLevel":"…|null","workspace":"…","selected":"",
 "source":"model_usage|settings"}
```

- 桌面路径：在现有 evidence 上加 `selected`、`source:"model_usage"`（FR-1、FR-3）。
- app-server 路径：`session/create` 时仍先写 `{"client","transport","session"}`（执行中 Studio 可见会话），**`turn.completed` 后、取最终答复前**覆盖为完整回执（FR-2）。`workspace` = 进程 cwd。

### 2.3 主对话用量读取（二进制 crate 内共用）

把 `zcode_desktop::read_reply` 中的 `model_usage` 循环抽成：

```rust
pub(crate) fn main_turn_usage(db: &Connection, session: &str, turn: Option<&str>,
    model: Option<&Value>, selected: &str) -> Result<Option<Usage>>
// Usage { provider, model, variant, turn, message }
```

- `turn=Some` 时按 turn 过滤（桌面）；`None` 时取该会话全部 `main_turn`（app-server 每个会话只发一轮）。
- 每条都要与 `model` 一致（provider、model、显式档位），不符 → `bail!(zcode_receipt::mismatch(..))`（E1、FR-5）。`model=None` 不校验。
- 返回最后一条 `completed`。

app-server 读取：`$HOME/.zcode/cli/db/db.sqlite` 存在时只读打开，最多轮询 5 秒等记录出现（WAL 提交滞后）；仍无记录或库不存在 → 回退 settings。数据库路径通过内部函数参数传入，便于测试。

### 2.4 写入失败

回执写入失败 → `context("无法写入 ZCode 实际模型回执")`，本轮失败（§3.4）。桌面缺主对话用量沿用「缺少 ZCode 主对话模型用量回执」（FR-4、E2）。不符错误已由 `zcode_stage` 现有 `inspect_err` 写入 `desktop-error.json`，接力卡与阶段失败原因都能读到。

### 2.5 消费方

| 消费方 | 改动 |
|---|---|
| `workflow_stage.rs` | 抽 `fn handoff_author(profile: &Profile, actual: Option<&ActualModel>) -> String`：ZCode 且有回执 → `actual.author()`，否则维持现状（FR-13）；`record["actual_model"] = json!(actual)`（FR-14，无回执为 `null`）。损坏回执不阻断交接，按无回执处理并 `eprintln!` |
| `project/timeline.rs` | `Agent` 加 `#[serde(default)] pub actual_model: Option<ActualModel>` 和 `#[serde(default, skip_serializing_if = "Option::is_none")] pub actual_model_error: Option<String>`；阶段读 attempt 目录，接力分别读 `zcode-edit`/`zcode-plan`（E6、E7）。`Agent.model` 仍为所选 |
| `project_cli.rs::view` | 每个 attempt 加 `"actual_model"`（`read(&d).ok().flatten()`，FR-11）；保留 `model` |
| `zcode_group/history.rs::row` | 加 `"actual_model"`（FR-12） |
| `project/archive.rs` | 阶段目录清单加 `"zcode-model.json"`（`"session.json"` 已在） |
| Studio `console.rs` | 模型段：有回执 `label()`（id `turn-model-<attempt>`）；ZCode 已结束无回执 → `fallback()`；其余现状。非 `active` 时在标题栏下加回执行：caption `"{transport_label()}实际模型"` + `detail_text("turn-receipt-<attempt>", "会话 … · turn …")`；ZCode 无回执 → caption「未记录实际模型」；存在 `desktop-create.json` → `zcode-open-<session>` 按钮（复用 `open_zcode_task`、`zcode_navigation` 反馈）。失败轮次的不符文本来自 evidence `error` 现有显示 |
| Studio `zcode.rs` | 抽 `fn zcode_open_button(&self, dir, session, cx) -> Div`（按钮 + 导航 caption），`desktop_dispatch` 与回执行共用 |
| Studio `relay_page.rs` | 回执不再限定 `transport=="desktop"`；文本 `"{label()} · 会话 {session}"`、caption `"{transport_label()}{role}回执"`；有 `desktop-create.json` 时加打开按钮；本端客户端 ZCode、非执行中、无回执 → caption「{role} · 未记录实际模型」 |
| Studio `project_page/timeline.rs` | 卡片标题追加模型段（§3.3）；详情「实际模型」「选择的模型」两行，会话追加 ` · turn …`；`actual_model_error` → caption「实际模型回执无法读取」 |
| Studio `zcode_history.rs` | `field("实际模型", label 或「未记录」)` |

## 3. 数据与兼容性

- **只增不改**：新字段均为可选；旧 `session.json`（无 `selected`、无模型）`from_receipt` 返回 `None` 或 `selected=None`（不追加「客户端默认」），界面走回退（FR-16）。
- **不改旧文件**：仅在新执行完成时写回执；读取路径只读（FR-17）。
- **Timeline JSON**：`actual_model` 总是序列化（`null` 或对象），`actual_model_error` 仅在出错时出现；desktop 读旧 JSON 靠 `serde(default)`。
- **`--recover-desktop`**：`zcode_desktop::completed` 已经走 `read_reply`，改后自动带 `selected` 和不符校验（FR-15），`run.rs` 不改。
- **E3**：已完成轮次的显示只读执行目录，不打开 ZCode 数据库。
- **E5**：`plan` 现算，未知 provider 回退 providerId。

## 4. 变更文件

见 `task.json` 的 `allowed_paths`。新增文件只有 `src/zcode_receipt.rs`。不改 `src/agent.rs`、`src/run.rs`、`src/zcode_catalog.rs`、模型选择 UI。

## 5. 测试计划

| 编号 | 测试 | 位置 | 覆盖 |
|---|---|---|---|
| T1 | 两种方案 `label()`：`BigModel Start Plan / glm / high`、`BigModel 个人 Coding Plan / glm / 默认 · 客户端默认`；未知 provider 显示 providerId；`selected=None` 不追加 | `zcode_receipt` 单元测试 | AC-4、E5 |
| T2 | `author()` 档位空写 `@默认`、客户端默认追加；`fallback("")` = `默认模型 · 未记录实际模型`；`mismatch` 同含「所选」「实际」 | 同上 | AC-8、AC-3 |
| T3 | `read()`：缺文件 None；旧回执（只有 session/transport）None；损坏 JSON Err；序列化键名与 §4 一致（`providerId`、`reasoningLevel`…） | 同上 | AC-7、AC-9 |
| T4 | 扩展 `only_uses_the_addressed_desktop_task_turn_and_verified_model`：对 Start Plan 与个人 Coding Plan 各跑一次，evidence 键集合相同且含 `selected`；不符错误含「所选」「实际」 | `zcode_desktop` 单元测试 | AC-1、AC-3、E1、E2 |
| T5 | `main_turn_usage(turn=None)`：内存库里 app-server 会话的最后一条 main_turn 带 variant；多条中任一不符 → Err；无记录 → None | `zcode_desktop` 或 `zcode_stage` 单元测试 | AC-2、Q2 |
| T6 | `completed()` 回收路径写出 `selected` 并执行不符校验（构造 desktop-task/request/create 夹具 + 内存库，抽出可注入连接的内部函数） | `zcode_desktop` 单元测试 | AC-10 |
| T7 | `handoff_author`：ZCode+回执 → `ZCode / BigModel Start Plan / glm@默认`；ZCode 无回执 → 现状；Claude → 现状 | `workflow_stage` 单元测试 | AC-8 |
| T8 | 扩展 `native_zcode_bridge_uses_the_existing_default_and_keeps_its_session_id`：子进程 `HOME` 指向临时目录（无 ZCode 库），断言 `session.json` 含 `transport=app-server`、`providerId=native`、`modelId=local-default`、`selected=""`、`reasoningLevel=null`、`source=settings`，且与 `zcode-model.json` 相同 | `tests/project_handoff.rs` | AC-2 |
| T9 | `aep project show` 对无回执 attempt 输出 `"actual_model": null`；写入回执后为对象 | `tests/project_handoff.rs` | AC-7 |
| T10 | 扩展 `legacy_relay_retries_and_session_stage_survive_reopening`：一轮 `zcode-edit` 用 Start Plan、`zcode-plan` 用个人 Coding Plan，两 agent 的 `actual_model.plan` 不同；旧轮次为 None；损坏回执 → `actual_model_error` 且时间线无 errors | `project::timeline` 单元测试 | AC-6、AC-9、E6、E7 |
| T11 | Studio 编译 | `cargo check --manifest-path desktop/Cargo.toml --all-targets` | AC-11 |
| T12 | ui_debug：Workflows（Start Plan 成功 + 打开按钮、个人 Coding Plan 客户端默认、旧轮次、不符失败）、StudioRelay（两端两方案），按交互稿 §5 断言 | `desktop/examples/support/{workflows,stage_relay}.rs` | AC-4、AC-5、AC-6 |
| T14 | `history::row` 对有回执的目录输出 `actual_model` 对象、无回执为 `null`（内存 tasks-index 夹具） | `zcode_group/history.rs` 单元测试 | FR-10、FR-12 |
| T13 | 人工：本机双方案各跑一个阶段，对照 ZCode 桌面任务模型，截图 | `validation.md` | AC-12 |

**交互稿 §5 的 History 场景调整**：`ui_debug` 的 `History` 是 Git 历史场景，ZCode 归组历史只有 live 场景（`--live-zcode-history`，需本机 ZCode）。FR-10 改由 T14 覆盖数据，界面字段在 live 场景中人工核对（`validation.md`），不新增离线场景。

全部：`cargo test --locked`、`cargo check --locked --manifest-path desktop/Cargo.toml --all-targets`。
