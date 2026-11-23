# 技术方案：从事项归档接到本地合并

## 0. 现状

- **归档**：`src/project/archive.rs::seal` 把一次交付写到 `<home>/workflows/<project>/<work>/deliveries/<run>/`，里面有 `manifest.json`、`timeline.json` 和各次执行的证据。回执 `Delivery` 记在 `Work.deliveries`。`Delivery::snapshot` 会检查 `manifest["delivery"] == self`。
- **候选提交**：开发阶段第二轮接力确认交接时，`Transfer::read_relay` 把 `candidate`（已验收的 SHA）和 `repository` 写进 `stage_transfers`。测试、独立验收和交付三个阶段都通过 `read_stage_transfer` 一环扣一环地读取上一阶段的确认，这条链在归档的 `timeline.json` 里有完整副本。
- **已有合并能力**：`src/local.rs::merge` 只适用于本地 ticket，归档后的事项用不上。可以复用的做法有：`local-delivery.lock`、先记录合并意图再操作、`merge --ff-only`、合并后核对 tip，以及崩溃后检测"tip 已等于候选"来补完。

## 1. 三个设计问题的决定

1. **合并哪个提交**：只从归档快照里取。从 `delivery.accepted_run` 开始，沿 `timeline.json` 中 rounds 的 `read_stage_transfer` 往回查（验收 → 测试 → 开发交接），必须正好查到一个带 `candidate` 的开发交接。仓库取那条交接的 `repository`。中间缺一环或者候选不唯一就拒绝合并。Studio 和 CLI 都不接受手填 SHA。
2. **回执写在哪**：`Delivery` 增加一个可选字段 `merge: Option<MergeReceipt>`（`serde(default, skip_serializing_if = "Option::is_none")`），继续用 `Work.deliveries` 里原来那一条记录。`snapshot` 比较时去掉 `merge` 字段再和 `manifest` 比对，所以旧归档能照常校验。归档目录里只新增一个 `merge.json`，作为权威回执，用 create-new 原子写入。已有的 `manifest.json`、`timeline.json` 和证据文件一个字节都不改。
3. **怎样改目标分支**：只做快进，要求 `merge-base --is-ancestor <tip> <candidate>`。用 `git worktree list --porcelain` 查目标分支的检出情况，分三种：
   - **没有被检出**：执行 `git update-ref refs/heads/T <candidate> <tip>`，对旧 tip 做比较交换，不动任何工作区。
   - **在主检出里被检出**：沿用 local.rs 的规则，工作区必须干净（包括未跟踪文件），然后执行 `git merge --ff-only`。
   - **在其他 linked worktree 里被检出**：直接拒绝，提示先切走。

## 2. 接口

**库函数**（新文件 `src/project/merge.rs`，Studio 在进程内调用，CLI 也调同一套）：

```rust
pub struct Candidate { repository: PathBuf, sha: String, spec_sha: Option<String>, transfer_run: String }
pub struct Branch { name: String, tip: String, checked_out: Option<PathBuf>, clean: bool, fast_forward: bool }
pub struct Preview { candidate: Candidate, target: String, tip: String, diff: String, truncated: bool, files: Vec<String> }
#[derive(Serialize, Deserialize)]
pub struct MergeReceipt {
    pub status: MergeStatus,          // Pending | Merged
    pub repository: PathBuf, pub target: String,
    pub from: String,                 // 合并前 tip
    pub candidate: String,
    pub merged_sha: Option<String>,   // 合并后实际读到的 tip
    pub method: String,               // "update-ref" | "ff-merge"
    pub actor: String,                // 取 git user.name 和 user.email
    pub requested_at: String, pub merged_at: Option<String>,
}
pub fn candidate(home, project, &Delivery) -> Result<Candidate>;
pub fn branches(&Candidate) -> Result<Vec<Branch>>;
pub fn preview(home, project, &Delivery, target) -> Result<Preview>;   // 用 target...candidate 生成 diff，最多 96 KiB
pub fn begin(home, project, &Delivery, target, expect_tip) -> Result<MergeReceipt>; // 在 Library::change 里写入 Pending
pub fn apply(home, project, &Delivery) -> Result<MergeReceipt>;        // 加锁，改 ref，核对，写 merge.json
```

**流程**：先调 `begin`，把 Pending 状态和 `from`、`target` 写进 projects.json。再调 `apply`：

1. 取得 `<repo>/.git/aep/local-delivery.lock`，和本地 ticket 合并共用这把锁，避免同一仓库同时合并。
2. 用 `update-ref refs/aep/deliveries/<run>` 钉住候选，防止被 gc。
3. 改 ref，再核对 tip 是否等于候选。
4. 写 `merge.json`，然后在 `Library::change` 里把状态改成 Merged。

**恢复**：遇到 Pending 状态时，tip 已等于候选就直接补完；tip 还等于 `from` 就允许重试；tip 是其他值就报"目标分支已变化"，允许操作者撤回这次 Pending。已经 Merged 的交付再次合并到同一个 target 时，原样返回原回执；换一个 target 则拒绝。

