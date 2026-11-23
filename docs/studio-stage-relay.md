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

交付完成后，运行以下命令核对状态：

```sh
aep project --work "$work_id" status
```

确认输出中的 `stage_name` 为「已交付」，并在 `deliveries` 中找到对应的归档记录：`accepted_run` 是独立验收回执 ID，`run` 是交付回执 ID。

安装 `jq` 后，可直接提取每条归档记录的两个执行 ID：

```sh
aep project --work "$work_id" status | jq '.deliveries[] | {"独立验收执行 ID": .accepted_run, "交付执行 ID": .run}'
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

## 交给同一事项的下一阶段

1. 在来源阶段打开「查看验收与阶段交接」，选择已通过独立审查和验收的第二轮。展开「记忆与交接」核对决策和审查报告，点「确认交接到测试验证」。普通阶段 Agent 成功完成后，也可以用「确认验收并交接到下一阶段」交付结果。
2. 在目标阶段核对「已接收上一阶段交接」，选择 CLI 和模型，填写本轮测试说明，启动阶段 Agent。接收同仓库的接力提交时，Studio 从已验收的 SHA 创建隔离工作区。目标阶段也可以选择「阶段两轮接力」，两轮的实现端和审查端都会收到交接，任务基线由所选 Spec 决定。
3. 重开 Studio，回到事项的「会话与轮次」。来源执行显示「已确认交接」，接收执行显示「本次执行已读取阶段交接」。比较来源执行 ID、决策版本及验收结果，即可核对两边的记录。回到旧阶段重做时，原回执保留在时间线，新执行沿用重新确认的交接。

在 CLI 中确认同一笔交接时，传入来源阶段号（从 1 开始）和来源执行 ID。对于接力，使用最近一次成功的第二轮执行 ID。确认后可重新打开 Studio 选择模型，也可用项目已有的阶段执行命令继续。

```sh
aep project --work "$work_id" confirm-handoff --stage 4 --run "$source_run_id"
aep project --work "$work_id" timeline
```

开发者可以用离屏 Studio 复验这一段。在前面的 UI debug 命令中，将 `--live-stage-relay plan.json` 换成 `--live-stage-transfer transfer.json`，使用独立的输出目录。配置包含 `repository`、`work`、`stage`（来源阶段号）、`source_run`（已通过的来源执行 ID），以及目标阶段的 `client`（例如 `Codex`）、`model` 和 `brief`。复验会保存确认回执、目标执行证据，以及重开后两端时间线的截图。

## 独立验收与交付归档

在「测试验证」中核对通过的测试执行并确认交接，在「独立验收」选择 Claude 和模型。可在阶段说明里指定需要核对的决策、测试命令和回执。Claude 会收到决策正文和原始测试回执的副本。在「会话与轮次」展开验收执行，可核对它的会话、记忆读取和验收报告。通过报告的末行是 `AEP: PASS`。

确认将这次验收交给「交付归档」后，选择模型并启动交付执行。执行成功后点「确认交付并归档」。重开同一事项，在「会话与轮次」点「查看归档时间线」，或者在 Finder 打开归档目录查看原始回执、报告、失败尝试和交接记录。CLI 的 `project --work "$work_id" timeline` 也会列出归档。

复验最终交付时，在来源为独立验收阶段的 `transfer.json` 中加入 `"finalize_delivery": true`。UI debug 会点击归档确认、重开 Studio，核对归档时间线，并在输出目录保存 `archive.json` 和截图。

## 一条命令从开发跑到归档

在 AEP 源码目录复制计划示例，填好后运行。输出目录用于保存这次执行的回执和截图。

```sh
cp deploy/stage-lifecycle.example.json /tmp/aep-plan.json
# 编辑 /tmp/aep-plan.json 后执行
./deploy/stage-lifecycle.sh /tmp/aep-plan.json ./target/my-work-delivery
```

- 用 `aep project list` 查到事项 ID，填入 `work`。`repository` 是 Studio 已关联的本地仓库，`task` 是已提交的 Spec 名称。新事项应已到达开发实现阶段。软件开发模板填 `"stage": 4`。后续阶段沿用该仓库。
- 在 `first`、`second` 分别选实现端、审查端和各自的模型。在 `test`、`accept`、`deliver` 分别填阶段客户端、模型及本轮说明。Spec 应写清两轮范围和第一轮要留下的问题。
- 默认由操作者在 Studio 完成确认。按终端提示进入同一事项，打开阶段的接力记录，把第一轮问题修订成决策并启动第二轮。然后依次确认开发到测试、测试到独立验收、独立验收到交付的交接，最后点「确认交付并归档」。每次确认后命令自动继续。
- 要做可重复的离屏演练，在计划的 `decision` 填入修订正文，运行同一命令时加 `--rehearse`。它会使用真实客户端，通过 Studio 的编辑和确认按钮完成演练。
- 中断后用相同计划、确认方式和输出目录重跑，可继续原事项。已验证的输出目录只复核证据。要独立跑一次新任务，选处于开发阶段的事项，并指定新的输出目录。

成功后打开 `verified.json` 查看决策版本、五次执行的对应关系、独立验收与交付回执，以及截图索引。`evidence/` 保存可单独复制的完整交付归档，`relay/`、`test/`、`accept/`、`deliver/` 保留各阶段的 Studio 截图和确认记录。

也可以在终端核对验证结果。安装 `jq` 后，将 `output` 设置为传给命令的输出目录（上例为 `./target/my-work-delivery`），再检查该目录中的 `verified.json`：

```sh
output=./target/my-work-delivery
jq -e '.verified == true' "$output/verified.json"
```

输出 `true` 且退出码为 0 表示验证通过。

继续使用上面的 `output`，可列出截图相对于该输出目录的路径，每行一条：

```sh
jq -r '.screenshots[]' "$output/verified.json"
```

## 从开发事项启动全流程

1. 在 Studio 打开开发事项，点「开发全流程」。事项进入「开发实现」后，选择目标仓库的任务 Spec，核对提交 SHA。
2. 分别点第一轮和第二轮的实现、审查角色，再选择客户端和模型。测试验证、独立验收、交付归档也可单独设置。模型支持快捷选择或手填 ID，ZCode 可以从本机配置中选择。
3. 点「启动开发全流程」。Studio 使用与 `deploy/stage-lifecycle.sh` 相同的本地执行器。页面显示当前阶段和执行日志。
4. 等待决策时，点「修订决策并继续第二轮」。在原接力记录中将第一轮问题修订为决策，然后用该决策启动第二轮。回到事项的「开发全流程」，按提示打开验收记录，逐次确认交接，最后确认交付归档。
5. 关闭 Studio 后，后台继续运行或停在原来的确认步骤。重开同一事项的「开发全流程」可查看进度。如果执行器中断，点「继续原流程」可读取同一份计划和输出目录。
6. 如果执行失败，页面会显示失败阶段、原因和执行编号。补齐缺少的证据或修复运行环境后，点「从独立验收重试」等对应阶段的按钮。Studio 复用原 Spec、模型、决策和已确认的交接，为失败执行生成新的回执。失败尝试仍可在归档里查看。
7. 显示「核验通过」后，点击验收报告、回执、交接文件或 Studio 截图即可预览。「在 Finder 中打开归档」可定位可复制的证据目录。

核对一次独立验收重试时，将 `output` 设置为本次开发全流程的输出目录，读取已确认交给独立验收的测试执行编号：

```sh
jq -r '.run' "$output/accept/confirmed.json"
```

这里的 `.run` 是交给独立验收的测试执行编号，用于确认本次验收接收了哪一次测试执行。

独立验收重试成功后，继续使用同一个 `output`，将确认记录与成功后的验收回执交叉比对：

```sh
jq -e -s '
  .[0].run as $test_run
  | ($test_run | type == "string" and length > 0)
    and (.[1].read_stage_transfer.run == $test_run)
