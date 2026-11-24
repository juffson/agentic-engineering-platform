# 技术方案：ZCode 项目默认模型 · 两轮验证

对应 PRD：`spec.md`。本方案**不改产品代码**（AC-9），只新增测试与验证脚本；验证出缺口时记为缺陷，不在此修复。

## 0. 证据位置（已确认）

`aep run` 的请求不单独落盘为 `request.json`，而是写入运行证据 `evidence.json` 的 `request` 字段（`src/run.rs:399-406`，经 `workspace::status` 读取）；时间线与 Studio 卡片也从该字段取值（`src/project/timeline.rs`、`relay_page.rs`）。独立的 `request.json` 只存在于 `handoff` 外部接力目录（`src/handoff.rs`）。因此本方案中凡「`request.json`」一律指 **`<data>/runs/<run_id>/evidence.json` → `request`**；验收断言、保存的证据和截图都以它为准，不要求也不伪造单独的 `request.json`。

## 1. 仓库边界

| 区域 | 角色 | 本次是否改动 |
|---|---|---|
| `src/project.rs` `zcode_default` / `set_zcode_default` | 默认值存取 | 否 |
| `src/project_cli.rs` `ZcodeDefault` | CLI 保存/清除，保存前用 `zcode_catalog::accepts` 校验 | 否 |
| `src/run.rs`（约 225–347、406、436、473） | 把 `--zcode-model` 写入证据 `request.zcode_model`；`reviewer_model` 另记 | 否 |
| `src/project/timeline.rs`（约 170–185） | 时间线取模型：审查端 `reviewer_model` 优先，否则 `zcode_model` | 否 |
| `desktop/src/relay.rs` `advance` / `launch` / `RelayAgents` | 每次启动把 `relay.agents` 存盘并传 `--zcode-model`；重试复用已存值 | 否 |
| `desktop/src/view/relay_page.rs` `reset_relay_agents` / `sync_relay_form` | 新建与第二轮预填；已有第二轮记录时用已存值 | 否 |
| `desktop/src/view/project_page/zcode.rs` `relay_project_default` | 读**最新**项目库默认，非本项目/未绑定返回空 | 否 |
| 新增 `tests/zcode_default_two_rounds.rs` | 根 crate 集成测试（CLI、项目、请求、时间线） | 新增 |
| 新增 `desktop/examples/support/zcode_two_rounds.rs`，在 `ui_debug.rs` 注册 `zcode-two-rounds` | Studio 两轮场景（测试替身） | 新增/小改 |

## 2. 现状与验收映射

- **默认存取**：默认值是项目上的一个字符串，空串表示「客户端默认」。CLI 与 Studio 保存都走 `set_zcode_default`。→ AC-1、AC-2（E1、E5）。
- **按轮配置**：`relay.agents` 是接力当前配置；`advance` 在每次启动前整体替换它（阶段计划接力同步写 `plan.first/second`）。所以覆盖只落在接力记录，不回写项目（FR-3、E4）。
- **第二轮预填**：`sync_relay_form` 仅在「第一轮已通过且无第二轮记录」时用 `relay_project_default()` 覆盖 `zcode_model`；一旦有第二轮记录，表单读 `relay.agents`，因此重试沿用上次值（FR-2、FR-4、AC-5）。
- **默认读取时机**：`relay_project_default` 每次从磁盘重读项目库，所以第一轮期间改默认，第二轮预填新值（FR-6、AC-7）。
- **证据**：`run.rs` 写 `request.zcode_model`（空则不写）；时间线与 `relay_page` 卡片从 `request` 读，旧记录无该字段即不显示（FR-5、AC-6、AC-8、E6）。
- **ZCode 同为实现端与审查端**：命令行仅传一个 `--zcode-model`；独立 `reviewer_model` 存在时审查端不使用它（E7）。

## 3. 接口与数据

无新增接口、无数据格式变更。被验证的契约：
- 运行证据 `evidence.json.request`：`zcode_model`（可缺省）、`reviewer_model`（可缺省）。
- `relay.json`：`agents.zcode_model` 字符串，缺省为空。
- CLI：`aep project zcode-default [--model V | --clear]` 输出 `{"project","model"}`。

## 4. 兼容性

- 旧 `relay.json` 无 `zcode_model`：`RelayAgents` 为 `#[serde(default)]`，已有测试 `old_and_mixed_relay_profiles_round_trip` 覆盖；本方案在 AC-8 中追加「旧证据（`request` 无该字段）的时间线」用例。
- 测试只在临时目录与临时项目库运行，不读写用户真实项目库；`PATH` 里只放测试替身 `zcode`。

## 5. 测试计划

测试替身：沿用 `tests/project_handoff.rs` 的脚本式 `zcode` 与 `desktop/examples/support/handoff/studio-zcode.sh` 夹具；模型目录用替身输出固定的两个 provider（含 `team/qa` 与 `team/other`、档位 low/high），不调用真实 ZCode，不耗额度。

