# 交互设计：ZCode 桌面双方案 · 实际模型与交接

依据：`spec.md`（S1–S7、FR-1–FR-17、E1–E8、AC-1–AC-12）。本文只定义操作者看到什么、在哪里看到、各状态下怎么变化，不定义回执的读取实现（留给技术方案）。Q1、Q2 不影响本设计：无论阶段是否继承项目默认模型、app-server 是否给出档位，界面都按回执里的 `selected` 和 `reasoningLevel` 是否为空来决定显示。

## 0. 不改的部分

- 模型选择入口（阶段 profile 输入框、「ZCode 项目默认模型」面板、接力改选）不变，PRD 已排除。
- 接力卡里已有的「ZCode 桌面实现/审查回执」位置不变，只把文本换成统一格式（§2），并补上 app-server 回执（今天它只认 `transport == "desktop"`）。
- 执行中的派发与审批提示（`zcode.rs::desktop_dispatch`）的内容和顺序不变。

## 1. 用户路径

**P1 阶段执行（S1/S2/S4）**
1. 操作者在事项「会话与轮次」里启动阶段。
2. 执行中：轮次标题显示所选模型（§3.1 状态 B），标题栏下方保留现有派发提示：「正在 ZCode 桌面创建阶段任务 · 方案 / 模型 / 档位」→「ZCode 桌面任务 <session> · 等待交接」或「等待工具审批」，并有「打开对应 ZCode 任务」。
3. 完成：轮次标题的模型段换成实际模型；标题栏下方出现「实际模型」回执行和「打开对应 ZCode 任务」（不再随执行结束消失）。
4. 操作者展开事项时间线，同一轮显示同样的实际模型；确认交接后，项目记忆的作者是「ZCode / 方案 / 模型@档位」。

**P2 接力（S3/E6）**：实现端、审查端分别完成后，接力卡在各自的回执下显示自己的实际模型；时间线中实现卡和审查卡各自显示，两者不互相回填。

**P3 不符失败（S5）**：轮次或接力端失败。红字原因「所选 X，实际 Y」出现在失败位置（§3.4）。没有交接、没有记忆写入，操作者用现有重试或改选模型后重跑。

**P4 回看历史（S7/E5/E7）**：打开旧事项或旧轮次。有回执的照常显示；没有回执的显示所选模型加灰色「未记录实际模型」。同一阶段多次重试，每一轮各显示各的。

**P5 CLI（S6）**：`aep project show`、时间线 JSON、`aep zcode-group --list` 输出 `actual_model` 对象（§4）。

## 2. 统一的「实际模型」文本

所有界面调用同一个格式函数，两种方案只差方案名（AC-4、FR-6）：

```
<方案名> / <modelId> / <档位>[ · 客户端默认]
```

| 输入 | 显示 |
|---|---|
| providerId 属于双方案 | 方案名取 `zcode_catalog::desktop_plan`，如「BigModel Start Plan」「BigModel 个人 Coding Plan」 |
| providerId 不在双方案（app-server 的其他 provider，或本机已移除的方案，E5） | 直接显示 providerId |
| `reasoningLevel` 为空或缺失 | 「默认」 |
| `selected == ""` | 末尾追加「 · 客户端默认」 |
| `selected` 非空 | 不追加（成功即说明与所选一致，不重复显示所选） |

例：
- `BigModel Start Plan / glm-4.6 / high`
- `BigModel 个人 Coding Plan / glm-4.6 / 默认 · 客户端默认`

**回退文本**（无回执时）：`<所选模型或「默认模型」> · 未记录实际模型`，「未记录实际模型」为灰色 caption。

传输路径用小号 caption 区分：桌面回执标「ZCode 桌面」，app-server 回执标「ZCode app-server」。只出现在回执行，不进标题，避免标题过长。

## 3. 界面与状态

### 3.1 「会话与轮次」阶段轮次（`console.rs`）

