# AEP 单仓库试点

试点包含两条入口。Studio 管理本机任务队列、两轮记忆接力、PR、CI 和收尾。需要独立控制面时，用 Rust CLI 派发，并由本地 Worker 主动领取。Studio 可打开该 Worker 的仓库，查看 Run、验收和记忆。控制面跟踪到经人工确认发布的 PR 为止，最终合并由仓库维护者确认。[架构方案与故障验收](https://app.notion.com/p/41a9d9f6f60581d08560fa33ed8c4810)

通过 HTTPS 派发的任务在 Studio 中分别确认发布和合并，并保留两项回执。

## 启动控制面和设备

在私有 Docker/Linux VM 里运行完整 HTTPS 试点时，使用[部署与验收手册](pilot-https.md)。下面的直接启动方式适合接入已有代理和沙箱。

安装同一版本的 `aep`。为操作人和设备各生成一个独立随机令牌，在服务器的进程环境中设置 `AEP_CONTROL_OPERATOR_TOKEN` 和 `AEP_CONTROL_DEVICE_TOKEN`。设备只领取设备令牌，操作端使用操作人令牌。将持久文件放在由服务用户拥有的私有目录。

```sh
aep control serve --listen 127.0.0.1:8787 \
  --state /private/aep/control.json --device pilot-mac
```

服务器绑定回环地址。跨机器试点时，用环境已有的 HTTPS 反向代理暴露 `/v1/control`，并将以下命令的 `--url` 改为该 HTTPS 地址。服务重启必须使用同一状态文件、设备 ID 和授权配置。Worker 的安装标识保存在该克隆的 Git 公共目录，重启时使用相同克隆。控制面存储任务、版本引用、设备和客户端版本、事件、批准与 PR 编号。命令输出、Diff、提示词、模型对话和 SQLite 记忆留在执行设备。

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
