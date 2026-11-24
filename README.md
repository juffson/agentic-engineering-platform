# Agentic Engineering Platform · AEP

AEP 的首个试点把 Git 中的需求、隔离工作区中的代码改动和针对同一提交的验收证据连起来。GitHub 保存任务和 PR，执行器在本地运行。示例只使用合成行情数据。

## 从一次派发到一个可审查的提交

需要 macOS 或 Linux、Git、Rust 1.89+、Cargo 和 C 编译器（用于编译内置 SQLite）。在仓库根目录安装 CLI，然后验证已派发的 Rust 行情示例。

```sh
cargo install --locked --path .
git fetch origin
aep run --task market-volume-dedup-rust --candidate origin/aep-task/market-volume-dedup-rust
```

命令会读取 GitHub 上的 `aep-task/market-volume-dedup-rust` 分支，在独立 Worktree 验证候选提交，打印本地 `evidence.json` 路径。报告包含 Spec SHA、候选 SHA、修改文件、测试命令、退出码与审查状态。失败的工作区会保留供排查。核对报告后可用 `git worktree remove <路径>` 清理。执行器核验通过后保留候选引用 `refs/aep/runs/<run_id>`；本地报告位于 Git 公共目录的 `aep/runs/` 下。

已有仓库可直接使用原 Git 公共目录中的 SQLite 记忆库、Run 锁和 JSON 证据。历史派发继续使用其固定提交里声明的验收命令。新的行情示例使用独立任务 ID `market-volume-dedup-rust`。

### AEP Studio 项目工作台

Studio 是 Tauri 桌面应用（`studio/`）：React 界面加一层直接调用 `aep` 核心的 Rust 命令。需要 Rust、Node.js 和 Xcode 命令行工具，在仓库根目录运行：

```sh
./studio/bundle-macos.sh
open "studio/src-tauri/target/debug/bundle/macos/AEP Studio.app"
```

脚本会先构建 `aep` 执行器并放进应用包；从源码运行时也可设置 `AEP_STUDIO_RUNNER` 指向执行器。

| 模板 | 阶段 |
| --- | --- |
| 软件开发 | 产品需求 → 交互设计 → 技术方案 → 开发实现 → 测试验证 → 独立验收 → 交付归档 |
| 游戏开发 | 玩法需求 → 影响分析 → 环境预检 → 游戏实现 → 运行验证 → 玩家验收 → 交付归档 |

界面围绕一条主线：左栏选事项（进行中 / 已交付 / 已归档），中间是当前阶段的会话，右侧是下一步决定。

- **阶段会话**：Agent 输出按终端方式呈现，命令和文件改动折叠成一行，可展开看输出。底部输入框首次运行阶段，之后在同一会话里继续对话。
- **执行配置**：每个阶段可选 Codex、Claude、ZCode 或自定义 CLI，模型（Codex 读取本机 CLI 可用模型，ZCode 读取本机模型目录）和权限（默认沙箱 / 允许网络 / 完全权限）。
- **推进与退回**：通过后确认进入下一阶段；测试或验收不通过时，一键退回负责的前序阶段并立即开始修复，也可写明理由人工放行。可选的规划阶段可以跳过。
- **自动运行**：选好各阶段的 CLI、模型和终点，剩余阶段自动执行；问题按 `AEP-BACK` 退回对应阶段，同一对阶段最多自动往返两次，`BLOCKED` 或 CLI 故障会停下等人。
- **工作区与提交**：同一事项的所有阶段共用一个工作树（分支 `aep-stage/<事项ID>`）。规划和开发阶段通过后，平台自动把改动提交到该分支；测试和验收针对固定提交。
- **交付**：交付后在右侧预览 Diff，合并到本地目标分支（合并的是独立验收确认的那个提交）；再把要点写入项目记忆并归档，可同时移除工作树（分支保留）。
- **历史**：事项的每次执行、读写的记忆、阶段交接和合并记录；也可以给同项目的其他事项发送交接摘要。
- **记忆**：「项目记忆」对项目内所有阶段生效；「仓库」页可查看和补充仓库记忆，阶段同样会读取。

