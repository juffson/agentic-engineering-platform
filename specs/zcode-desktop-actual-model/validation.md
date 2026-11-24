# 验证：ZCode 桌面双方案 · 实际模型与交接

## 自动检查（task.json `checks`）

| id | 命令 | 覆盖 |
|---|---|---|
| receipt | `cargo test --locked --lib zcode_receipt` | T1–T3 · AC-4/7/8/9 |
| timeline | `cargo test --locked --lib project::timeline` | T10 · AC-6/9 |
| bridge | `cargo test --locked --bin aep zcode` | T4–T7、T14 · AC-1/2/3/8/10、FR-10 |
| cli | `cargo test --locked --test project_handoff` | T8–T9 · AC-2/7 |
| all | `cargo test --locked` | AC-11 |
| studio | `cargo check --locked --manifest-path desktop/Cargo.toml --all-targets` | AC-11 |
| ui-workflows | `ui_debug --scenario workflows --screenshots` | AC-3/4/5/9（需 macOS 桌面服务） |
| ui-relay | `ui_debug --scenario studio-relay --screenshots` | AC-6（需 macOS 桌面服务） |

## 人工验收（AC-12）

前提：本机 ZCode 已配置 Start Plan 与个人 Coding Plan。

1. 新建事项，某阶段选 Start Plan 的模型执行；完成后核对轮次标题 `turn-model-*` 与 ZCode 桌面任务模型一致，点击「打开对应 ZCode 任务」能跳到原任务。
2. 同一阶段改选个人 Coding Plan 重跑，两轮各显示各的方案。
3. 阶段模型留空执行一次，确认显示「… · 客户端默认」，且档位与 ZCode 设置一致（app-server 路径，Q2）。
4. `aep project show` 中对应 attempt 的 `actual_model` 与界面一致；项目记忆作者为「ZCode / 方案名 / modelId@档位」。
5. `ui_debug --live-zcode-history` 中会话卡显示「实际模型」字段（FR-10）。
6. 保存第 1、2 步截图与 ZCode 桌面任务截图。
