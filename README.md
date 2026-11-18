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

在一次执行中加上 `--shared-memory`，Codex 会收到相关工程约定，完成修改后留下一条交接摘要和可选的未决问题。Claude 在检查通过后读取同一候选提交的摘要，再写下自己的审查摘要。独立 Run 或关联 Worktree 也可以读取适用的记录。

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

- 这版用 Git 远端作为最小控制面，按任务名手动触发本地执行。CLI、共享记忆和任务执行器使用 Rust。独立控制面、后台 Daemon、设备身份、Lease、断线接管和桌面端仍在路线图中。
- 只在受信任的仓库运行。Worktree 隔开文件变更，测试命令和待测代码会以本机用户身份执行。选择外部模型客户端前，须确认其环境与数据权限。
- 同一任务可以有多个 Run。每次都针对固定的版本生成证据，由人显式推送候选提交并建 PR。CI 在 PR 当前提交上独立复验，平台不会代替人批准或合并。
- 本仓库用作流程试点与基线。业务收益、费用和可靠性由后续真实需求测量。[AEP 架构提案与八周路线图](https://app.notion.com/p/41a9d9f6f60581d08560fa33ed8c4810)

## 开发验证

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked --all-targets
```

[试点 Spec 与六条验收用例](specs/market-volume-dedup-rust/spec.md) · [任务清单与验收命令](specs/market-volume-dedup-rust/task.json)
