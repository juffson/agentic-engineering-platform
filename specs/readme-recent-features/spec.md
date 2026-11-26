# 文档 · README 对齐近期功能 — 产品需求（修订版）

> 本版按已确认决策 #125 完成修订：产品需求审查 run `aefb20eef2b849e6aa12c3efdf2ae329`（有条件通过，提出 C1–C3）、交互设计审查 run `f1ff22cba41245dfab57bd4feaa2f25c`（无界面改动跳过成立，退回完成修订）。修订记录见 §9；除标注外，原始 brief 内容全部保留。事实基线为 HEAD `daad3f6`（工作区干净），关键声称均已对照源码复核，证据见本次档案 `runs/dcf66547d7db4a59a03b7a19637fc11c/evidence/prd-fact-check.md`。

## 1. 背景

README.md 的最后一次实质性内容更新停在 4a743ca（2026-11-24，Tauri Studio 重写），其后 main 已前进 70+ 提交。README 与已交付功能之间既有「缺失」（流程页、插件页、远端交付等未提及），也有「矛盾」（交付方式、流程模板、工作树位置等描述已过时）。本任务把 README 与现状对齐：补上缺的部分，改掉过时的描述。

与本任务直接相关的用户可见语义基线（均已在源码复核）：

- **工作树位置**：新建阶段工作树位于 Studio 目录 `~/Library/Application Support/AEP Studio/worktrees/<仓库路径转义>/<事项 ID>` 下、仓库 `.git` 之外（src/project/branches.rs:70-76 `worktree_home`，路径中 `/`、`\`、`:` 替换为 `-`；原因：Claude 拒写 `.git` 内路径）。旧位置 `.git/aep/workflow-worktrees/` 仍被导入扫描识别为 AEP 自家工作树（src/project/import.rs:255-259，路径经 src/workspace.rs:97-102 解析为 `<git-common-dir>/aep/workflow-worktrees`）。
- **流程 CLI**：`Flows/FlowShow/FlowSave/FlowCopy/FlowDelete`（src/project_cli.rs:86-105）、`add-work --flow`（src/project_cli.rs:955）；**远端交付 CLI**：`Upstream`（src/project_cli.rs:193）、`DeliverPr`（src/project_cli.rs:217）。

## 2. 用户场景

- **S1 新读者**：只读 README 能找到 Studio「流程」「插件」「分支」页面的入口说明，能区分自定义流程（可建可编）与内置流程（只读），并知道事项保存的是流程快照。
- **S2 CLI 用户**：能从 README 查到 `aep project flows`、`aep project flow-show`、`aep project upstream`、`aep project deliver-pr` 等命令的用法与示例；示例命令在当前代码上可跑通（子命令与参数名以 src/project_cli.rs 的 clap 定义与 `--help` 为准）。
- **S3 运维/排障用户**：从 README 能确定事项工作树的实际位置（Studio 目录下），并知道旧项目位于 `.git/aep/workflow-worktrees/` 的工作树仍被平台识别。
- **S4 交付用户**：从 README 能了解两种交付方式（本地合并 / 推送并提 PR，api/manual 等模式、可绑定已有 PR、之后跟踪 PR 状态），并知道默认沙箱下阶段可写本事项工作树、越界写入被拒且阶段失败。

## 3. 范围

### 3.1 本轮覆盖（「近期功能」划界）

只改 README.md，补齐与修正以下已交付能力（差距清单，每条写进 README 前先在代码里核实）：

1. **可编排的阶段流程**（#121；specs/orchestrable-workflows/，src/flow.rs，src/project.rs 的 ProjectFlow/WorkFlow，studio/src/components/Flows.tsx）。README 30–33 行只列了两个固定模板。补：侧边栏「流程」页、新建或编辑自定义流程、内置流程只读、新建事项时选流程、事项保存流程快照；CLI 相关用法（project_cli.rs:86-105 的 `flows`/`flow-show` 等、`add-work --flow`）一并说明。
2. **插件菜单与分支远端**（#122；specs/studio-plugins-branch-remote/，studio/src-tauri/src/plugins.rs，studio/src/components/{Plugins,Branches}.tsx）。补：插件页、git-tools.json 配置（含「在访达中显示」：lib.rs 注册 `revealGitToolsConfig` → plugins.rs `reveal_git_tools_config`）、按主机绑定的令牌及其存储方式（macOS 钥匙串，src/project/secret.rs:86-102；主机归一 src/project/forge.rs:84 `normalize_host`）、分支页的远端状态和刷新。
3. **从远端目标开启事项、通过 PR 交付、显示每个阶段的输出**（daad3f6；specs/remote-linkage/design.md；project_cli.rs:193 `Upstream` / 217 `DeliverPr`；pr 模式 `auto|api|agit|manual`、绑定已有 PR（`--force-with-lease`）、跟踪 PR 状态；「显示每个阶段的输出」对应 studio/src/components/Output.tsx 的 stageOutput/stageDiff）。README 41–42 与 191 行只写「合并到本地目标分支」，改成两种交付方式都覆盖；一并补仓库「远端与交付」配置（remote/target）、事项从 `<远端>/<目标>` 开分支、阶段跟随远端基线。
4. **默认沙箱下阶段可写文件的用户可见语义**（#123；specs/default-sandbox-write-check/；ad25597 使「写入被拒 → 阶段失败」成为用户可见语义）。按 §8 落定 2：只在 README:38 附近的权限说明补一句用户可见行为，不写内部验证细节。
5. **Studio 验证命令段核对**（README 204–212）：核对 `test:ui`（studio/package.json）与 cargo 测试命令是否仍准确，是否新增需要说明的测试（如 studio/tests/flows.spec.ts）。
6. **工作树位置修正**（C1 新增的第 6 条差距）：README:92 现文「事项工作树在仓库的 `.git/aep/workflow-worktrees/`」与 branches.rs:70-76 矛盾。改为现位置（Studio 目录下，见 §1），并用一句说明旧位置的工作树仍被识别（import.rs:255-259）；不得再表述为新建工作树位于 `.git` 内。

以及**全局矛盾清零**：README 里所有与现状矛盾的描述一并清除（判定与核对方法见 §6），不限于上述 6 条涉及的段落。

### 3.2 显式排除（后续事项处理，本任务不新增描述）

以下均为 4a743ca 之后已交付的能力，但**不在本轮范围**，本任务不为它们新增任何描述性章节或条目（AC-7）：

- 用量页与阶段 token 预算（3c5f13a / a9149bf；studio/src/components/Sidebar.tsx:81、Budget.tsx、Usage.tsx）
- 项目记忆版本化（ebeaeb8）
- 导入已有仓库/分支与 Claude 记忆（6b40e22 / 0f0e23f；project_cli.rs `ImportScan`/`ImportSync`，118–125 行附近）
- `aep project remove-work`（16dcf68；project_cli.rs:118）
- 草稿事项用模型 CLI（ac6d5e6）
- 分支页的提交图与工作树管理中与「远端状态/刷新」无关的部分（0b293ab；差距第 2 条只覆盖分支页远端能力）

排除规则的边界：若全局扫描（§6）发现 README 已有与这些功能相关的**矛盾**描述，仍须删除或修正（AC-2 优先于划界）；划界只限制「新增描述」。

### 3.3 其他不做

不改代码；不改 docs/pilot*.md 与其他任务的 specs/ 文档（本 PRD 所在 specs/readme-recent-features/ 为阶段产物）；不新增截图；不翻译。

## 4. 边界情况

- **E1** README 出现但源码中不存在的命令、子命令、参数或文件路径（按 §6 扫描核对）→ 判失败。
- **E2** 工作树说明只写新位置、漏掉旧位置兼容说明 → 老项目用户无法定位既有工作树，AC-2 视为不通过。
- **E3** §3.2 排除清单中的功能被新增进 README → 范围蔓延，AC-7 失败；反之它们已有矛盾描述而未清除 → AC-2 失败。
- **E4** 交付方式描述写成二选一（只写本地合并或只写提 PR）→ 与现状（两者并存）矛盾，AC-2 失败。
- **E5** 修正导致 README 现有仓库内相对链接失效 → AC-4 失败。

## 5. 验收标准

- **AC-1** 差距第 1–3 项在 README 里都有对应说明，并标明入口（Studio 页面名或 CLI 命令）。
- **AC-2** README 里不再有和现状矛盾的描述；至少三处旧表述必须消失并被与源码一致的说法替换——①「交付只能合并到本地目标分支」；②「模板只有两种固定流程」；③「事项工作树在仓库的 `.git/aep/workflow-worktrees/`」。且按 §6 方法对 README 全部命令与路径执行扫描核对，不存在任何源码中没有的命令/参数/路径。
- **AC-3** 新增或修改的每条 CLI 命令、参数、文件路径（如 git-tools.json、令牌存储位置、工作树位置）都在源码里找得到；交付时附「README 行号 ↔ 源码位置」核对清单。
- **AC-4** README 里的仓库内相对链接都指向存在的文件（外部链接如 Notion 不在约束范围）。
- **AC-5** 实现改动只有 README.md（git diff --stat 为证；specs/readme-recent-features/ 为本阶段产物，不计入）。
- **AC-6** 不写入令牌、真实主机地址、个人信息等敏感内容；示例里的远端和主机用占位名，如 control.example.org。
- **AC-7** git diff 的新增行里没有对 §3.2 排除清单功能的描述性内容（划界可验证）。

## 6. 矛盾扫描方法（AC-2 的可执行核对程序）

1. **命令提取**：`rg -n 'aep [a-z][a-z-]+' README.md` 列出 README 提及的全部 CLI 子命令（含行内反引号命令与 ```sh 代码块）。
2. **命令核对**：每个子命令/参数在 src/ 与 studio/src-tauri/ 下 `rg -n '<名称>'` 验证存在；枚举命令以 src/project_cli.rs 的 Action 枚举与 `--help` 文案为准，参数名以 clap 定义为准。
3. **路径提取与核对**：列出 README 提及的文件/目录路径（`~/Library/Application Support/AEP Studio/…`、`aep/memory.sqlite3`、`git-tools.json`、工作树位置等），逐一验证：仓库内路径 `test -e`；本机约定路径与源码常量比对（src/workspace.rs、src/project/branches.rs:70-76、studio/src-tauri/src/plugins.rs 等）。
4. **指名矛盾复核**：对 AC-2 三处旧表述 rg 确认已从 README 消失，新表述与对应源码行为一致。
5. **记录**：每条命令/路径的核对结果（README 行号 ↔ 源码位置）写入交付时的核对清单（同时满足 AC-3）。

