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

### 用 AEP Studio 查看运行、编写交接记忆

macOS 上可用 Rust 桌面端浏览执行记录、写入和修订共享记忆。安装 Xcode 及命令行工具，使用当前稳定版 Rust，在仓库根目录运行：

```sh
./desktop/bundle-macos.sh
open "desktop/target/AEP Studio.app"
```

点击「打开仓库」选择执行过 AEP 的 Git 工作区。也可用命令直接指定仓库：

```sh
cargo run --locked --manifest-path desktop/Cargo.toml -- --repo /path/to/repo
```

在「运行记录」筛选待审查或需要处理的 Run。详情列出 Spec、候选提交、验收命令、退出码和审查报告。「Agent 记忆交接」按角色列出读写的记忆 ID。点 ID 查看当时引用的版本，再点「来源 Run」回到写入它的那次运行。「共享记忆」支持搜索正文、查看历史版本和修订链。执行新任务或写入记忆后，点击「刷新」读取最新记录。

在「共享记忆」点「新建记忆」可写入仓库约定，也可限定到任务与 Spec 或某个候选提交。从运行详情点「为此任务写记忆」会预填任务名和提交。保存时会把 Git 引用解析为固定 SHA。启用 `--shared-memory` 的后续执行会按这些范围和上下文预算选取记录。

在当前记忆的详情点「修订」，可以更正正文，或将已回答的「未决问题」改为「决策」。新版本记为人工写入，继承原范围。历史 Run 仍链接到当时读写的版本。如果两人同时修订同一条记录，先保存的版本生效，另一位的草稿留在编辑器中。

记忆详情的「读取记录」列出读取过所打开版本的 Run 和 Agent 角色。点击 Run 可回到对应的执行证据。打开上一版本可查看它自己的读取记录。

桌面端和 CLI 使用所选克隆的同一个本地 SQLite 库，关联 Worktree 共用记录。CLI 可独立安装。

### 在 Studio 发起两轮接力

使用上面的打包版 Studio。先在本机登录 Codex CLI 和 Claude Code，并用 `aep dispatch` 将任务派发到仓库的 `origin`。从源码启动 Studio 时，可将同版本的 `aep` 放入 `PATH`，或将 `AEP_STUDIO_RUNNER` 设为该执行器的绝对路径。

1. 打开仓库，进入「两轮接力」，选中已派发任务并点击「发起第一轮」。可填写 Codex 模型，留空使用客户端默认配置。
2. 第一轮验收和审查完成后，点击未决问题旁的「修订为决策」，编辑并保存。若两个 Agent 都未留下问题，可新建任务决策，或选用当前 Spec 已有的决策。
3. 点击「用此决策发起第二轮」。第二轮从第一轮候选提交继续，沿用同一个 Spec SHA。执行器会把选中的决策版本优先放入两个 Agent 的上下文。如果该版本已被替换或不再适用，本轮会报告具体原因。
4. 接力页自动刷新准备、实现、验收和审查进度。每轮的「执行证据」可打开提交、验收日志、审查报告和记忆读写 ID。失败时可以查看原因和日志，并按轮重试。重试保留之前的尝试，重新启动 Studio 后可从「接力记录」继续查看。两轮完成后候选仍等待人工审查。

### 人工审查并创建 GitHub PR

第二轮通过后，点「查看 Diff 与创建 PR」。确认 `origin` 的取回与推送地址指向同一个 GitHub 仓库，并在本机使用 `gh auth login` 登录有推送和建 PR 权限的账号。

在「最终 Diff」「验收日志」和「审查报告」之间切换即可核对候选。最终 Diff 使用 GitHub 默认分支与候选的合并基点，右侧显示仓库、目标分支和两个固定 SHA。核对后勾选确认并点击「确认并创建 GitHub PR」。目标分支更新时，重新加载 Diff 再确认。

Studio 将候选推送到 `aep/pr/<task>/<run-id>`，并保存确认账号、候选 SHA 和 PR 链接。推送成功而建 PR 失败时，可重开该审查页后重试。Studio 会复用匹配的远端提交，并检查该分支是否已有 PR。GitHub CI 使用已派发的 Spec 和 PR 当前提交独立验收。后续 GitHub 审查与合并由维护者处理。

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

