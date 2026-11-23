# PRD：从事项归档接到本地合并

## 背景

事项现在能走完「开发实现 → 测试验证 → 独立验收 → 交付归档」。归档记录（`Work.deliveries` 和 `deliveries/<run>/`）保存独立验收回执、交付回执和每轮接力的 `changes.patch`。

AEP 另有一套工单式的本地批准与合并（`src/local.rs` 里的 `approve-delivery` / `merge`）：
- 只做 `--ff-only` 快进合并；
- 要求目标分支检出、工作区干净、分支没有移动；
- 改分支前先写合并意图（`merging`），并持有 `local-delivery.lock`。

两边还没有接上，具体缺三样：
- 操作者在事项里看不到最终要合入的 Diff；
- 已归档的交付无法合入本地分支；
- 归档里没有实际合并的 SHA。

## 目标

在一条已归档交付上，操作者依次完成：查看最终 Diff → 选择本地目标分支 → 确认合并 → 在同一份交付记录里看到实际合并 SHA。全程只动本地仓库，不推送，也不访问远端。

## 用户场景

- **S1 查看最终 Diff**：在「会话与轮次」的归档记录上打开「本地合并」，看到将合入的提交、文件统计和完整 Diff。
- **S2 选择目标分支**：从本地分支中选择目标，每个分支标明能否快进合并以及原因。
- **S3 确认合并**：核对 Diff 摘要、目标分支和分支顶端后确认，得到快进合并结果。
- **S4 回看记录**：重开 Studio，或用 CLI `status` / `timeline` 查看，同一条交付会显示分支、合并前后的 SHA、时间和确认人。
- **S5 中断恢复**：合并中途进程退出，重开后状态准确，可以安全地补记或重试。

## 术语

- **candidate（已验收提交）**：开发阶段第二轮接力通过审查和验收后，在确认交接时写入 `Transfer.candidate` 的提交。后续阶段的工作区都从这个提交创建（`workflow_stage.rs` 中的 `worktree add … <candidate>`）。
- **最终 Diff**：`git diff <目标分支顶端> <candidate>`。快进合并的前提是目标顶端是 candidate 的祖先，所以这就是合并后目标分支上的全部变化。

## 范围

**包括**
- 确定并校验 candidate。
- 最终 Diff 视图、目标分支选择、确认合并和结果展示。
- 与 Studio 等价的 CLI。
- 把合并回执写回同一份交付记录。
- 合并前校验、互斥和中断恢复。

**不包括**
- push、fetch、PR 等任何远端操作。
- 合并提交、rebase、cherry-pick、squash 和冲突解决。只支持快进，不能快进时只说明原因。
- 回滚已完成的合并。
- 没有本地仓库或没有 candidate 的交付：只显示不可合并的原因。
- 改动现有 `aep local merge` 的行为。

## 功能需求

### 确定要合入的提交
- **FR-1** candidate 取自开发阶段确认交接的 `Transfer.candidate`，并且必须等于归档中对应接力回执的 `candidate_sha`。
- **FR-2** candidate 必须在仓库中存在，并且等于 `refs/aep/runs/<接力执行 ID>`。不一致时不可合并，提示「已验收提交引用已改变」。
- **FR-3** 后续阶段（测试、验收、交付）的工作区如果在 candidate 之上有新提交或未提交修改，就判定为「验收后有未纳入的改动」，不可合并，并列出工作区路径和文件。合并内容只能是经过验收的 candidate。

### 最终 Diff
- **FR-4** 显示以下内容：
  - candidate SHA、来源接力执行 ID 和目标分支顶端 SHA；
  - `--stat` 摘要和完整 Diff，Diff 按文件折叠，超过上限时截断并提供打开完整 patch 的入口；
  - 二进制文件只显示「二进制变更」。
- **FR-5** 预览时生成 `diff_sha256`，覆盖 candidate、目标分支名、目标顶端和 Diff 字节。确认时必须带上这个值；重新计算后不一致就拒绝，并提示「目标或变更已改变，请重新查看 Diff」。
- **FR-6** 显示归档的已验收 Diff（`spec_sha..candidate`）与最终 Diff 是否相同。两者不同时，标注「目标分支已包含部分变更或已前进」。

