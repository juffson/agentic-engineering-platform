# 实现与自测记录

日期：2026-11-23。核对依据：[PRD](spec.md)、[技术方案](design.md)。

## 实现决定

- 从封存的交付 → 验收 → 测试 → 开发第二轮交接链确定候选，校验归档回执与保留引用。事项重新打开后留下的其他历史交接不参与候选选择；同一链上的重复或缺失执行仍拒绝。
- 后续阶段以执行回执中的 `workspace` 校验仓库、HEAD 和未提交改动；缺少工作区信息时拒绝，不跳过检查。
- `Delivery.merge` 保存意图和完成回执；`merge_history` 保留明确放弃的意图。`merge.json` 原子、不可覆盖写入，并在重开项目库或归档时间线时恢复镜像。原封存文件不改写。
- 操作者使用 Git `user.name` / `user.email`。CLI 和 Studio 共用 `project::merge::merge` 事务，持有现有 `local-delivery.lock`。
- 未检出目标用带旧顶端比较的 `update-ref`；干净主检出用 `merge --ff-only`；其他工作树检出的分支拒绝；已包含候选时只记实际顶端。
- 确认使用 `diff_sha256`，覆盖候选、分支、顶端及完整 Diff。预览只读；完整二进制 patch 通过显式导出生成，避免预览写文件。

## 变更文件

| 文件 | 作用 |
| --- | --- |
| `src/project/merge.rs` | 候选链、分支状态、Diff、摘要、快进、回执、中断恢复及单元测试 |
| `src/project/archive.rs` | 交付可选字段、封存兼容校验及回执覆盖展示 |
| `src/project.rs` | 模块入口及项目库重开时恢复权威回执 |
| `src/project/timeline.rs` | 合并事件与回执恢复 |
| `src/project_cli.rs` | targets / preview / patch / merge / abandon CLI |
| `desktop/src/view/project_page.rs` | 合并页面入口 |
| `desktop/src/view/project_page/merge.rs` | 分支选择、按文件折叠 Diff、完整 patch、二次确认与恢复 |
| `desktop/src/view/project_page/timeline.rs` | 归档入口、意图/回执、合并前后 SHA、确认人与复制 SHA |
| `desktop/src/view/project_page/delivery.rs` | 交付卡片显示合并结果 |
| `desktop/examples/ui_debug.rs` | 注册 `work-merge` 界面自测场景 |
| `desktop/examples/support/work_merge.rs` | 真实事件处理器的预览、展开、确认、回执和重开自测 |
| `tests/work_item_merge.rs` | CLI 只读、摘要拒绝、双进程确认、幂等、封存文件与重开回执 |
| `tests/support/work_item_merge.rs` | CLI 与界面共用临时 Git / 归档夹具 |
| `Cargo.toml`、`desktop/Cargo.lock` | 让无 control feature 的 Studio 也可使用 SHA-256 |
| `docs/studio-stage-relay.md` | 操作、CLI、恢复和自测说明 |
| `specs/work-item-local-merge/{spec.md,design.md,task.json,validation.md}` | 需求、方案、实现范围与验证记录 |

## 需求与测试对应

| 需求 / 边界 / 验收 | 验证证据 |
| --- | --- |
| FR-1～3；E-1～4；AC-1～3、14 | 候选回执/引用校验、断链/重复候选拒绝、后续工作区脏或提交变化、缺工作区拒绝、旧记录兼容等单元测试 |
| FR-4～6；E-5、6、15；AC-1、7 | Diff 统计、归档 Diff 比较、摘要过期拒绝、大文件截断和完整二进制 patch 测试；按文件展开界面仅编译验证 |
| FR-7～9；E-7、8；AC-4 | 本地分支、主检出脏状态、linked/强制重复检出、分叉点和无共同祖先测试；默认目标和内部分支折叠已实现，界面操作待桌面验证 |
| FR-10～14；E-9、11、12、16；AC-5、6、9、10、13 | 主检出 ff、未检出 update-ref、共享锁、双进程只有一次目标 reflog 更新、重复回执一致、已包含分支和不可达 remote 测试 |
| FR-15～18；E-14；AC-8 | CLI 集成测试逐字节比较所有原归档文件，检查 `status` / `timeline` / snapshot；旧二进制丢失镜像后的恢复单元测试；Studio 重开显示待桌面验证 |
| FR-19、20；E-10；AC-11 | Pending 的三个顶端状态、恢复标记、重试及放弃历史单元测试 |
| FR-21、22；AC-12 | CLI 预览前后 catalog 字节和所有 refs 一致；错误摘要非零退出；合并回执与重开记录一致 |
| E-13 | 每条交付按 run 保存独立回执；历史同阶段候选不覆盖封存链候选有回归测试；同事项多归档的完整界面操作待验收 |
| AC-15 | 全量 `cargo test --locked`，包括原 local merge、归档和 snapshot 回归 |

## 验证命令与限制

以下命令均在本事项工作区执行；Git 合并用临时测试仓库，没有合并当前项目分支，没有推送或发布。

