# 验证 · 默认沙箱下 Claude 阶段能写文件 — 技术方案

输入：`specs/default-sandbox-write-check/spec.md`（R1–R6、E1–E10、AC-1–AC-8）。
本阶段 run：`ccab9dcf350440d08292b57d0658e18e`（Standard，Claude，技术方案）。

## 1. 已核实的事实

| # | 事实 | 证据 |
|---|------|------|
| F1 | 默认沙箱下的写入类 Claude 阶段能写文件，平台也会自动提交 | 产品需求 run `6bc03cc5bd304b6a8b0fb87fbbea3ea1`（`sandbox: Standard`）写了 `specs/default-sandbox-write-check/spec.md`，已自动提交为 `1217548`（`aep-stage/9ed69533…`）。本阶段也在默认沙箱下写 `design.md`/`task.json`。**AC-1 已有第一份证据。** |
| F2 | 产品需求 run 里，一条 Bash 复合命令（`ls`/`cat` 执行目录）被拒，阶段仍显示成功 | 产品需求阶段的会话过程；印证 spec E2 |
| F3 | 提示词说「前序执行的 result.txt 和 evidence.json 位于同级执行目录」，但默认沙箱下 **读不到**同级执行目录：本阶段用 Read 读 `runs/6bc03cc5…/evidence.json` 被拒（"requested permissions to read … but you haven't granted it yet"） | 本阶段会话。原因：`--add-dir` 只加了本次执行目录 `dir`（`src/workflow_stage.rs:1124-1125`），提示词文案在 `src/workflow_stage.rs:158-162` |
| F4 | 写入拒绝的判定只认 `tool_input.file_path`。NotebookEdit 的路径字段是 `notebook_path`，被拒时会被 `filter_map` 丢掉，计数为 0，阶段误判为成功 | `src/workflow_stage.rs:1317-1336`（印证 E3） |
| F5 | 写入拒绝判定 **没有任何测试** | `rg permission_denials src tests` 只命中实现本身 |
| F6 | 插件统计只保留数量和 `@builtin` 的名字，其余插件的 name/source 都没进 evidence，所以「plugins=5」查不到来源 | `src/launch.rs:223-239` |
| F7 | 最小启动用 `--setting-sources project,local`，不读用户级 `enabledPlugins`；但插件仍有 5 个，说明有的插件不经 settings 加载（内置插件，或者 marketplace 安装后默认启用） | 待 T4 实测 |

F3 是本次验证中发现的缺陷，按 R6 纳入本事项修复。

## 2. 设计决定（回答 spec §6 的 4 个待定项）

1. **越界写入怎么触发**：分两层。
   - 确定性层：集成测试里用伪造的 `claude`，输出带 `permission_denials` 的 result 事件，走真实的 `aep workflow-stage` 流程，断言阶段失败、错误信息里有路径、没有提交。CI 里跑。
   - 真实层：带 `#[ignore]` 的集成测试，设置 `AEP_REAL_CLAUDE=<claude 路径>` 时才跑。用本机真实 Claude、Standard 沙箱、临时 git 仓库各跑一次：(a) 写 `specs/probe/spec.md`；(b) 说明里明确要求用 Write 写 `/tmp/aep-sandbox-probe-<随机>/x.md`（E5、E6）。
   - E7（Claude 不肯写）：真实层断言 stream-json 里确实出现了被拒的 Write/Edit；如果没出现，测试报「未触发拒绝」，不算通过。
2. **Bash 被拒是否算阶段失败**：**不算**。理由：
   - Bash 的拒绝项只有 `command`，分不清读还是写。F2 里只读命令被拒、产物正常，算失败会误伤。
   - 第一次事故里 Write 也同时被拒，现有规则已经能拦住。
   - 处理方式：全部拒绝项写进 evidence 的 `permission_denials: [{tool, target}]`，`target` 取 `file_path` / `notebook_path` / `command` / `path` 中第一个存在的字段。写入类阶段如果有 Bash 拒绝，在 evidence 里另记 `denied_bash` 计数，供界面和记忆查看，不改变状态。
3. **插件来源怎么查**：
   - init 事件里每个插件的完整描述（name 加上 `source` 或 `path`）写进 evidence 的 `launch.plugin_sources`（T3）。
   - 再用一次真实最小启动对照 `claude plugin list`（或 `claude plugins list`，以本机 `--help` 为准），以及 `~/.claude/plugins/installed_plugins.json` / `known_marketplaces.json`，逐个确定来源（T4）。
   - 能关的，在 `MINIMAL_FLAGS` 的 `--settings` JSON 里加 `"enabledPlugins": {"<name>@<marketplace>": false}`。只有 T4 实测确认这个键在 `--settings` 下生效，才改 `launch.rs`；不生效就只记录原因。
4. **记忆条目类型**：用 **decision**。内容包括：AC-1/AC-2 的 run_id 和结论、Bash 拒绝规则、E3 修复、插件清单与处置。由 T4 产出正文；开发阶段写不了 Studio 的项目记忆目录（不在 `--add-dir` 里），由用户在 Studio 记忆页添加，或者由阶段 handoff 自动写入。

## 3. 接口与数据变更

### 3.1 `src/workflow_stage.rs`