### 目标分支
- **FR-7** 只列本地分支 `refs/heads/*`。默认选中开发计划记录的目标分支；没有记录时，选中主工作区当前检出的分支。
- **FR-8** 每个分支标注一种状态：
  - 可快进：顶端是 candidate 的严格祖先。
  - 已包含：顶端等于 candidate 或是 candidate 的后代。
  - 无法快进：已分叉，同时显示分叉点。
  - 不可写入：在主工作区检出但不干净（含未跟踪文件），或在其他工作树中检出。
- **FR-9** `aep-stage/*`、`aep-task/*` 默认折叠，并标注「AEP 内部分支」。

### 确认与执行
- **FR-10** 确认前展示：目标分支、合并前顶端、candidate、文件数和增删行，并写明「只更新本地分支，不会推送」。
- **FR-11** 只做快进，分两种情况：
  - 目标分支在主工作区检出且干净：执行 `git merge --ff-only <candidate>`。
  - 目标分支未被任何工作树检出：执行 `git update-ref refs/heads/<b> <candidate> <旧顶端>`。

  两种情况都不得改动其他工作树。
- **FR-12** 改分支前，先把合并意图写入交付记录：交付、分支、旧顶端、candidate、`diff_sha256` 和开始时间。合并后校验分支顶端等于 candidate，再写合并回执。
- **FR-13** 与工单合并共用 `local-delivery.lock`。同一交付重复确认时，直接返回原回执，不再改分支。
- **FR-14** 目标分支「已包含」candidate 时，确认后不改分支，回执记为 `mode=already_contained`，`merged_sha` 取当时的分支顶端。

### 写回交付记录
- **FR-15** 回执写在原交付条目上：
  - 作为新的可选字段，旧数据没有这个字段时仍能读取；
  - 同时在归档目录新增 `merge.json`；
  - 已封存的 `manifest.json`、`timeline.json` 和各执行文件保持字节不变，`Delivery::snapshot` 校验继续通过。
- **FR-16** 回执字段：`repository`、`target_branch`、`before_sha`、`candidate_sha`、`merged_sha`、`mode`、`diff_sha256`、`files_changed`、`insertions`、`deletions`、`intent_at`、`merged_at`、`operator`。
- **FR-17** 每条交付最多一个已完成的回执。合并后界面显示「已合并」，不再提供合并入口。
- **FR-18** 展示位置：
  - Studio 的事项时间线、归档时间线和交付卡片显示「已合并到 `<分支>` · `<短 SHA>`」，可以复制完整 SHA；
  - `status` 的 `deliveries[]` 输出 `merge` 对象；
  - `timeline` 列出合并事件。

### 中断恢复
- **FR-19** 交付有合并意图、没有回执时，按目标分支当前顶端处理：
  - 等于 candidate：补写回执，并标注「恢复补记」。
  - 等于旧顶端：显示「合并未完成」，可以重新确认，确认时仍要校验 Diff 摘要。
  - 其他情况：显示「目标分支在合并过程中被修改」，不自动处理，由操作者放弃这次意图后重新预览。
- **FR-20** 放弃意图时保留一条记录（时间和原因），不删除历史。

### CLI
- **FR-21** `aep project --work <id> merge-preview --delivery <run> --target <branch>`：输出 JSON，包含 candidate、目标顶端、状态、统计、`diff_sha256` 和 patch 路径。只读，不改动任何东西。
- **FR-22** `aep project --work <id> merge --delivery <run> --target <branch> --diff-sha256 <v>`：执行与 Studio 相同的校验和合并。失败时退出码非 0，并给出与 Studio 相同的中文原因。

## 边界情况

| 编号 | 情况 | 期望 |
|---|---|---|
| E-1 | 没有本地仓库，或交接链上没有 candidate | 不显示或禁用合并入口，并说明原因 |
| E-2 | 仓库已移动或删除 | 提示仓库不可用 |
| E-3 | candidate 对象已被清理，或 run ref 已改变 | 按 FR-2 阻止 |
| E-4 | 后续阶段工作区有修改或新提交 | 按 FR-3 阻止，列出文件 |
| E-5 | 预览后目标分支前进（仍可快进） | 摘要变化，按 FR-5 拒绝 |
| E-6 | 预览后目标分支分叉 | 拒绝，状态变为「无法快进」 |
| E-7 | 主工作区有未跟踪文件 | 不可写入 |
| E-8 | 目标分支在其他工作树（含 AEP 阶段工作树）检出 | 不可写入，显示工作树路径 |
| E-9 | 两个 Studio 窗口，或 Studio 与 CLI 同时合并 | 只有一个成功 |
| E-10 | 分支已快进但回执未写入时进程退出 | 按 FR-19 补记 |
| E-11 | 对已合并的交付再次确认 | 返回原回执，不改分支 |
| E-12 | 目标分支已包含 candidate | 按 FR-14 处理 |
| E-13 | 同一事项有多条归档 | 每条单独合并、单独记录 |
| E-14 | 旧版本生成的交付记录 | 显示「未合并」，可以合并 |
| E-15 | Diff 很大或含二进制 | 截断显示并提供完整文件入口，合并不受影响 |
| E-16 | 仓库配置了远端或 hook | 不推送、不拉取；hook 按 git 默认行为运行 |

