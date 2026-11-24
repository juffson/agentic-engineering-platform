# PRD：ZCode 桌面双方案 · 实际模型与交接

## 背景

「双方案」是 ZCode 桌面已配置的两个 BigModel 账号方案（`zcode_catalog::desktop_plan`）：

- `account:bigmodel-start-plan`（BigModel Start Plan）
- `account:bigmodel-individual-coding-plan`（BigModel 个人 Coding Plan）

选中其中任一方案的模型时，AEP 通过 aep-studio 扩展在 ZCode 桌面派发任务（`zcode_stage::converse` → `zcode_desktop::converse`）。其他模型仍走 app-server。

### 现状

**已有**
- 桌面路径从 `model_usage`（`query_source='main_turn'`）读取实际 provider、模型和推理档位。provider 或模型与选择不符时报错。
- 回执写入执行目录的 `session.json` 和 `zcode-model.json`，其中 `transport` 为 `desktop`。
- 两轮接力卡（`relay_page.rs`）显示「ZCode 桌面实现/审查回执 · 会话 · provider/model · 实际档位」。

**缺口**

下列位置显示或记录的都是**所选模型**（`profile.model`、`request.zcode_model`），不是实际模型：

| 位置 | 代码 | 现在显示 |
|---|---|---|
| 「会话与轮次」阶段轮次标题 | `console.rs` | 所选模型，未选时为「默认模型」 |
| 事项时间线（阶段执行和接力轮次） | `project/timeline.rs:343`、`:175` | 所选模型 |
| `aep project show` | `project_cli.rs` `view()` | `model:a.profile.model` |
| 阶段交接写入的项目记忆作者 | `workflow_stage.rs` | `ZCode / 客户端默认` 或所选模型 |

其他缺口：

- 阶段执行完成后，Studio 不再显示桌面回执（实际方案、模型、档位、任务 ID）。「打开对应 ZCode 任务」只在执行中出现。
- 走 app-server 时，`zcode-model.json` 只记下会话创建时的 `settings.model.current`，没有档位。`session.json` 里也没有模型。
- 选「客户端默认」时，实际用了哪个方案和模型，只能翻执行档案查看。

## 目标

无论走哪个方案、在哪一端（阶段执行、接力实现、接力审查）、通过哪条路径（桌面或 app-server），操作者在 Studio、CLI 和共享记忆里看到的都是同一份**实际模型回执**：方案、模型、推理档位和 ZCode 任务。交接摘要也应能追溯到产出它的实际模型和会话。

## 术语

- **所选模型**：阶段 profile、项目默认或本轮改选得到的 `provider/model[@档位]`，可以为空，即客户端默认。
- **实际模型回执**：ZCode 完成本轮后，AEP 从 ZCode 读到的 `providerId`、`modelId`、`reasoningLevel`，以及 `transport`、`session`、`turn`。桌面路径的来源是 `model_usage` 主对话记录；app-server 路径的来源是会话设置。
- **方案名**：`desktop_plan(providerId)` 的显示名。不属于双方案的 provider 原样显示。

## 用户场景

- **S1 阶段使用 Start Plan**：某阶段选 Start Plan 的某个模型执行。完成后，轮次标题显示「BigModel Start Plan / 模型 / 实际档位」。可以打开原 ZCode 任务，并看到交接摘要。
- **S2 阶段使用个人 Coding Plan**：同 S1，换成个人 Coding Plan。两种方案的显示和回执完全一致，只有方案名不同。
- **S3 接力两端分别用两种方案**：实现端用 Start Plan，审查端用个人 Coding Plan。接力卡、时间线和交接记录分别标出两端的实际方案与模型。
- **S4 客户端默认**：阶段或接力未指定模型。完成后显示 ZCode 当时实际使用的方案与模型，并标注「客户端默认」。
- **S5 所选与实际不符**：桌面任务实际用了另一个 provider 或模型。这一轮失败，原因写明「所选 X，实际 Y」，不产出交接，也不写入记忆。
- **S6 CLI 查看**：`aep project show`、时间线 CLI 和 `aep zcode-group --list` 输出同样的实际模型字段。
- **S7 旧记录**：功能上线前的执行没有实际模型回执。显示所选模型，并标「未记录实际模型」，不报错。

