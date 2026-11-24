# 验证 · 默认沙箱下 Claude 阶段能写文件 — 产品需求

事项：work-a7ec0f2906ac4f05b77f6bf170cd84a1
本阶段 run：`6bc03cc5bd304b6a8b0fb87fbbea3ea1`（`request.json` 中 `sandbox: "Standard"`，Claude，产品需求阶段）

## 1. 背景

- 第一次跑 Token 优化的产品需求时，工作树在 `.git/aep/workflow-worktrees/` 下，Claude 把 `.git/` 当敏感路径，写入、mkdir、touch 全被拒，阶段却显示成功。
- ad25597 做了三件事：
  - 阶段工作树移到 `<catalog>/worktrees/<repo>/<checkout>`；
  - Claude 启动时带 `--add-dir <执行目录>`，有 receipt 时再加 receipt 的父目录（`src/workflow_stage.rs:1121-1125`）；
  - 写入类阶段的结果里只要有 Edit/Write/MultiEdit/NotebookEdit 的 `permission_denials`，阶段就失败，并列出被拒的 `file_path`（`src/workflow_stage.rs:1317-1336`）。
- 权限模式映射（`src/workflow_stage.rs:1132-1137`）：
  - 完全权限 → `bypassPermissions`；
  - 默认沙箱 / 联网 且写入类阶段 → `acceptEdits`；
  - 其余 → `default`。
- Token 优化的 19 次执行全部开在完全权限，所以 `acceptEdits` 这条路径没有在真实执行里验证过。
- 最小启动（`src/launch.rs:19-24`）带 `--strict-mcp-config --setting-sources project,local --disable-slash-commands --settings {"autoMemoryEnabled":false}`。即使这样，实测 init 里仍有 `plugins=5`，来源没查过。init 统计只单独列出 `@builtin` 来源的插件（`src/launch.rs:223-239`）。

本事项是验证型事项：主要产出是真实执行证据和项目记忆。只有验证中发现缺陷时才改代码。

## 2. 用户场景

- **S1 正常写入**：用户用默认沙箱跑一个写入类 Claude 阶段（如产品需求写 spec）。文件写进阶段工作树，阶段成功，平台自动提交到 `aep-stage/<id>`。
- **S2 写入被拒**：阶段试图用 Edit/Write 写工作树和 `--add-dir` 以外的位置，被 Claude 拒绝。阶段状态为失败，错误信息列出被拒路径，不能显示成功。
- **S3 查看插件来源**：用户在 Token 消耗页或启动配置里看到「插件 5」，想知道是哪 5 个、从哪里加载、能不能关。
- **S4 回看结论**：之后有人怀疑默认沙箱写不了文件时，查项目记忆就能看到结论和对应 run_id，不用重跑。

## 3. 范围

### 包含
- **R1**：在默认沙箱（Standard）下真实执行至少一次写入类 Claude 阶段，记录 run_id、写入的文件、自动提交的 commit。
- **R2**：在默认沙箱下真实执行至少一次故意越界的写入（用 Write/Edit 写工作树外的绝对路径），记录 run_id、阶段状态和错误信息。
- **R3**：查清最小启动后 init 中 `plugins` 的每一项（name、source/path），说明各自从哪里加载（内置 / 用户级 `~/.claude` / marketplace / 项目级 等）。
- **R4**：对 R3 中每个插件给出处置：能用启动参数或设置关掉的，在最小启动里关掉；关不掉的记录原因（如 `@builtin` 无开关）。
- **R5**：把结论和全部 run_id 写进项目记忆，类型为 decision 或 note。
- **R6**：如果验证发现缺陷（如 S2 显示成功、Bash 写入被拒没被识别），在本事项中修复并补测试，或者在记忆里登记为后续事项。具体选哪种由技术方案决定。

### 不包含
- 不改权限模式映射本身，也不引入新的沙箱档位。
- 不验证 Codex 客户端的沙箱行为。
- 不追求把插件数降到 0；目标是「来源清楚、可关的已关」。

## 4. 边界情况