已在本地配置 Codex CLI 和 Claude Code、且仓库内容获准发送给对应模型供应商时，可选择实际客户端。该模式使用各自的模型账户与额度。

```sh
aep run --task market-volume-dedup-rust --implementer codex --reviewer claude
```

Codex 在独立 Worktree 生成候选提交。Claude 接收该版本的 Spec 和 Diff，禁用工具，审查文本保存在同一次运行的 `review.txt`。人工在 GitHub 查看 Diff、CI 和审查意见并决定是否批准。选用现有候选提交时可单独加 `--reviewer claude`。默认走人工审查，不会调用模型。协议测试用本地替身验证适配器调用，不消耗模型额度。

### 两个 Agent 如何共享记忆

在一次执行中加上 `--shared-memory`，Codex 会收到相关工程约定，完成修改后产出交接摘要和可选的未决问题。候选提交验收通过后才会发布这两条内容。Claude 随后读取同一候选提交的摘要，再写下自己的审查摘要。验收失败时，可在该 Run 的证据目录里查看原始交接。独立 Run 或关联 Worktree 也可以读取适用的已发布记录。

```sh
base=$(git rev-parse origin/aep-task/market-volume-dedup-rust)
aep memory add --task market-volume-dedup-rust --spec "$base" --kind decision --text '同一来源的重复事件采用首次记录。'
aep run --task market-volume-dedup-rust --implementer codex --reviewer claude --shared-memory --run-id market-handoff-001
aep status --run-id market-handoff-001
```

如果本机配置的模型不在当前 Codex 账号的可用列表中，可在上述 `run` 命令加上 `--codex-model <模型名>`。该选择只对这次 Run 生效，并记入执行证据。

| 范围 | 适合记录的内容 | 何时会被选中 |
| --- | --- | --- |
| 仓库 | 操作人通过 `memory add --project` 写入的工程约定 | 任一任务可读 |
| 任务与 Spec | 需求决策、跨轮次仍需回答的问题 | Task 和 Spec 提交都相同 |
| 候选提交 | 实现交接、审查摘要 | Task、Spec 和候选 SHA 都相同 |

`memory list --task <task> --spec <sha> --candidate <sha>` 展示该版本可用的上下文。Run 报告中的 `memory_inputs`、`memory_outputs` 分别保存每个角色读到和写入的 ID；用 `memory get --id <id>` 可查原文、来源角色、Run 和提交。用 `memory add ... --replaces <id>` 修订当前范围内的记录，被替代版本仍可按 ID 查询。

记忆保存在 Git 公共目录的 `aep/memory.sqlite3`，同一克隆的 Worktree 共用，独立克隆各自保存。自动传递需显式开启，启用后所选文本会作为提示词发送给该角色所用的模型。每条摘要最多 1,000 个字符，每次上下文最多 12 条、8 KiB，优先选本轮交接和当前候选记录。完整日志、原始会话与详细审查报告保存在本次 Run 的本地证据目录。Agent 留言不代表人工批准。

## 试点边界