**布局**：标题栏保持「第 N 轮 · 状态 chip · <客户端> · <模型段> · <仓库> · 耗时 · [原始日志][执行档案][工作区]…」。只替换「模型段」，并在标题栏下方、对话内容上方加一行回执区。

| 状态 | 判断 | 模型段 | 回执区 |
|---|---|---|---|
| A 未开始 / 已领取 | `unstarted`、`claimed` | 所选模型，空则「默认模型」（现状） | 无 |
| B 执行中（加载） | `busy(ev)` | 同 A | 现有 `desktop_dispatch` 面板（派发中、等待交接、等待工具审批、重连审批状态），含「打开对应 ZCode 任务」。E8：等待审批期间不显示回执 |
| C 成功，有回执 | 执行目录 `session.json` 含 `providerId`、`modelId` | §2 实际模型文本 | caption「ZCode 桌面实际模型」或「ZCode app-server 实际模型」+ detail_text「会话 <session> · turn <turn>」；若存在 `desktop-create.json`，右侧「打开对应 ZCode 任务」（FR-8） |
| D 结束但无回执（空数据） | 已结束（成功/取消/需对账）且无回执 | ZCode：回退文本；非 ZCode：现状 | 仅 ZCode 显示 caption「未记录实际模型」；有 `desktop-create.json` 仍给「打开对应 ZCode 任务」 |
| E 失败 | `failed` | 有回执用实际，无回执用回退文本 | 红字失败原因（§3.4）；有 `desktop-create.json` 时保留打开按钮，便于在 ZCode 里查看 |

点击「打开对应 ZCode 任务」后的反馈沿用 `zcode_navigation`：按钮下方 caption「正在打开…」，成功后清空，失败显示 `zcode-open` 的 stderr（如 ZCode 未运行、任务已删除，E3）。已完成轮次的回执只读执行目录，不读 ZCode 数据库，ZCode 是否运行不影响 C、D 的显示。

元素 id（供 `ui_debug` 断言）：`turn-model-<attempt>`（模型段）、`turn-receipt-<attempt>`（回执 detail_text）、`zcode-open-<session>`（已存在）。

### 3.2 接力卡（`relay_page.rs:644` 附近）

位置和结构不变，按实现端、审查端（`zcode-edit`、`zcode-plan`）各一段：

| 状态 | 显示 |
|---|---|
| 执行中 | 现状：本端 phase 匹配时显示 `desktop_dispatch` |
| 有回执（桌面或 app-server） | caption「ZCode 桌面实现回执」「ZCode app-server 审查回执」等 + detail_text「§2 文本 · 会话 <session>」，id 沿用 `relay-<key>-<mode>-receipt` |
| 本端已结束无回执 | 本端客户端是 ZCode 时显示 caption「<实现/审查> · 未记录实际模型」；不是 ZCode 不显示 |
| 失败 | `desktop-error.json` 的红字原因（现状）；不符错误同样在这里出现 |

已有回执且存在 `desktop-create.json` 时，在回执行下补「打开对应 ZCode 任务」（目前完成后没有入口）。

### 3.3 事项时间线（`project_page/timeline.rs`）

**卡片标题行**：「<角色> · <客户端> · <阶段>」后追加 caption 模型段：
- 有回执：§2 文本；
- 无回执：所选模型 + 「未记录实际模型」；所选也为空时为「默认模型 · 未记录实际模型」；
- 非 ZCode 客户端：只显示所选模型（为空不显示），不出现「未记录实际模型」。

**展开详情**：把今天的「选择的模型」改为两行：
- 「实际模型」：§2 文本，或「未记录」；
- 「选择的模型」：所选值，空则「客户端默认」。
- 「会话 ID」保持；有回执时追加「 · turn <turn>」。

接力轮次的实现卡与审查卡各自取自己目录的回执（E6）；重试产生的多张卡各自取（E7）。「打开 ZCode 原会话」的显示条件不变（`desktop_dir`）。

时间线加载失败的现有错误展示不变。单个回执文件损坏时不影响整条时间线：该卡按「无回执」显示，展开详情加 caption「实际模型回执无法读取」。

### 3.4 失败文案