```sh
cargo test --locked
cargo test --locked --lib project::merge::tests
cargo check --locked --manifest-path desktop/Cargo.toml --all-targets
cargo fmt --all -- --check
cargo fmt --manifest-path desktop/Cargo.toml --all -- --check
git diff --check
```

原始日志保存在 `target/work-item-local-merge-validation/`：全量 `cargo-test.log`、合并单元测试 `merge-tests.log`、CLI 集成测试 `cli-test.log`、Studio 编译 `studio-check.log`。合并模块 17 个测试及 CLI 集成测试通过；全部 33 个库测试通过。Studio 全目标编译、格式、补丁空白及 20 个变更文件的 allowed_paths 范围检查通过。

最终版本的全量复测连续两次在现有的 `zcode_desktop_send::tests::waits_for_delayed_extension_ack_after_accept` 失败，返回 `No such file or directory`，当轮未继续集成测试。日志为 `cargo-test.log`、`cargo-test-initial-retry-failure.log`；单独重跑也失败，见 `zcode-socket-test.log`。独立最小 Unix socket bind 探针报 `PermissionError(1, 'Operation not permitted')`，见 `socket-probe.log`，与当前沙箱禁止 socket 绑定的表现一致。

显式跳过这一项继续全量检查时，现有 `control_worker_recovers_after_restart_and_delivery_recovers_a_lost_receipt` 启动 `control serve --listen 127.0.0.1:0` 返回 `Operation not permitted`，随后因没有监听地址而失败；见 `cargo-test-except-socket.log`。没有改动这两项测试或相关实现。**最终版本不能报告全量测试通过**；需在允许本地 socket 和监听端口的环境重跑原始全量命令。

为执行被全量失败挡住的后续集成测试，另行运行：

```sh
cargo test --locked --test work_item_merge
cargo test --locked --test pilot --test project_handoff --test studio_task_target \
  --test work_item_merge -- --skip control_worker_recovers_after_restart_and_delivery_recovers_a_lost_receipt
```

后续集成检查通过 32 项（pilot 20、project_handoff 8、studio_task_target 3、work_item_merge 1），日志为 `integration-tests.log`。结合库测试 33 项、其余 bin 测试 15 项与 market_volume 6 项，最终代码分批验证合计通过 86 项；另外 2 项受环境限制。以上显式跳过仅用于定位环境限制，不等于原始全量门禁通过。

已尝试以下真实事件处理器自测，并另试不带 `--screenshots` 的 headless 模式：

```sh
cargo run --locked --manifest-path desktop/Cargo.toml --example ui_debug -- \
  --scenario work-merge --output-dir target/work-item-local-merge-ui --screenshots
```

均在界面初始化时退出 101：`com.apple.hiservices-xpcservice` 连接无效，`+[NSPasteboard generalPasteboard]` 返回 NULL。最终带截图运行的错误日志为 `target/work-item-local-merge-validation/ui-debug.log`。没有生成可用截图，也未执行到点击预览、确认、回执、重开，不能据此宣称 GUI 验收通过。

下一步：在允许本地 socket / 端口监听且可访问 macOS 桌面服务的会话重跑全量测试和上述界面命令，检查预览、确认、回执与重开截图，再完成测试验证与独立验收。

## 测试验证阶段复测（执行 0375fb83d270435fab755f192af6d4f2）

本阶段重新执行原始全量测试、Studio 编译、GUI 自测和格式检查，并增加 4 项 CLI 验收测试。可运行回归共 90 项通过；原始全量仍被两项本地监听相关测试阻断，GUI 仍在 macOS 桌面服务初始化时失败。阶段结论为 **BLOCKED**，不能将显式排除两项后的通过结果认作全量通过。

逐条 AC-1～15 结论、实际命令、退出码、失败原文和下一步见 [testing-stage.md](testing-stage.md)；结构化关键证据及被测文件摘要见 [testing-evidence.json](testing-evidence.json)。本轮原始日志单独保存在 `target/work-item-local-merge-testing-stage/`。

## 测试验证修复（执行 8fce19a873f74821be99f923d9271e86）

修复刷新 Diff 重置目标和 Pending 恢复仍能切换目标、顶端变化仍提供确认入口的问题。Studio 与库共用恢复状态判定，二次确认补齐增删行数。新增 3 项状态测试及 GUI 目标刷新断言，保留原全量和 GUI 门禁。

根项目可运行回归 90 项通过，Studio 串行及默认并发复跑均 25 项通过；首次默认并发运行有一次既有 relay lease 用例失败，原因未确认。全量仍受本地监听限制、GUI 仍因原生桌面服务初始化失败而未执行点击，阶段结论仍为 **BLOCKED**。逐条核对、实际命令及全部失败记录见 [repair-stage.md](repair-stage.md) 和 [repair-evidence.json](repair-evidence.json)。

## 人工确认通过

用户在获知上述限制后明确回复「通过吧」。当前测试验证阶段记为 **PASS（人工确认）**，进入独立验收；自动验证仍为 BLOCKED。监听权限导致的全量失败、GUI 缺失证据及 relay 锁间歇失败继续随交接保留，不改写命令退出码，不代表已完成全量或 GUI 验收。确认信息见 [repair-evidence.json](repair-evidence.json) 的 `manual_acceptance`。未推送、合并或发布。