- 这版用 Git 远端作为最小控制面。可从 CLI 或 Studio 发起已派发任务。CLI、共享记忆和任务执行器使用 Rust。独立控制面、后台 Daemon、设备身份、Lease 和跨设备接管仍在路线图中。
- 只在受信任的仓库运行。Worktree 隔开文件变更，测试命令和待测代码会以本机用户身份执行。选择外部模型客户端前，须确认其环境与数据权限。
- 同一任务可以有多个 Run。每次都针对固定版本生成证据。用户可以在 Studio 核对证据并确认发布候选，CI 在 PR 当前提交上独立复验。
- 本仓库用作流程试点与基线。业务收益、费用和可靠性由后续真实需求测量。[AEP 架构提案与八周路线图](https://app.notion.com/p/41a9d9f6f60581d08560fa33ed8c4810)

## 开发验证

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked --all-targets
```

[试点 Spec 与六条验收用例](specs/market-volume-dedup-rust/spec.md) · [任务清单与验收命令](specs/market-volume-dedup-rust/task.json)

### 离屏调试桌面界面

在 macOS 的仓库根目录运行以下命令，即可用生产版 Studio 视图验证记忆修订、并发冲突和 Run 与记忆的双向跳转。每个场景自建临时 Git 仓库，用 GPUI 模拟点击和键盘输入。测试使用本地 SQLite 和合成的运行证据，无需桌面控制权限或模型账号。

```sh
cargo run --locked --manifest-path desktop/Cargo.toml --example ui_debug
```

可用 `--scenario history`、`conflict`、`handoff` 或 `studio-relay` 单独运行。后者从 Studio 选任务发起两轮，并回归失败重试、修订、证据跳转、人工确认和 GitHub PR 发布恢复。要查看某一步的实际画面，添加输出目录和截图选项。

```sh
cargo run --locked --manifest-path desktop/Cargo.toml --example ui_debug -- \
  --scenario conflict --output-dir desktop/target/ui-debug --screenshots
```

`handoff` 会在临时仓库中通过 AEP CLI 派发 Rust 小任务，由两个本地替身进程完成实现和审查。审查进程只有在收到实现 Agent 的交接标识后才会返回报告。UI 随后打开执行器生成的 SQLite 记忆、验收日志和审查报告。调试流程还会在 Studio 将第一轮的未决问题修订为决策，再运行同一 Spec 的新候选提交。第二轮的两个 Agent 必须收到新决策。每个版本的读取记录都可以跳回对应 Run。

要用真实的 Codex 和 Claude 复跑两轮，在已登录两个客户端且支持 Metal 的 Mac 上运行以下命令。命令会使用模型账户额度。可以省略 `--codex-model` 来使用本机 Codex 配置的默认模型。

```sh
cargo run --locked --manifest-path desktop/Cargo.toml --example ui_debug -- \
  --live-relay --codex-model gpt-6.1-sol --output-dir desktop/target/live-relay
```

每次运行会在指定目录下创建新的 `live-relay-*` 子目录。Studio 将第一轮的问题修订为带唯一标识的决策。命令会核对第二轮实现和审查的输入 ID、两个客户端对该标识的确认，以及两个版本的 Studio 读取记录。成功结果包括 `verification.json`、两轮 Run 的证据和报告、`studio-revision.png` 及新旧版本的截图。失败会返回非零退出码，保留已有证据与截图，并将原因写入 `failure.txt`。

结果目录里的 `workspace/repo` 保存了合成 Git 仓库、两个候选提交、SQLite 记忆和原始 Run 证据。可在 Studio 点「打开仓库」选择这个目录，继续查看两轮证据。终端会打印完整路径，也可传给以下命令。

```sh
cargo run --locked --manifest-path desktop/Cargo.toml -- \
  --repo /path/to/live-relay-result/workspace/repo
```

已在本机用模型客户端跑完一次交接时，可以验证那次运行的界面。将 `--run-id` 改为实际运行 ID。

```sh
cargo run --locked --manifest-path desktop/Cargo.toml --example ui_debug -- \
  --repo /path/to/repo --run-id market-handoff-001 --output-dir desktop/target/ui-debug
```

每个检查点输出一份 JSON，包含控件的 ID、标签、值、可见性和窗口坐标。`--screenshots` 使用离屏渲染器生成同名 PNG，需要可用的 Metal 设备。断言失败时命令返回非零退出码。指定输出目录后会尝试写入 `failure.json` 和所选截图。

新交互可以用 JSON 步骤回放。下面的示例从界面创建一条记忆，然后验证搜索结果会随输入变化。

```sh
cargo run --locked --manifest-path desktop/Cargo.toml --example ui_debug -- \
  --script desktop/examples/memory-search.json --output-dir desktop/target/ui-debug
```

脚本从包含一条 `sample-run` 的新仓库开始。初始状态存为 `script-000.json`，之后每步递增。使用快照中的唯一 ID 做 `click`、`present`、`absent`，或用唯一标签做 `click_label`。此外支持 `input`、`press`、`scroll`（`id` 和像素增量 `dy`）和 `expect_value`（`label` 和 `equals`）。macOS CI 会运行四个内置场景和仓库内的搜索脚本。