## 范围

**包括**
- 统一读取实际模型回执：桌面和 app-server 两条路径，在阶段执行、接力实现、接力审查和 `--recover-desktop` 收取中都适用。
- 在 Studio 的阶段轮次、接力卡（保持现状）、事项时间线、ZCode 任务归组历史中显示实际模型。
- 在 `aep project show`、时间线 CLI、`aep zcode-group --list` 中输出实际模型。
- 记忆作者和交接摘要带上实际模型和会话来源。
- 阶段完成后保留「打开对应 ZCode 任务」入口。
- 归档（`project/archive.rs`）继续收录 `session.json` 和 `zcode-model.json`，阶段执行目录也要收录。

**不包括**
- 改变方案选择 UI、项目默认模型的保存逻辑，或桌面方案「档位使用模型默认」的规则。
- 新增方案，或支持双方案以外的桌面 provider。
- 读取 ZCode 凭据，或在 ZCode 中修改模型设置。
- 用量和计费统计。
- 推送、合并、发布。

## 功能需求

### 回执

- **FR-1** 桌面路径完成后，回执至少包含 `transport=desktop`、`client`、`session`、`turn`、`message`、`providerId`、`modelId`、`reasoningLevel`（可以为空）、`workspace`。沿用现有 `session.json` 和 `zcode-model.json` 两个文件。
- **FR-2** app-server 路径完成后，回执写出同一组字段，其中 `transport=app-server`。provider 和模型以本轮实际执行时的设置为准；如果协议能取得推理档位，写入 `reasoningLevel`，否则留空。
- **FR-3** 回执另记 `selected` 字段，保存所选模型字符串，空串表示客户端默认。由此可以区分「指定」和「客户端默认」。
- **FR-4** 回执写入失败或缺少主对话用量时，这一轮失败并写明原因，不产出交接（沿用现有 `缺少 ZCode 主对话模型用量回执` 的行为）。
- **FR-5** 所选 provider 或模型与实际不符时（FR-1 路径已有校验；FR-2 路径在会话创建时校验），这一轮失败。错误文本包含所选和实际两组值，Studio 在轮次或接力卡上用红色显示。

### 显示

- **FR-6** 统一格式为「方案名 / modelId / 实际档位」。档位为空时显示「默认」。客户端默认的运行在后面追加「· 客户端默认」。两种方案用同一个组件渲染。
- **FR-7** 「会话与轮次」的阶段轮次标题：有回执时显示 FR-6 文本；没有回执且已结束时，显示所选模型加「未记录实际模型」；执行中保持现有派发提示（`desktop_dispatch`）。
- **FR-8** 阶段轮次结束后，如果存在 `desktop-create.json`，保留「打开对应 ZCode 任务」按钮，并复用现有 `zcode-open`。
- **FR-9** 事项时间线中，阶段执行和接力轮次的模型字段优先取实际回执，回执缺失时回退到所选模型并标注来源。
- **FR-10** ZCode 任务归组历史的每条会话都显示实际模型。

### CLI

- **FR-11** `aep project show` 的每个 attempt 增加 `actual_model` 对象（FR-1 字段的子集），保留原 `model` 字段（所选）。没有回执时 `actual_model` 为 `null`。
- **FR-12** 时间线 JSON 和 `aep zcode-group --list` 使用同样的 `actual_model` 结构。

### 交接与记忆

- **FR-13** 阶段完成写入项目记忆时，作者为「ZCode / 方案名 / modelId@档位」，取自实际回执。没有回执的非 ZCode 客户端维持现状。
- **FR-14** evidence.json 增加 `actual_model`，与 FR-11 结构相同，下一阶段可以引用。交接摘要的提取规则（`AEP-HANDOFF:` 行）不变。
- **FR-15** `--recover-desktop` 和 `zcode_desktop::completed` 收取的交接同样写出 FR-1、FR-3 回执，并通过 FR-5 校验。