- 新增纯函数，并替换 `:1317-1336` 的内联逻辑：
  ```rust
  /// (写入类拒绝的目标, 全部拒绝项)
  fn denials(v: &Value) -> (Vec<String>, Vec<Value>)
  ```
  - 写入类工具：`Edit | Write | MultiEdit | NotebookEdit`。
  - 目标取 `file_path`，否则 `notebook_path`，否则用 `"<工具名>（无路径）"`。**不再丢弃**（修 F4/E3）。
  - 全部拒绝项转成 `{tool, target}`，写进 `record["permission_denials"]`，Bash 拒绝计数写进 `record["denied_bash"]`。这两项只读和写入类阶段都记录。
  - 失败文案保持 `"Claude 的文件写入被拒绝 {n} 次，本阶段没有完成：{路径，…}"`。AC-2 按这段文案匹配，不能改。
- 修 F3：Claude 启动参数在两条 `--add-dir` 之后追加只读授权
  ```
  --allowedTools Read(//<runs 目录>/**)
  ```
  `<runs 目录>` 就是 `dir.parent()`，Claude 权限规则里绝对路径以 `//` 开头。不对 runs 目录用 `--add-dir`，因为在 acceptEdits 下那等于允许写别的执行目录。
  - 路径里有空格（`Application Support`），单元测试要覆盖。
  - 真实层（T4）需要确认能读到同级 `evidence.json`。读不到时退回 `--add-dir <runs 目录>`，并在记忆里写明原因。
  - 多个 `--allowedTools` 共存：现有 `Bash`（Network）和 `Bash(check_command)` 都是各自追加一对参数，新参数同样单独追加。
- 不改：权限模式映射（`:1132-1137`）、写入类/只读类工具集。

### 3.2 `src/launch.rs`

- init 统计（`:223-239`）新增 `"plugin_sources": [String]`，每项为 `name@source`；没有 `source` 时用 `path`，都没有时只写 `name`。
- `plugins` 计数、`builtin_plugins` 保持不变，兼容现有 UI（`studio/src/lib/format.ts:149-164`）和 `studio/src-tauri/tests/api.rs:1016`。
- `MINIMAL_FLAGS` 只在 T4 确认可以关闭插件后修改。修改后 `src/launch.rs` 里的参数测试（`:265-305`）和 `tests/stage_token_diet.rs:209-210` 一起更新。

### 3.3 evidence.json 新增字段（只增不改）

```json
{
  "permission_denials": [{"tool": "Write", "target": "/tmp/aep-sandbox-probe-x/x.md"}],
  "denied_bash": 0,
  "launch": { "plugin_sources": ["agents-md@builtin", "..."] }
}
```

旧 evidence 没有这些字段，读取方一律按缺省处理。Studio 本次不展示新字段，交互设计已跳过。

## 4. 仓库边界

- 可改：`src/workflow_stage.rs`、`src/launch.rs`、`tests/`。
- 不改：`studio/`、`desktop/`、`specs/`（开发阶段）、权限模式、Codex/ZCode 路径。

## 5. 测试计划

| 测试 | 位置 | 覆盖 |
|------|------|------|
| `denials` 单测：Write 带 file_path / NotebookEdit 带 notebook_path / 写入类工具缺路径 / Bash 只计数 / 字段缺失或为 null | `src/workflow_stage.rs` 测试模块 | AC-2 文案、AC-4、AC-5 |
| 启动参数单测：Claude 参数含 `--allowedTools Read(//<含空格的 runs 目录>/**)`，与 Network 的 `Bash` 授权并存 | `src/workflow_stage.rs` 测试模块 | F3 |
| `plugin_sources` 单测：source / path / 只有 name 三种 | `src/launch.rs` 测试模块 | AC-6 |
| 集成（伪造 claude）：写入类阶段 result 带 Write 拒绝（`/tmp/...`）→ `aep workflow-stage` 退出非零，evidence `status=failed`，错误含路径，事项分支无新提交；同一阶段只有 Bash 拒绝 → 成功，`denied_bash=1` | `tests/default_sandbox_write.rs`（新建） | AC-2、AC-3、AC-4 |
| 真实层（`#[ignore]`，需 `AEP_REAL_CLAUDE`）：Standard 沙箱写 spec → 成功且有提交；越界写 → 失败，越界文件不存在；能读到同级执行目录的 evidence；打印 `launch.plugin_sources` 和 `claude_code_version` | `tests/default_sandbox_write.rs` | AC-1、AC-2、AC-3、AC-6、F3 |

命令：

```sh
cargo test --bin aep denials
cargo test --lib launch
cargo test --test default_sandbox_write
AEP_REAL_CLAUDE=$(command -v claude) cargo test --test default_sandbox_write -- --ignored --nocapture
cargo test --test stage_token_diet
```

真实层要访问本机 `~/.claude` 和网络。在默认沙箱的开发阶段里，Bash 跑不了它（F2 同类拒绝），需要用户在终端或「完全权限」阶段里跑，把输出贴回 T4。

## 6. 兼容性与风险

- evidence 只增不改，旧 run 不受影响。
- `Read(//…/**)` 规则的语法如果在本机 Claude Code 版本上不生效，F3 仍然存在。T4 会实测，不生效就按 §3.1 回退。
- 关插件依赖 `--settings` 里的 `enabledPlugins` 是否生效。不生效就保持现状，只记录原因，不算失败（R4 允许）。
