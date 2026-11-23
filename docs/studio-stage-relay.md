# Studio 阶段两轮接力

开始前，在阶段的目标仓库提交 `specs/<任务名>/spec.md` 和 `task.json`，写清允许修改的文件和验收命令。Studio 会列出当前提交中的任务，以及本地 `aep-task/<任务名>` 分支上的任务。

## 操作步骤

1. 打开开发事项，选择要推进的工作流阶段，点击「阶段两轮接力」。
2. 选择本地任务 Spec，核对任务与当前阶段是否匹配。
3. 分别打开「第一轮模型」和「第二轮模型」，为每轮选择实现端、审查端及各自使用的模型。
4. 点击「发起第一轮」。第一轮按 Spec 完成本轮修改、验收和审查，并在结构化交接中写明已完成内容与未决问题。
5. 查看第一轮交接，点击未决问题旁的「修订为决策」，将问题改写为明确、可执行的人工决策并保存。
6. 核对保存的决策，点击「用此决策发起第二轮」。第二轮从第一轮的提交继续开发，实现端和审查端都会收到这条决策。
7. 回到事项的「会话与轮次」，查看两轮实现端、审查端的共享记忆读写记录及执行证据，确认第二轮读取了人工修订后的决策版本。

## CLI 查询

在项目仓库打开终端，将变量 `work_id` 设置为开发事项的 ID，即可查询相同的轮次、会话和记忆记录。

```sh
aep project --work "$work_id" timeline
```

## 每轮验收

两轮都运行 Spec 中的验收命令。第一轮可以留下需要人工决策的问题。第二轮可在「会话与轮次」展开「记忆与交接」，核对双方读取的决策编号和审查报告。

关闭 Studio 后，回到同一事项和阶段，点「阶段两轮接力」，在「接力记录」里选中原记录即可继续。

## 用 UI debug 复验

在 AEP 源码仓库运行以下命令。它会操作离屏 Studio 窗口，使用本机已登录的客户端，并在输出目录保存执行回执、事项时间线和截图。

```sh
cargo build --locked
AEP_STUDIO_RUNNER="$PWD/target/debug/aep" cargo run --locked \
  --manifest-path desktop/Cargo.toml --example ui_debug -- \
  --live-stage-relay plan.json --output-dir ./target/stage-relay-proof --screenshots
```

`plan.json` 填写 `repository`（本地仓库目录）、`work`（开发事项 ID）、`stage`（从 1 开始的阶段号）、`task`（已提交的任务名）、`decision`（要保存的修订正文），以及 `first` 和 `second` 两组配置。每组包含 `implementer`、`implementation_model`、`reviewer` 和 `review_model`。为这项复验准备的 Spec 应要求第一轮留一个明确问题，第二轮落实修订。复用同一输出目录会按保存的接力 ID 打开原记录。