## 7. 交付物

- README.md（唯一实现改动文件）。
- 「README 行号 ↔ 源码位置」核对清单（随开发实现的交付证据提交，满足 AC-2/AC-3）。

## 8. 已落定的待确认问题（原 brief 4 项，按产品需求审查 §3 建议答复与决策 #125）

1. 处理方式：按「补齐并修正」处理，与事项标题一致。
2. #123 默认沙箱写入：写入 README，但只写用户可见行为一句——默认沙箱下阶段可写本事项工作树，越界写入被拒且阶段失败；内部验证细节（evidence 字段、插件清单）不进 README。
3. README:192 的 Notion 外链与「八周路线图」保留，不属于矛盾项；AC-4 只约束仓库内相对链接。
4. 交互设计：跳过——本任务只改 README.md 文档内容，不改任何 Studio 页面、组件或 CLI 行为，无用户路径、布局或空/加载/失败态需要设计。交互设计审查轮已复核该理由并结案。

技术方案按同一理由跳过：单文件文档任务，无接口、数据结构或兼容性决策；本 PRD（§3.1 差距清单、§3.2 划界、§5 验收标准、§6 扫描方法）即开发实现的执行清单，测试验证按 AC-1–AC-7 逐项核对。

## 9. 修订记录（相对原始 brief，对应审查意见 C1–C3）

- **C1**：差距清单 5 → 6 条，新增第 6 条（README:92 工作树位置改为 Studio 目录下，并说明旧路径兼容）。
- **C2**：新增 §3.1/§3.2 「近期功能」划界——纳入 #121/#122/#123/daad3f6 与全局矛盾清零；排除清单显式列出 6 项后续处理，并写明「矛盾清零优先于划界」的边界（E3）。
- **C3**：新增 §2 场景 S1–S4、§4 边界 E1–E5、§6 矛盾扫描方法；AC-2 由全局表述细化为「三处指名矛盾 + 扫描结果为空」；新增 AC-7 使划界可验证；原 AC-1/AC-3/AC-4/AC-5/AC-6 编号与语义保持不变。