#### 事项交接

在事项的「历史」里给同一项目的另一事项发送摘要后，接收方能查看来源事项、阶段和结论；下一次阶段执行或独立 CLI 预览会带上最近的交接。每个来源保留最新一条摘要。

```sh
aep project --work 对账单导出 handoff --to 发布验收 --note '空对账单场景已实现，验收见 fixtures/empty.csv'
aep project --work 发布验收 context
aep project --work 对账单导出 link-record --repository . relay RELAY_ID
aep project --work 对账单导出 status
aep project site --repository . --endpoint https://control.example.org --operator-file /path/to/operator
```

#### 在已有 Agent 会话里交接

`aep project` 与 Studio 使用同一份项目库。事项间的定向交接保存在项目库。项目共享决策和阶段 Agent 写入的交接摘要存在每个项目自己的 SQLite 中，完整的阶段结果、执行回执和主动提交的文件按执行 ID 归档。默认给下一位 Agent 的内容包括当前职责、阶段说明、有效决策、上游交接摘要和档案位置。可用 `project memory get ID` 读取完整记忆。

在其中一个 Git 工作区建立项目和事项后，可把当前工作树绑定到它们。绑定存于该工作树的 Git 元数据目录。多个工作树可以绑定不同事项。

```sh
aep project create 支付平台 --repository "$PWD"
aep project add-work 对账单导出
aep project bind
aep project context
```

在已经打开的 Claude、Codex、ZCode 或其他终端会话中，领取时填写会话名或原客户端的会话 ID。会话继续在自己的工作目录工作。Studio 事项页面也可复制领取命令。外部会话只需能执行本机的 `aep`。

```sh
aep project claim --stage 1 --client claude --session prd-design
```

领取返回 `attempt`、`context` 和项目记忆游标 `next_since`。在下列命令中将 `RUN_ID` 和 `CURSOR` 替换成回执值。`context --since` 只返回本次领取适用的项目记忆变更，`has_more` 为真时用新的游标继续读。已领取的阶段可以重复查询。没有变更时交接文本为空。

```sh
aep project context --attempt RUN_ID --since CURSOR
aep project submit --attempt RUN_ID --session prd-design --file prd.md --summary '范围已定，AC-1 涵盖空对账单，下一步设计导出交互。' --artifact prd.md
aep project show --attempt RUN_ID --artifact prd.md
aep project advance --stage 1
```

`--artifact` 可重复，路径相对于领取时的工作目录，文件以提交当时的内容归档。测试与独立验收通过时在结果末行写 `AEP: PASS`，有阻断时用 `submit --blocked` 提交原因。取消外部领取会关闭该次交接，原客户端独立运行。失败或取消后用 `claim --retry` 重新领取。

直接从命令行托管模型时，用 `project configure --stage N` 设置客户端、模型、工作仓库和说明，再用 `project run` 执行当前阶段。人工步骤可用 `advance --stage N --note '结论'` 完成。`list` 跨项目查看当前负责人，`status` 查看事项和历次执行，`metrics` 查看每次执行的实际输入字节数、耗时和客户端返回的用量字段。外部会话的推理用量由原客户端记录。

默认共用本机 Studio 项目库。需要单独存储时，在启动 Studio 和 CLI 前设置相同的绝对目录 `AEP_PROJECT_HOME`，CLI 也接受 `--home`。在其他目录中可用 `--project` 和 `--work` 指定名称或 ID。上述本地交接要求参与方可访问同一份项目库和档案。

项目库保存在本机的 `~/Library/Application Support/AEP Studio/projects.json`，阶段执行档案在同目录的 `workflows/` 下，事项工作树在仓库的 `.git/aep/workflow-worktrees/`。CLI 的 `aep memory` 命令可按任务、Spec 或候选提交范围读写仓库记忆。