' "$output/accept/confirmed.json" "$output/accept/destination.json"
```

这里按顺序读取两份文件，比较 `accept/confirmed.json` 的 `.run` 和 `accept/destination.json` 的 `.read_stage_transfer.run`。输出 `true` 且退出码为 0，表示成功后的独立验收读取了已确认的那次测试执行；编号缺失、为空或不一致时检查不通过，应核对输出目录和本次重试的回执。

### 从归档交付合入本地分支

在事项的「会话与轮次」中，打开归档卡片上的「查看 Diff 并合入本地」。页面从封存的交接链确定已验收提交，校验接力保留引用和后续阶段工作区；选择本地分支后，可按文件展开最终 Diff，或导出完整二进制 patch。核对目标、顶端和统计后，再点「确认本地合并」。

合并只更新本地分支，不会推送。未检出的分支使用带旧 SHA 校验的 `update-ref`；主工作区检出的分支要求工作区干净（包括未跟踪文件），执行 `merge --ff-only`；其他工作树检出的分支不可写入。已经包含候选的分支只记录当时的实际 SHA，不移动分支。AEP 内部分支默认折叠。

CLI 使用相同的校验和回执（`delivery_run` 是交付归档执行 ID）：

```sh
aep project --work "$work_id" merge-targets --delivery "$delivery_run"
aep project --work "$work_id" merge-preview --delivery "$delivery_run" --target main
# 核对预览后，使用输出中的 diff_sha256 确认：
aep project --work "$work_id" merge --delivery "$delivery_run" --target main --diff-sha256 "$diff_sha256"
# 显式导出完整 patch；输出文件必须尚不存在：
aep project --work "$work_id" merge-patch --delivery "$delivery_run" --target main --output /tmp/final-changes.patch
```

`merge-preview` 严格只读，JSON 包含候选、分支状态、顶端、统计、Diff、截断标记和摘要。预览不自动写 patch 文件；`merge-patch` 才会写入指定的新文件。摘要覆盖候选、目标名、顶端和完整 Diff，确认时重新校验。

合并结果写在原交付条目的 `merge` 中，并在归档目录新增 `merge.json`。原有 `manifest.json`、`timeline.json` 和执行证据字节保持不变。`status` 的 `deliveries[].merge`、`timeline` 的 `merge_events[]` 以及 Studio 归档卡片均可回看实际 SHA、确认人和时间。

中断后，`merge.status=pending` 保存原摘要和目标：目标仍在 `before_sha` 时，用原摘要重新确认；目标已经到 `candidate_sha` 时，重新确认会恢复补记；目标变为其他 SHA 时，拒绝自动处理。可以明确放弃意图后重新预览，放弃的时间、原因和原意图保存在 `merge_history`：

```sh
aep project --work "$work_id" merge-abandon --delivery "$delivery_run" --reason "目标分支已由其他操作更新"
```

同一交付只允许一份完成回执。重复确认同一目标返回原回执，换目标则拒绝。事项合并和 `aep local merge` 共用仓库的 `local-delivery.lock`。

自测入口（全部使用临时仓库）：

```sh
cargo test --locked --lib project::
cargo test --locked --test work_item_merge
cargo check --locked --manifest-path desktop/Cargo.toml --all-targets
cargo run --locked --manifest-path desktop/Cargo.toml --example ui_debug -- --scenario work-merge --output-dir target/work-item-local-merge-ui --screenshots
```