### 兼容

- **FR-16** 旧执行目录缺少新字段时，读取不报错，按 FR-7 和 FR-9 回退显示。
- **FR-17** 不改写已有执行目录中的回执文件。

## 边界情况

- **E1** 一个 turn 内有多条 `main_turn` 用量，例如中途重试：取最后一条完成记录，每一条都必须与所选一致（现有行为）。
- **E2** 只有 `title` 等非主对话用量：视为缺少回执，按 FR-4 处理。
- **E3** 桌面任务被删除、ZCode 未运行或数据库被锁：执行中按现有逻辑等待或报错；已完成的显示不依赖 ZCode 数据库，只读执行目录里的回执。
- **E4** 同一派发被发送两次：沿用「同一派发已发送多次」报错。
- **E5** 方案在本机已被移除，但历史回执还引用它：仍按回执显示原 provider 和模型，方案名回退为 providerId。
- **E6** 接力两端都是 ZCode，且分属两种方案：两端的回执相互独立，不串用（目录分别为 `zcode-edit` 和 `zcode-plan`）。
- **E7** 同轮重试：每次执行的回执分别保留，显示各自的实际模型。
- **E8** 运行等待工具审批后才完成：回执在完成时写入，审批等待不影响回执。

## 验收标准

| 编号 | 场景 | 验收标准 | 验证方式 |
|---|---|---|---|
| AC-1 | S1/S2 | 用 Start Plan 和个人 Coding Plan 的模拟 `model_usage` 各跑一次阶段，`session.json` 和 `zcode-model.json` 含 FR-1 全部字段及 `selected`，两份结构相同 | `zcode_desktop` 单元测试 |
| AC-2 | S4 | 所选为空、走 app-server 时，回执含 `transport=app-server`、实际 provider 和模型，`selected=""` | `zcode_stage` 协议替身测试 |
| AC-3 | S5 | 所选与实际的 provider、模型或（显式指定时的）档位不符，这一轮失败，错误同时含两组值，且不写 result 记忆 | 单元测试 + 阶段执行测试 |
| AC-4 | S1/S2 | 阶段轮次标题的 FR-6 文本对两种方案分别正确，客户端默认的运行追加「客户端默认」 | desktop 渲染单元测试 + `ui_debug` 截图场景 |
| AC-5 | S1 | 阶段完成后仍可见并能点击「打开对应 ZCode 任务」 | `ui_debug` 场景 |
| AC-6 | S3 | 实现端用 Start Plan、审查端用个人 Coding Plan 的接力，在接力卡和时间线上分别显示各自的实际方案 | timeline 单元测试 + `ui_debug` |
| AC-7 | S6 | `aep project show` 和时间线 JSON 含 `actual_model`，旧记录中为 `null` | CLI 集成测试 |
| AC-8 | FR-13 | 阶段交接写入记忆的作者为「ZCode / 方案名 / modelId@档位」 | `workflow_stage` 测试 |
| AC-9 | S7 | 旧执行目录（无 `selected`、无 `actual_model`）在 Studio 和 CLI 中正常显示「未记录实际模型」 | 兼容性单元测试 |
| AC-10 | FR-15 | `--recover-desktop` 收取后同样写出完整回执，并执行不符校验 | `zcode_desktop::completed` 测试 |
| AC-11 | 全局 | `cargo test --locked` 与 `cargo check --locked --manifest-path desktop/Cargo.toml --all-targets` 通过 | CI 本地命令 |
| AC-12 | 真实环境（人工） | 本机 ZCode 已配置双方案时，分别用两种方案跑一个阶段，Studio 显示与 ZCode 桌面任务中的模型一致 | 人工验收，记录截图 |

## 待决问题

- **Q1** 阶段 profile 的模型为空时，是否应继承「项目设置 → ZCode 项目默认模型」？这个默认值现在只作用于接力。本 PRD 不改变它，只把实际使用的模型显示出来。
- **Q2** app-server 协议是否在 turn 级别提供实际档位？若不提供，FR-2 中的档位留空并显示「默认」。由技术方案阶段确认。