### 重复派发与本地恢复

派发方可以给一次执行指定稳定的 `--run-id`。在同一设备和同一仓库（包括它的其他 Worktree）重送消息时，沿用该 ID。以下示例可执行两次；第二次复用第一次的证据。

```sh
aep run --task market-volume-dedup-rust --candidate origin/aep-task/market-volume-dedup-rust --run-id market-review-001
aep status --run-id market-review-001
```

`status` 离线读取本地 JSON。并发重复派发返回退出码 75，表示执行权暂时被占用。运行结束后再次派发，成功返回 0，失败返回原失败信息及退出码 1。相同 ID 绑定任务、Spec 提交、候选提交或实现客户端、审查方式和超时。候选分支移动或参数变化时，使用新的 ID。

若执行器在写入最终结果前退出，`status` 显示 `needs_reconciliation`。先检查报告中的 `worktree`、候选提交和本地子进程，再决定是否以新 ID 发起下一次尝试。成功结束的工作区会被清理。省略 ID 仍会创建新的 Run。历史版本的证据可用 `status` 读取。

### 新任务

在 `specs/<task>/` 编写 `spec.md` 和 `task.json`，声明准确的可修改文件和验收命令，提交后由维护者派发。首次派发新需求时，指定经过评审的版本：

```sh
aep dispatch --task <task> --rev <reviewed-sha>
```

派发将远端任务分支固定到本次 Spec 提交。相同版本可重复派发，已存在的任务引用不能通过该命令改指其他版本。修订需求需使用新的任务 ID。维护者应限制任务分支的写权限。

### 客户端执行

已在本地配置 Codex CLI、Claude Code 或 ZCode、且仓库内容获准发送给对应模型供应商时，可选择实际客户端。该模式使用各自的模型账户与额度。实现端可选 Codex 或 ZCode，审查端可选 Claude 或 ZCode。

```sh
aep run --task market-volume-dedup-rust --implementer codex --reviewer claude
aep run --task market-volume-dedup-rust --implementer zcode --reviewer manual
```

Codex 在独立 Worktree 生成候选提交。Claude 接收该版本的 Spec 和 Diff，禁用工具，审查文本保存在同一次运行的 `review.txt`。ZCode 实现端以编辑模式执行附件中的任务，审查端以计划模式只读核对 Spec 与 Diff。指定普通 CLI 模型时 ZCode 通过本机 app-server 锁定模型。BigModel 桌面方案使用下文的桌面任务交接。人工在 GitHub 查看 Diff、CI 和审查意见并决定是否批准。选用现有候选提交时可单独加 `--reviewer claude` 或 `--reviewer zcode`。默认走人工审查，不会调用模型。协议测试用本地替身验证适配器调用，不消耗模型额度。

### 两个 Agent 如何共享记忆

在一次执行中加上 `--shared-memory`，Codex 会收到相关工程约定，完成修改后产出交接摘要和可选的未决问题。候选提交验收通过后才会发布这两条内容。Claude 随后读取同一候选提交的摘要，再写下自己的审查摘要。验收失败时，可在该 Run 的证据目录里查看原始交接。独立 Run 或关联 Worktree 也可以读取适用的已发布记录。

```sh
base=$(git rev-parse origin/aep-task/market-volume-dedup-rust)
aep memory add --task market-volume-dedup-rust --spec "$base" --kind decision --text '同一来源的重复事件采用首次记录。'
aep run --task market-volume-dedup-rust --implementer codex --reviewer claude --shared-memory --run-id market-handoff-001
aep status --run-id market-handoff-001
```

使用 BigModel 桌面方案时，先安装 AEP 的 ZCode 扩展。在 AEP 仓库根目录运行：

```sh
zcode=/Applications/ZCode.app/Contents/Resources/glm/zcode.cjs
"$zcode" plugins marketplace add "$PWD/integrations/zcode"
"$zcode" plugins install aep-studio@aep-local
```