**CLI**（`src/project_cli.rs`）：

```sh
aep project --work "$w" merge-targets --delivery "$run"
aep project --work "$w" merge-preview --delivery "$run" --target main
aep project --work "$w" merge --delivery "$run" --target main --expect-tip "$tip"
```

`status` 输出的 `deliveries[].merge` 会自动带上这些字段。

**Studio**：在 `timeline.rs` 的「交付已归档」面板里加一个「查看 Diff 并合入本地」按钮，打开新的 `project_page/merge.rs`：

- 显示候选和仓库。
- 列出本地分支：标注检出位置、工作区是否干净、能否快进；不能快进的分支禁止选择。
- 显示 Diff 预览，以及是否被截断。
- 「确认合并」按钮把预览时看到的 `tip` 作为 `expect_tip` 传入。
- 合并完成后，面板显示目标分支、合并 SHA、操作者和时间。

## 3. 数据与兼容性

- 旧版 projects.json 和归档里没有 `merge` 字段，读出来是 None，`snapshot` 照常校验。
- 旧版二进制读取新数据时会忽略 `merge` 字段，重新保存时会丢掉这份镜像。`merge.json` 仍保留在归档里，`apply` 和 `status` 会以它为准恢复回执。
- 不改变 local ticket 的数据和流程，不推送，不访问远端。

## 4. 变更文件

`src/project/merge.rs`（新）、`src/project/archive.rs`、`src/project.rs`、`src/project_cli.rs`、`desktop/src/view/project_page.rs`、`desktop/src/view/project_page/timeline.rs`、`desktop/src/view/project_page/merge.rs`（新）、`docs/studio-stage-relay.md`。

## 5. 测试计划（`src/project/merge.rs` 单元测试，用临时 git 仓库）

1. 从归档链解析出候选。链断开、没有候选或候选多于一个时都拒绝。
2. 目标分支未被检出：用 update-ref 合并后，`merge.json` 正确；manifest 和 timeline 的字节哈希不变；`snapshot()` 仍能通过。
3. 目标分支在主检出里被检出：工作区脏（包括未跟踪文件）时拒绝；干净时完成 ff，工作区跟着更新。
4. 目标分支已分叉，无法快进：拒绝，并且什么都不写。
5. `expect_tip` 已过时：拒绝。
6. 重复合并到同一 target 时幂等，换 target 时拒绝。
7. 恢复 Pending 的三种情况：tip 等于候选、tip 等于 `from`、tip 是其他值。
8. 目标分支在其他 linked worktree 里被检出：拒绝。
9. 不带 `merge` 字段的旧 Delivery JSON 可以正常反序列化和校验。

**task.json 草案**：

```json
{
  "title": "从事项归档快进合并到本地目标分支",
  "allowed_paths": ["src/project/merge.rs","src/project/archive.rs","src/project.rs","src/project_cli.rs",
    "desktop/src/view/project_page.rs","desktop/src/view/project_page/timeline.rs",
    "desktop/src/view/project_page/merge.rs","docs/studio-stage-relay.md"],
  "check": ["cargo","test","--locked","--lib","project::"],
  "checks": [
    {"id":"lib","criteria":["归档合并单元测试全部通过"],"command":["cargo","test","--locked","--lib","project::"]},
    {"id":"studio","criteria":["Studio 与示例可编译"],"command":["cargo","check","--locked","--manifest-path","desktop/Cargo.toml","--all-targets"]}
  ]
}
```

这里的检查命令我没有实际跑过，提交 Spec 时先跑一遍确认能通过。

## 实现时核对 PRD 的补充决定

本阶段已读取原 PRD（来源执行 2c85c8e9521e4b6cbabbf921b744d00b），以已确认的 PRD 为准补齐上述简化方案：

- candidate 必须与封存第二轮回执和 refs/aep/runs/<run> 一致；沿归档交付→验收→测试→开发链取唯一来源。
- FR-3 使用归档中后续执行回执的 workspace 路径，逐个校验仓库、HEAD 和包含未跟踪文件的工作区状态。Transfer.checkout 只是目录标识，回执路径是实际执行位置。
- operator 沿用 Git user.name / user.email。
- 确认必须传 diff_sha256；摘要覆盖候选、分支、顶端、完整 Diff 字节。expect-tip 已被摘要覆盖，不作为独立 CLI 参数。
- 已包含候选的分支允许确认，mode=already_contained，回执保存实际顶端；仍拒绝脏主检出或其他 linked worktree。
- Pending 写在原 Delivery.merge，完成回执 create-new 原子写入 merge.json 后同步原条目；读取项目库与归档时间线时以 merge.json 恢复镜像。放弃意图追加 merge_history，封存文件不改。
- merge-preview 严格只读，直接输出 Diff；完整二进制 patch 通过显式 merge-patch --output 导出，避免预览为了生成文件而产生写入。Studio 用临时目录导出完整 patch。

对应 FR/E/AC 的验证与实现文件见 validation.md。
