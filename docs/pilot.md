# AEP 单仓库试点

试点包含两条入口。Studio 管理本机任务队列、两轮记忆接力、PR、CI 和收尾。需要独立控制面时，用 Rust CLI 派发，并由本地 Worker 主动领取。Studio 可打开该 Worker 的仓库，查看 Run、验收和记忆。控制面跟踪到经人工确认发布的 PR 为止，最终合并由仓库维护者确认。[架构方案与故障验收](https://app.notion.com/p/41a9d9f6f60581d08560fa33ed8c4810)

## 本地计划、验收与交付

在 Studio 打开仓库后选择「本地任务」。填写任务票据，从列表选本地目标分支，点击「编写 Spec」。填写任务 ID、标题、Spec、允许修改的文件和验收命令，也可以从已有 Spec 提交生成计划。保存时固定目标提交、Spec、文件范围和验收编号。新写的 Spec 保存在仓库的 `refs/aep/local-specs/` 中，当前工作区、暂存区和目标分支保持原样。

先批准计划，再让 Codex 在独立 Worktree 实现，由本地命令验收，最后交给 Claude 审查。失败后可从原票据重试，新的 Run 会带上上一轮的检查状态和阻断意见。点击历史计划版本或 Run 可查阅对应 Spec、实现日志、验收和审查；点击「当前计划」可回到待审批的版本。用「修订 Spec」也可修改 Codex 模型。新版本须重新批准计划，之前的批准留在原版本。

核对 Diff、验收日志和审查后，分别确认交付和本地合并。目标工作区须干净且处在计划指定的分支上。合并会复验 Spec、Run、候选引用、目标和证据摘要。若合并回执丢失，可以重试同一个操作。Spec 或目标变化时，修订计划并重新审批。

相同流程也可从空闲的本地验证仓库调用。以下命令中的 SHA 和版本取自上一步输出。检查 `inspect` 的证据后再执行交付批准。

`aep local targets` 列出目标分支和基线。编写新 Spec 时运行 `aep local author --ticket market-fix --target main --baseline "$base" --draft draft.json`，其中 JSON 包含 `name`、`spec` 和 `manifest`（与 `task.json` 格式相同）。修订时再传入 `--version` 指定当前版本。`aep local inspect --ticket market-fix --version 1 --run "$run"` 可查看历史证据。下面用现有 Spec 提交演示同一交付流程。

```sh
aep local plan --ticket market-fix --task market-volume-dedup-rust --spec "$spec" --target main
aep local approve-plan --ticket market-fix --version 1 --spec "$spec" --baseline "$base"
aep local run --ticket market-fix
aep local inspect --ticket market-fix
aep local approve-delivery --ticket market-fix --version 1 --spec "$spec" --baseline "$base" \
  --run "$run" --candidate "$candidate" --report-sha256 "$report" \
  --human-minutes 8 --baseline-minutes 12
aep local merge --ticket market-fix --version 1 --spec "$spec" --baseline "$base" \
  --run "$run" --candidate "$candidate"
aep local report
```

`plan` 可设置 `--attempts`、`--timeout`、`--codex-model` 和可选的 `--max-tokens`、`--max-cost-micros`。超时预算覆盖单轮 Agent 和验收步骤。用量和费用根据提供方回执累计，在下一次模型调用和交付前检查。费用单位为百万分之一美元，缺失回执单独显示。模型账户额度负责请求执行中的费用上限。人工耗时用实际记录填写。

`cargo run --locked --manifest-path desktop/Cargo.toml --example ui_debug -- --scenario local` 通过 Studio 操作一个本地验证仓库，覆盖创建、修订、验收失败、重开、重试、查阅历史和合并。带 `--output-dir` 和 `--screenshots` 可保存界面和两次 Run 的归档。

## 启动控制面和设备

在私有 Docker/Linux VM 里运行完整 HTTPS 试点时，使用[部署与验收手册](pilot-https.md)。下面的直接启动方式适合接入已有代理和沙箱。

安装同一版本的 `aep`。为操作人和设备各生成一个独立随机令牌，在服务器的进程环境中设置 `AEP_CONTROL_OPERATOR_TOKEN` 和 `AEP_CONTROL_DEVICE_TOKEN`。设备只领取设备令牌，操作端使用操作人令牌。将持久文件放在由服务用户拥有的私有目录。

```sh
aep control serve --listen 127.0.0.1:8787 \
  --state /private/aep/control.json --device pilot-mac
```

服务器绑定回环地址。跨机器试点时，用环境已有的 HTTPS 反向代理暴露 `/v1/control`，并将以下命令的 `--url` 改为该 HTTPS 地址。服务重启必须使用同一状态文件、设备 ID 和授权配置。Worker 的安装标识保存在该克隆的 Git 公共目录，重启时使用相同克隆。控制面存储任务、版本引用、设备和客户端版本、事件、批准与 PR 编号。命令输出、Diff、提示词、模型对话和 SQLite 记忆留在执行设备。

需要上传证据时，可在服务端启用 `--require-evidence-grant` 并在 Worker 配置 `--upload-evidence`。用 `queue-pause --paused true` 暂停派发，提交任务后为回执中的准确 Run 执行 `control evidence-grant --ticket <ticket> --run <run> --seconds 3600`，再恢复队列。批准记录包含操作人、Run、固定证据包范围和到期时间。固定证据包可含日志、审查、记忆库和候选 Git bundle，授权前先核对可发送的数据。Worker 先检查授权再上传，过期后控制面拒绝访问并清理对应的上传文件；授权记录和 Worker 原始证据继续保留。

在执行设备准备试点克隆，安装 Codex CLI 和 Claude Code，配置已批准的模型账号和该仓库的只读访问方式。启动 Worker。关闭 Studio 不会中断它。

```sh
aep --repo /path/to/repo control --url https://aep.example.com worker
```

可加 `--once` 处理一个 Run 或补报原 Run。可通过 `--sandbox-launcher /path/to/launcher` 接入站点的沙箱，启动器需按收到的可执行文件和参数运行，并保留子进程退出状态。先用合成凭证核验对无关文件、SSH、GitHub 凭证和网络的限制。Worktree 提供文件隔离，Worker 从执行器环境移除控制面令牌及常见 GitHub 凭证变量。需要受限执行的仓库还须使用独立账号或已验证的沙箱，因为本地客户端和测试命令具备执行代码的权限。

## 派发、审查和发布

将 Spec 和验收规则提交到 Git，用现有 `aep dispatch --task <task> --rev <spec-sha>` 固定派发。使用现有隔离目标分支的完整 SHA 作为基线。控制面启动后，操作人可用以下命令安排任务。

```sh
aep --repo /path/to/repo control --url https://aep.example.com submit \
  --ticket volume-fix --task volume-v1 --spec "$spec_sha" \
  --target "$target_branch" --baseline "$target_sha" --timeout 600 --max-attempts 3
aep control --url https://aep.example.com status
```

同一派发重复提交返回同一个 Run。Worker 本地记录每个事件的序号，收到确认才推进游标。连接中断时本机证据继续落盘。过期 Lease 的事件只补写历史，旧 Run 无法获得交付授权。若已生成候选，可以用 `retry --ticket volume-fix --reverify` 查找原 SHA 并用新 Run 独立重跑验收和审查。普通 `retry` 重新实现。取消使用 `cancel --ticket volume-fix`，运行中的任务待设备确认进程退出后显示已取消。

打开 Studio 的运行记录，或用 `aep status --run-id <run>` 核对证据，再查看对应目录的 `checks.log` 和 `review.txt`。操作端使用同一设备上包含候选引用和证据的仓库，并使用具备目标仓库写权限的 GitHub 账号。

```sh
aep --repo /path/to/repo control --url https://aep.example.com approve \
  --ticket volume-fix --run "$run_id" --spec "$spec_sha" --candidate "$candidate_sha"
aep --repo /path/to/repo control --url https://aep.example.com publish \
  --ticket volume-fix --run "$run_id" --spec "$spec_sha" --candidate "$candidate_sha"
```

批准记录保存 Run、Spec、候选 SHA 及本地验收和审查证据的 SHA-256。发布会复核这些证据、GitHub 目标和已派发 Spec。丢失回执时重复相同命令，系统会查询已创建的 PR。发布前如需修改需求，派发新任务版本，然后使用相同 ticket 加 `submit --revise`，从新 Spec 重新验收、审查和批准。原批准仍可查阅。候选或必需 CI 检查变化时，由维护者核对当前 PR head 后再合并。

## 按验收项记录证据

现有任务的 `check` 命令数组继续可用。可在 `task.json` 声明主命令覆盖的验收编号以及额外必需检查。Studio 的「验收命令」也接受包含下面三个字段的 JSON 对象。

```json
{
  "check": ["cargo", "test", "--locked", "--test", "market_volume"],
  "criteria": ["AC-1", "AC-2", "AC-3"],
  "checks": [
    {"id": "format", "criteria": ["AC-4"], "command": ["cargo", "fmt", "--all", "--", "--check"]}
  ]
}
```

Spec 写明每个编号的预期结果。Run 证据将各项绑定到固定 Spec 与候选 SHA，记录参数、退出状态和本地日志。必需检查失败后，剩余项标记为未执行。编号化任务的 Claude 审查会分开记录带文件和行号的阻塞项。阻塞结果会退回实现，已有测试日志可继续查看。模型客户端提供的用量和报价写进本地 `usage`，订阅和缺失数据按实际口径单独记录。

## 核验与试点观测

- `cargo test --locked --all-targets` 覆盖重复派发、旧 Lease 补报和发布被拒、旧批准失效、控制服务与 Worker 恢复、回执丢失、取消进程组和必需检查失败。
- `cargo run --locked --manifest-path desktop/Cargo.toml --example ui_debug` 离屏运行 Studio。任务队列、并发、取消与恢复还有桌面端单元测试。
- 真实 PR 可核对候选 SHA、分开的确定性测试与 Agent Review、一次实际人工批准及 GitHub CI。[AEP 隔离试跑 PR #34](https://github.com/juffson/agentic-engineering-platform/pull/34)

试点评估逐个记录需求规模、人工分钟、从确认到批准的耗时、返工、未交付原因和全部尝试的模型用量。待负责人确认真实数据范围、沙箱和费用上限后，再与同仓库、同 CI 的常规交付基线比较。单次流程实跑只验证闭环和故障恢复。