打开 ZCode，在插件详情页给 aep-studio 授予会话权限。若已打开的 ZCode 尚未载入插件，可重新打开。之后用 `aep run --implementer zcode` 或把阶段的 CLI 选为 ZCode 即可。官方扩展按 ZCode 的新任务默认权限在对应本地项目中创建任务并发送本轮 Spec、Diff 和共享记忆。实现任务使用隔离工作区。桌面方案使用模型默认推理档位，旧配置中的手选档位在新派发时也按默认处理。普通 CLI 模型仍可指定档位。

AEP 派发的 ZCode 实现和审查会话归到所选仓库对应的本机 ZCode 项目。实现仍在各自的 Git 工作区执行。建会话使用官方扩展；归组只更新 ZCode 本机项目索引中的任务归属，并固定隔离任务原有的附加目录。每次派发的归组结果在 `desktop-group.json`，原创建回执继续保留。归组后，AEP 会从 ZCode 侧栏移除本次派发创建的空 `aep-work-*` 项目。清理与任务数量检查在同一个事务内完成；有任务记录（含未命名、归档和预留记录）、定时任务或闲时任务的项目会保留。实现会话引用的隔离工作区会在验收后保留，便于重新打开原任务。

用 `aep zcode-group --list` 查看实际归属项目、执行目录和原项目的清理结果；`aep zcode-group` 同步历史任务并清理空项目。

在「开发事项 → 会话与轮次」查看每次接力的实现、审查、读写的记忆版本及交接正文，或打开原 ZCode 桌面会话。同轮重试按执行顺序分别保留。在「协作与交接」可以为整条接力指定阶段，也可以在「项目设置 → ZCode 任务归组 → 关联事项与阶段」为单个会话指定阶段。阶段工作流本身创建的会话自动沿用原事项和阶段。旧接力没有阶段回执时会显示「阶段待关联」。

CLI 示例（阶段从 1 开始）：

```sh
aep project --work "$work_id" link-record --repository . --stage 4 relay "$relay_id"
aep project --work "$work_id" link-session --repository . --stage 4 "$session_id"
aep project --work "$work_id" timeline
```

已有关联记录可在仓库目录运行 `aep zcode-group --verify` 归回并核对。命令按 AEP 回执识别任务，重复运行会复用原会话。`--verify` 通过 aep-studio 扩展 1.2 或更高版本向 ZCode 查询该项目的会话列表。

ZCode 等待工具审批时，在对应的 ZCode 桌面会话点「允许一次」，原任务会自动续跑。ZCode 扩展仅能在插件自有项目中代答审批。AEP 的仓库和隔离工作区使用桌面原生确认。可用 `aep zcode-open --dir <本次运行的 zcode-edit 或 zcode-plan 目录>` 打开同一任务。已安装旧版扩展的用户可运行 `"$zcode" plugins marketplace update aep-local` 和 `"$zcode" plugins update aep-studio@aep-local`，再重新打开 ZCode。

本地接力只在 ZCode 持续报告同一会话正在等待审批时暂停该模型步骤的超时计时，批准或拒绝后继续计时。对于旧版已在实现阶段超时的运行，待原桌面任务完成后，用相同的 `aep run` 参数和 `--run-id` 加上 `--recover-desktop` 可收取原交接。AEP 保留原超时记录和工作区，在独立工作区执行该任务的验收和审查。

开始第二轮时，AEP 会把第一轮的运行 ID 与起始提交一起传给执行器。实现端和审查端都能读到该轮已发布的交接与审查记录，即使第二轮产生了新提交。记忆仍受任务、Spec 和大小上限约束，输入编号可在 `evidence.json` 的 `memory_inputs` 核对。

每轮的 `zcode-edit` 或 `zcode-plan` 目录保留派发请求、扩展创建回执和最终回执。AEP 按目录、派发消息、会话和主对话用量核对方案与模型，回执中记录任务 ID、实际推理档位和交接摘要。