| 层 | 用例 | 覆盖 |
|---|---|---|
| 根集成测试 | 保存、读取、`--clear`；保存目录里没有的值被拒绝；两个项目互不影响 | AC-1、AC-2(后半)、E1、E2(保存侧)、E3、E5 |
| 根集成测试 | 用 `aep run` 夹具跑两次：第一次 `--zcode-model B`，第二次不传；断言两份运行证据 `evidence.json` 的 `request`（B；无字段），项目默认仍为 A | AC-3、AC-4(请求侧) |
| 根集成测试 | 时间线构造：实现端 ZCode、审查端 ZCode 带/不带 `reviewer_model`；旧请求无字段 | AC-6、AC-8、E6、E7 |
| Studio 场景 `zcode-two-rounds` | 默认 A→第一轮改 B 通过→第二轮预填 A；选「使用客户端默认」→请求无 `zcode_model`；第二轮失败后把默认改 C 再重试→请求仍为失败那次的值；第一轮仍显示原值、新建后预填 C | AC-3、AC-4、AC-5、AC-7、E4 |
| Studio 场景 `zcode-default-projects` | 项目 P1/P2/P3 切换预填（见 5a） | AC-2、E1、E5 |
| Studio 场景 `zcode-timeline-reopen` | 两轮完成后重开，时间线按 `reviewer_model` 优先显示（见 5a） | AC-6、FR-6 |
| Studio 场景 `zcode-legacy-open` | 旧记录打开、无模型字段（见 5a） | AC-8、E6 |
| 现有 | `cargo test --locked`、`cargo check --locked --manifest-path desktop/Cargo.toml --all-targets` | 回归 |

顺序与 PRD 一致：先 AC-3，再 AC-5，最后 AC-7；每步保存每轮 `evidence.json` 的 `request` 对象和截图到 `target/zcode-default-two-rounds-ui/`。

## 5a. 补充 GUI 验证场景（AC-2、AC-6、AC-8）

三个场景作为 `ui_debug` 的独立场景实现，共用 `desktop/examples/support/zcode_two_rounds.rs` 的夹具（临时项目库 + 测试替身 `zcode`/runner），不读用户真实库。重开 Studio 一律用 `ui.frame(|w,_| w.remove_window())` 后 `Driver::open_with_runner(...)`（做法同 `support/local.rs`），不是刷新页面。

**G-AC2 项目切换（场景 `zcode-default-projects`）**
1. 建项目 P1（默认 `team/qa@high`）、P2（默认 `team/other@low`）、P3（无默认），各绑定一个临时仓库。
2. 在 P1 打开「两轮接力」→ 选事项，断言「ZCode 模型」栏 = `team/qa@high`。
3. 切到 P2（项目选择入口，沿用 `support/project.rs` 的项目切换控件），重新打开新建接力，断言 = `team/other@low`；切到 P3，断言为空。
4. 切回 P1，断言仍是 `team/qa@high`，且 P2、P3 的默认在磁盘上未变（`Library::open` 重读）。
5. 检查点截图：`ac2-p1`、`ac2-p2`、`ac2-p3-empty`。覆盖 AC-2、E1、E5。

**G-AC6 两轮完成后重开 Studio 的时间线（场景 `zcode-timeline-reopen`）**
1. 跑完两轮（沿用 `zcode-two-rounds` 的两轮夹具），第一轮：实现端 Codex、审查端 ZCode，**不带** `reviewer_model`，`zcode_model = B`；第二轮：审查端 ZCode，**带** `reviewer_model = team/review@high`，`zcode_model = A`（阶段计划接力，因为只有它能写独立审查模型）。
2. 记下两轮 `evidence.json.request`，再关闭窗口并重开 Studio。
3. 进入事项「时间线」页（`work-timeline-tab`，做法同 `support/zcode_history.rs::timeline`），逐轮展开 `timeline-detail-N`。
4. 断言：第一轮审查模型显示 B（无 `reviewer_model` 时回落 `zcode_model`）；第二轮审查模型显示 `team/review@high`，不是 A（`reviewer_model` 优先）；两轮显示值都等于各自 `evidence.json.request` 推出的值（`src/project/timeline.rs` 的取值规则）。
5. 再在项目设置里把默认改为 C，再次重开，断言时间线两轮显示值**不变**（FR-6）。
6. 检查点：`ac6-timeline-first`、`ac6-timeline-second`、`ac6-timeline-after-default-change`。

**G-AC8 旧记录打开（场景 `zcode-legacy-open`）**
1. 在临时数据目录里直接写旧格式：`relay.json` 无 `zcode_model` 与 `reviewer_model`（只含 `id/task/model/attempts`，同 `old_and_mixed_relay_profiles_round_trip` 的旧样本），并写对应旧 `evidence.json`（`request` 里无 `zcode_model`、`reviewer_model`）。
2. 启动 Studio，进入「两轮接力」→ 选该旧接力：断言页面能打开、无报错提示（不存在错误元素）、没有「ZCode 模型」「审查模型」字段行，模型栏为空。
3. 进入时间线页：该轮正常列出，不显示模型。
4. 项目有默认 A 时，该旧接力若已有第二轮记录则不被改写，表单仍为空；无第二轮记录则预填 A（符合 FR-2）。
5. 检查点：`ac8-legacy-relay`、`ac8-legacy-timeline`。

三个场景都是只读断言加上述最小写入；缺口只记入 `validation.md`，不改产品代码。需要 macOS 桌面服务；不可用时只做编译检查并注明「点选未覆盖」。

## 6. 风险与待确认

- GUI 场景需要 macOS 桌面服务；不可用时 T3 只执行编译检查，并在结论注明「点选未覆盖」。
- Q1/Q2 仍开放：E2 的「已保存默认失效」暂按警告验证；若人工确认为阻止启动，需回到产品需求修订，不在本阶段自行假设。
- 若发现 AC 与现有实现不符（例如重试回到项目默认），记为缺陷写入 `validation.md`，不改产品代码。
