# 验证记录（待实现阶段填写）

| AC | 命令/场景 | 结果 | 证据 |
|---|---|---|---|
| AC-1 | `cargo test --locked --test zcode_default_two_rounds default_` | 待跑 | |
| AC-2 | 同上 `projects_` + GUI 场景 `zcode-default-projects` | 待跑 | |
| AC-3 | `zcode_default_two_rounds round_override_` + 场景 | 待跑 | |
| AC-4 | 同上 `native_second_round_` + 场景 | 待跑 | |
| AC-5 | 场景 `zcode-two-rounds`（失败后改默认再重试） | 待跑 | |
| AC-6 | `timeline_` + GUI 场景 `zcode-timeline-reopen`（重开后审查模型优先） | 待跑 | |
| AC-7 | 场景 | 待跑 | |
| AC-8 | `legacy_` + GUI 场景 `zcode-legacy-open` | 待跑 | |
| AC-9 | `git diff --stat -- src desktop/src` 为空 | 待跑 | |

缺陷只记录在此，不在验证阶段修复。未能执行的 GUI 点选须在此注明。

## 合入说明（2026-11-24）

该事项交付早于阶段自动提交，产出未进入事项分支。合入 main 时只保留核心测试
`tests/zcode_default_two_rounds.rs`（8 项通过）与本规格；GPUI Studio（desktop/）已移除，
其中的 GUI 场景演练（`desktop/examples/support/zcode_two_rounds.rs`、`ui_debug.rs` 改动）不再适用，未合入。