- **E1**：本 run（`6bc03cc5…`）就是 S1 的一次实例：Standard、Claude、写入类阶段，写的是 `specs/default-sandbox-write-check/spec.md`，本阶段内 `mkdir` 也成功了。它能不能算 R1 的证据，取决于平台在本阶段通过后是否把该文件自动提交到 `aep-stage/9ed69533e2144c7298b64da09753e5ea`，需要事后核对。
- **E2**：越界写入只拦截 Edit/Write/MultiEdit/NotebookEdit 的 `permission_denials`。用 Bash（`mkdir`/`touch`/`echo >`）被拒时，不会让阶段失败。第一次事故里 mkdir/touch 也被拒，所以需要确认 Bash 被拒时阶段的表现，并决定是否纳入失败判定。
  - 本 run 已有一例：一条 Bash 复合命令（`ls`/`cat` 执行目录）在默认模式下需要审批，被直接拒绝；改用 Read 工具读取后成功。如果本阶段最终显示成功，就说明 Bash 被拒不影响阶段状态。这次被拒是只读命令，不影响产出，但规则同样适用于写入命令。
- **E3**：`permission_denials` 的 `tool_input` 若没有 `file_path`（如 NotebookEdit 用 `notebook_path`），当前 `filter_map` 会丢掉这一项，计数变成 0，阶段被误判为成功。需要核实这种情况。
- **E4**：只读阶段（`write == false`）被拒写入时不判失败，这是预期行为，不属于缺陷。
- **E5**：越界目标不能是仓库主工作区或其他事项的工作树，以免误写。用 `/tmp` 下的唯一路径或 `$HOME` 下明确不存在的路径。
- **E6**：`--add-dir` 包含执行目录（runs/<run_id>）。往执行目录写入算「允许」，不能拿它当越界测试。
- **E7**：故意越界时，Claude 可能拒绝执行或改写到工作树内。如果这样，阶段可能成功，但证据不成立。验证时需要从 stream-json 里确认确实发生了被拒的 Write/Edit 调用。
- **E8**：插件列表与 Claude Code 版本和用户本机 `~/.claude` 状态相关，记录时要带上 `claude_code_version` 和日期。
- **E9**：`--setting-sources project,local` 已排除 user 设置，但插件可能不经 settings 加载（如 marketplace 安装目录、内置插件），需要分别说明。
- **E10**：工作树里如果残留越界测试产生的文件，不能被自动提交进事项分支。

## 5. 验收标准

- **AC-1**：有至少一个 Standard 沙箱的写入类 Claude 阶段 run，状态为成功，写入的文件出现在该事项分支的自动提交里。记录 run_id 和 commit。
- **AC-2**：有至少一个 Standard 沙箱的写入类 Claude 阶段 run，stream-json 中有被拒的 Write 或 Edit（目标在工作树和 `--add-dir` 之外），阶段状态为失败，错误信息包含「文件写入被拒绝」和被拒的完整路径。
- **AC-3**：AC-2 的越界目标路径在执行后不存在，事项分支里也没有越界产物。
- **AC-4**：对 E2（Bash 写入被拒）给出实测结论：阶段失败，或者阶段成功并附说明。如果判定为缺陷，按 R6 处理。
- **AC-5**：对 E3（无 `file_path` 的拒绝项）给出结论（代码核实即可），必要时按 R6 处理。
- **AC-6**：列出最小启动 init 中全部插件的 name、source 和加载来源，总数与 `plugins` 计数一致，并附 Claude Code 版本。
- **AC-7**：每个插件标注「已关闭（用的参数/设置）」或「无法关闭（原因）」。如果最小启动参数有改动，`launch.rs` 的测试同步更新并通过。
- **AC-8**：项目记忆新增一条，包含 AC-1、AC-2 的 run_id 和结论、E2/E3 结论、插件清单与处置。

## 6. 待技术方案确认

1. 越界写入怎么稳定触发：在说明里明确要求写某个绝对路径，还是用测试夹具。
2. Bash 被拒是否纳入失败判定。stream-json 的 `permission_denials` 里 Bash 项只有 `command`，没有路径，需要定判定规则和错误信息格式。
3. 插件查询方式：用一次真实最小启动的 init，还是另做探针（如 `claude plugin list`）。
4. 记忆条目归 decision 还是 note。