## 验收标准

- **AC-1**（S1，FR-1、FR-4）界面显示的 candidate 同时等于 `Transfer.candidate` 和接力回执的 `candidate_sha`，Diff 统计与 `git diff --stat <顶端> <candidate>` 一致。
- **AC-2**（FR-2）把 `refs/aep/runs/<run>` 改到其他提交后，预览和合并都失败，分支保持不变。
- **AC-3**（FR-3）在测试阶段工作区新增未提交文件或新提交后，预览列出该文件，确认按钮不可用，CLI 退出码非 0。
- **AC-4**（FR-7、FR-8）准备可快进、已包含、已分叉、在其他工作树检出这 4 个分支，状态显示依次正确。目标分支有未跟踪文件时显示「不可写入」，列表中没有远端分支。
- **AC-5**（S3，FR-11、FR-12）在检出且干净的分支上合并后，分支顶端等于 candidate，主工作区文件等于 candidate，其他工作树没有改动，也没有新的合并提交。
- **AC-6**（FR-11）未检出的分支合并后指向 candidate，主工作区当前的分支和文件不变。
- **AC-7**（FR-5）预览后在目标分支上追加一个提交，再用原 `diff_sha256` 确认，被拒绝，分支只多出追加的那个提交。
- **AC-8**（S4，FR-15～18）合并后：
  - `status.deliveries[].merge` 字段齐全，且 `merged_sha == candidate_sha`；
  - 归档目录新增 `merge.json`，原有归档文件的 sha256 不变，`snapshot` 校验通过；
  - 重开 Studio 后显示「已合并到 …」。
- **AC-9**（FR-13）两个进程同时合并，只有一个改动分支。对已合并的交付再次执行，返回原回执，分支和回执都不变。
- **AC-10**（FR-14）对已包含 candidate 的分支确认后，分支不变，回执 `mode=already_contained`。
- **AC-11**（S5，FR-19）FR-19 的三种中断状态各自得到「恢复补记」「合并未完成，可重试」「目标分支在合并过程中被修改」。
- **AC-12**（FR-21、22）`merge-preview` 前后的 `git for-each-ref` 输出和 catalog 字节完全一致。`merge` 的输出与 Studio 写入的记录相同。
- **AC-13**（范围）remote 指向不可达 URL 时合并仍然成功，过程中没有任何网络或 push、fetch 调用。
- **AC-14**（E-1、E-14）没有 candidate 的交付有不可合并的说明；旧版记录显示「未合并」，可以正常合并。
- **AC-15**（回归）现有 `aep local merge`、归档封存和 `snapshot` 的测试全部通过，`cargo test --locked` 通过。

## 留给开发阶段的设计问题
1. 回执放在 `Delivery` 的可选字段里，还是由 `Work` 按交付 run 单独关联？需要满足 FR-15 和 `snapshot` 校验。
2. FR-3 检查工作区时，以 `Transfer.checkout` 为准，还是以各阶段回执里的 `workspace` 为准？
3. `operator` 的来源与现有 `Stamp` 保持一致。

## 建议的 task.json
```json
{"title":"从事项归档接到本地合并",
 "allowed_paths":["src/project.rs","src/project/archive.rs","src/project/merge.rs","src/project/timeline.rs","src/project/transfer.rs","src/project_cli.rs","src/local.rs","src/git.rs","desktop/src/view/project_page.rs","desktop/src/view/project_page/delivery.rs","desktop/src/view/project_page/timeline.rs","desktop/src/view/project_page/merge.rs","desktop/examples/ui_debug.rs","desktop/examples/support/work_delivery.rs","desktop/examples/support/work_merge.rs","docs/studio-stage-relay.md"],
 "check":["cargo","test","--locked"]}
```