CLI 可用 `aep project zcode-models` 列出本机选项，`aep project zcode-default` 查看项目默认，`aep project zcode-default --model provider/model@档位` 保存，`--clear` 清除。

如果本机配置的模型不在当前 Codex 账号的可用列表中，可在上述 `run` 命令加上 `--codex-model <模型名>`。该选择只对这次 Run 生效，并记入执行证据。ZCode 的实现端或审查端可用 `--zcode-model provider/model` 或 `provider/model@推理档位` 指定模型，同样只对本次 Run 生效并记入证据。用 `--reviewer-model <模型>` 可以单独指定 Claude 或 ZCode 的审查模型。ZCode 的交接摘要和未决问题由最终答复末行的 `AEP-RELAY: {json}` 返回，验收通过后与 Codex 的结构化交接进入同一发布链路，审查端同样按 criterion、文件和行号校验阻断发现。

| 范围 | 适合记录的内容 | 何时会被选中 |
| --- | --- | --- |
| 仓库 | 操作人通过 `memory add --project` 写入的工程约定 | 任一任务可读 |
| 任务与 Spec | 需求决策、跨轮次仍需回答的问题 | Task 和 Spec 提交都相同 |
| 候选提交 | 实现交接、审查摘要 | Task、Spec 和候选 SHA 都相同 |

`memory list --task <task> --spec <sha> --candidate <sha>` 展示该版本可用的上下文。Run 报告中的 `memory_inputs`、`memory_outputs` 分别保存每个角色读到和写入的 ID；用 `memory get --id <id>` 可查原文、来源角色、Run 和提交。用 `memory add ... --replaces <id>` 修订当前范围内的记录，被替代版本仍可按 ID 查询。

记忆保存在 Git 公共目录的 `aep/memory.sqlite3`，同一克隆的 Worktree 共用，独立克隆各自保存。自动传递需显式开启，启用后所选文本会作为提示词发送给该角色所用的模型。每条摘要最多 1,000 个字符，每次上下文最多 12 条、8 KiB，优先选本轮交接和当前候选记录。完整日志、原始会话与详细审查报告保存在本次 Run 的本地证据目录。Agent 留言不代表人工批准。

## 试点边界

- Git 远端固定 Spec。也可启动 Rust 控制面和独立 Worker，在一个仓库中向独立 Worker 派发和恢复任务。HTTPS 控制面支持多 Worker 并行。Worker 主动连接，控制面持久化 Lease、执行事件和批准记录。[控制面启动与试点手册](docs/pilot.md)
- 只在受信任的仓库运行。Worktree 隔开文件变更，测试命令和待测代码会以本机用户身份执行。选择外部模型客户端前，须确认其环境与数据权限。
- 同一任务可以有多个 Run。每次都针对固定版本生成证据。事项交付时合并的是独立验收确认的提交，CI 在 PR 当前提交上独立复验。
- 本仓库用作流程试点与基线。业务收益、费用和可靠性由后续真实需求测量。[AEP 架构提案与八周路线图](https://app.notion.com/p/41a9d9f6f60581d08560fa33ed8c4810)

## 开发验证

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked --all-targets
```

[试点 Spec 与六条验收用例](specs/market-volume-dedup-rust/spec.md) · [任务清单与验收命令](specs/market-volume-dedup-rust/task.json)

### Studio 验证

```sh
cargo build --locked --bin aep
cargo test --locked --manifest-path studio/src-tauri/Cargo.toml   # 后端命令端到端：真实 Git 仓库 + 替身 CLI
cd studio && npx -y pnpm@9 install && npx -y pnpm@9 build && npx -y pnpm@9 test:ui
```

`test:ui` 用 Playwright 在浏览器里跑界面主流程（无 Tauri 时自动使用模拟后端），截图保存在 `studio/tests/screenshots/`。