| 情况 | 出现位置 | 文案（红字） |
|---|---|---|
| 所选与实际不符（S5、FR-5） | 阶段轮次失败原因 / 接力卡本端失败 | `ZCode 实际模型与所选不符：所选 <provider/model[@档位]>，实际 <方案名 / modelId / 档位>` |
| 缺少主对话用量（FR-4、E2） | 同上 | 沿用现有「缺少 ZCode 主对话模型用量回执」 |
| 同一派发发送多次（E4） | 同上 | 沿用现有文本 |
| 回执写入失败 | 同上 | `无法写入 ZCode 实际模型回执：<原因>` |

失败轮次不产出交接，确认交接类按钮不会出现，与现状一致。失败文本必须同时含两组值，`ui_debug` 用子串断言。

### 3.5 ZCode 任务归组历史（`zcode_history.rs`）

每张会话卡在「<角色> · <run>」caption 下加 `field("实际模型", …)`：§2 文本；无回执显示「未记录」。不加按钮。空历史文案「这个仓库还没有 ZCode 桌面任务记录。」不变。

### 3.6 项目记忆

记忆面板直接显示存入的作者字符串，无需改 UI。新写入的作者为「ZCode / <方案名> / <modelId>@<档位>」，档位为空写「@默认」；客户端默认时追加「 · 客户端默认」。旧记忆作者保持原文（FR-17）。

## 4. CLI 输出

`actual_model` 结构在 `aep project show`、时间线 JSON、`aep zcode-group --list`、evidence.json 中一致（FR-11、FR-12、FR-14）：

```json
"actual_model": {
  "transport": "desktop",
  "plan": "BigModel Start Plan",
  "providerId": "...",
  "modelId": "...",
  "reasoningLevel": null,
  "selected": "",
  "session": "...",
  "turn": "..."
}
```

- 无回执：`"actual_model": null`；原 `model` 字段（所选）保留。
- `plan` 由 `desktop_plan` 计算，不在双方案时等于 `providerId`。
- 人类可读输出用 §2 文本，旧记录显示「未记录实际模型」。

## 5. 验证场景

在 `desktop/examples/ui_debug.rs` 现有 Scenario 夹具上补数据，不新建 live 场景：

| 场景 | 夹具 | 断言 | 对应 |
|---|---|---|---|
| Workflows：Start Plan 成功 | 阶段执行目录写 desktop 回执 + `desktop-create.json` | `turn-model-*` 含「BigModel Start Plan /」；完成后 `zcode-open-*` 存在 | AC-4、AC-5 |
| Workflows：个人 Coding Plan 客户端默认 | 回执 `selected=""`、档位空 | 文本以「/ 默认 · 客户端默认」结尾 | AC-4 |
| Workflows：旧轮次 | 只有 evidence，无回执 | 「未记录实际模型」，无报错 | AC-9 |
| Workflows：不符失败 | `failed` + 不符错误 | 红字同时含「所选」「实际」 | AC-3 |
| StudioRelay：两端两种方案 | `zcode-edit` 用 Start Plan，`zcode-plan` 用个人 Coding Plan | 两个 `relay-*-receipt` 方案名不同；时间线两张卡同样 | AC-6 |
| History | 一条有回执、一条没有 | 「实际模型」分别为文本和「未记录」 | FR-10 |

截图（`--screenshots`）保存 Workflows 成功轮次和 StudioRelay 接力卡两张，供 AC-12 人工比对 ZCode 桌面里的模型。

## 6. 交给技术方案的事项

- 格式函数放在库里（如 `zcode_catalog` 旁），Studio、timeline、CLI、记忆作者共用，保证 §2 只有一个实现。
- 阶段执行回执直接位于 attempt 目录（`zcode_stage.rs:105` 已写 `session.json`），接力位于 `zcode-edit`、`zcode-plan`；§3.1 的状态判定依赖这一约定。
- Q1、Q2 的结论只改变 `selected` 与 `reasoningLevel` 的取值，不改变本文的显示规则。
