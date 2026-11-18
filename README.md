# Agentic Engineering Platform · AEP

AEP 的首个试点把 Git 中的需求、隔离工作区中的代码改动和针对同一提交的验收证据连起来。GitHub 保存任务和 PR，执行器在本地运行。示例只使用合成行情数据。

## 从一次派发到一个可审查的提交

需要 Git、Python 3.9+ 和 macOS 或 Linux。在克隆后的仓库根目录运行以下命令。这两条命令只需要远端仓库的读取权限。

```sh
git fetch origin
python3 aep.py run --task market-volume-dedup --candidate origin/pilot/market-volume-dedup
```

命令会读取 GitHub 上的 `aep-task/market-volume-dedup` 分支，在独立 Worktree 验证候选提交，打印本地 `evidence.json` 路径。报告包含 Spec SHA、候选 SHA、修改文件、测试命令、退出码与审查状态。失败的工作区会保留供排查。核对报告后可用 `git worktree remove <路径>` 清理。执行器核验通过后保留候选引用 `refs/aep/runs/<run_id>`；本地报告位于 Git 公共目录的 `aep/runs/` 下。

### 新任务

在 `specs/<task>/` 编写 `spec.md` 和 `task.json`，声明准确的可修改文件和验收命令，提交后由维护者派发。例如首次派发行情需求时使用：

```sh
python3 aep.py dispatch --task market-volume-dedup --rev main
```

派发将远端任务分支固定到本次 Spec 提交。相同版本可重复派发，已存在的任务引用不能通过该命令改指其他版本。修订需求需使用新的任务 ID。维护者应限制任务分支的写权限。

### 客户端执行

已在本地配置 Codex CLI 和 Claude Code、且仓库内容获准发送给对应模型供应商时，可选择实际客户端。该模式使用各自的模型账户与额度。

```sh
python3 aep.py run --task market-volume-dedup --implementer codex --reviewer claude
```

Codex 在独立 Worktree 生成候选提交。Claude 接收该版本的 Spec 和 Diff，禁用工具，审查文本保存在同一次运行的 `review.txt`。人工在 GitHub 查看 Diff、CI 和审查意见并决定是否批准。选用现有候选提交时可单独加 `--reviewer claude`。默认走人工审查，不会调用模型。协议测试用本地替身验证适配器调用，不消耗模型额度。

## 试点边界

- 这版用 Git 远端作为最小控制面，按任务名手动触发本地执行。Python 标准库提供参考实现。独立 Go 控制面、Rust Daemon、设备身份、Lease、断线接管和桌面端仍在路线图中。
- 只在受信任的仓库运行。Worktree 隔开文件变更，测试命令和待测代码会以本机用户身份执行。选择外部模型客户端前，须确认其环境与数据权限。
- 同一任务可以有多个 Run。每次都针对固定的版本生成证据，由人显式推送候选提交并建 PR。CI 在 PR 当前提交上独立复验，平台不会代替人批准或合并。
- 本仓库用作流程试点与基线。业务收益、费用和可靠性由后续真实需求测量。[AEP 架构提案与八周路线图](https://app.notion.com/p/41a9d9f6f60581d08560fa33ed8c4810)

## 开发验证

```sh
python3 -m unittest discover -s tests -v
```

[试点 Spec 与六条验收用例](specs/market-volume-dedup/spec.md) · [任务清单与验收命令](specs/market-volume-dedup/task.json)
