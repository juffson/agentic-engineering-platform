# Agentic Engineering Platform · AEP

AEP 的首个试点把 Git 中的需求、隔离工作区中的代码改动和针对同一提交的验收证据连起来。GitHub 保存任务和 PR，执行器在本地运行。示例只使用合成行情数据。

## 从一次派发到一个可审查的提交

需要 macOS 或 Linux、Git、Rust 1.89+、Cargo 和 C 编译器（用于编译内置 SQLite）。在仓库根目录安装 CLI，然后验证已派发的 Rust 行情示例。

```sh
cargo install --locked --path .
git fetch origin
aep run --task market-volume-dedup-rust --candidate origin/aep-task/market-volume-dedup-rust
```

命令会读取 GitHub 上的 `aep-task/market-volume-dedup-rust` 分支，在独立 Worktree 验证候选提交，打印本地 `evidence.json` 路径。报告包含 Spec SHA、候选 SHA、修改文件、测试命令、退出码与审查状态。失败的工作区会保留供排查。核对报告后可用 `git worktree remove <路径>` 清理。执行器核验通过后保留候选引用 `refs/aep/runs/<run_id>`；本地报告位于 Git 公共目录的 `aep/runs/` 下。

已有仓库可直接使用原 Git 公共目录中的 SQLite 记忆库、Run 锁和 JSON 证据。历史派发继续使用其固定提交里声明的验收命令。新的行情示例使用独立任务 ID `market-volume-dedup-rust`。

### AEP Studio 项目工作台

macOS 上可用 Rust 桌面端浏览执行记录、写入和修订共享记忆。安装 Xcode 及命令行工具，使用当前稳定版 Rust，在仓库根目录运行：

```sh
./desktop/bundle-macos.sh
open "desktop/target/AEP Studio.app"
```

从「项目库」创建或打开项目，在「关联仓库」中选择一个或多个本地 Git 仓库。新建功能或版本时可选择开发模板。

已有项目可以直接点左栏「打开项目目录…」，选择仓库或其子目录。Studio 会打开关联的项目。如果用 `aep project --project 项目ID --work 事项ID bind` 绑定过该工作区，会直接进入同一项目库的对应事项。「立即同步」重新读取当前项目库和阶段回执。独立 CLI 会话用 `project claim`、`project submit` 登记进度和产物后，打开的工作台也会自动刷新。

| 模板 | 阶段 |
| --- | --- |
| 软件开发 | 产品需求 → 交互设计 → 技术方案 → 开发实现 → 测试验证 → 独立验收 → 交付归档 |
| 游戏开发 | 玩法需求 → 影响分析 → 环境预检 → 游戏实现 → 运行验证 → 玩家验收 → 交付归档 |

点击流程图中的阶段，可预先选择 Codex、Claude、ZCode 或自定义 CLI、模型和工作仓库。Studio 显示检测到的程序路径，也可填写自己的可执行文件。模型留空使用客户端当前配置。ZCode 支持显式的 `provider/model` 或 `provider/model@推理档位`。自定义 CLI 从标准输入读任务，在标准输出写交付物，指定模型时接收 `--model`。Claude 可在当前阶段单独允许一条精确的验收命令。

填写阶段说明并点击「运行阶段 Agent」。同一事项在每个仓库使用独立的本地工作树，后续阶段继续使用其中的文件。「执行档案」保留提示词、答复、原始日志和记忆回执。「项目记忆」可查看和修订 Agent 的交接，新的阶段会读取当前版本及本事项已确认的交付物。测试和独立验收需要返回通过结论。核对结果后，点击「完成并进入下一阶段」或「确认完成交付」。运行期间可以关闭 Studio，重新打开后继续查看、取消或重试。已有五阶段事项保留经典流程。

#### 项目设置与事项交接

「项目设置」按仓库管理执行环境。从这里可以查看 Worker、队列、Deploy Key 和滚动轮换历史，也能打开仓库原有的两轮接力、隔离全流程和 HTTPS 任务归档。首次配置会带出仓库已有连接，保存项目连接后只对当前项目的这个仓库生效。配置中保存控制面地址、CA 和操作员令牌的文件路径。

打开「开发事项 → 协作与交接」，可进入该事项的两轮接力、HTTPS 任务或本地验收。从事项发起的新记录会自动关联，也可用「关联已有仓库记录」选回以前的执行。解除关联只移除事项里的引用。给同一项目的另一事项发送摘要后，接收方能查看来源事项、阶段和结论；下一次阶段执行或独立 CLI 预览会带上最近的交接。每个来源保留最新一条摘要。

```sh
aep project --work 对账单导出 handoff --to 发布验收 --note '空对账单场景已实现，验收见 fixtures/empty.csv'
aep project --work 发布验收 context
aep project --work 对账单导出 link-record --repository . relay RELAY_ID
aep project --work 对账单导出 status
aep project site --repository . --endpoint https://control.example.org --operator-file /path/to/operator
```

#### 在已有 Agent 会话里交接

`aep project` 与 Studio 使用同一份项目库。事项间的定向交接保存在项目库。项目共享决策和阶段 Agent 写入的交接摘要存在每个项目自己的 SQLite 中，完整的阶段结果、执行回执和主动提交的文件按执行 ID 归档。默认给下一位 Agent 的内容包括当前职责、阶段说明、有效决策、上游交接摘要和档案位置。可用 `project memory get ID` 读取完整记忆。

在其中一个 Git 工作区建立项目和事项后，可把当前工作树绑定到它们。绑定存于该工作树的 Git 元数据目录。多个工作树可以绑定不同事项。

```sh
aep project create 支付平台 --repository "$PWD"
aep project add-work 对账单导出
aep project bind
aep project context
```

在已经打开的 Claude、Codex、ZCode 或其他终端会话中，领取时填写会话名或原客户端的会话 ID。会话继续在自己的工作目录工作。Studio 事项页面也可复制领取命令。外部会话只需能执行本机的 `aep`。

```sh
aep project claim --stage 1 --client claude --session prd-design
```

领取返回 `attempt`、`context` 和项目记忆游标 `next_since`。在下列命令中将 `RUN_ID` 和 `CURSOR` 替换成回执值。`context --since` 只返回本次领取适用的项目记忆变更，`has_more` 为真时用新的游标继续读。已领取的阶段可以重复查询。没有变更时交接文本为空。

```sh
aep project context --attempt RUN_ID --since CURSOR
aep project submit --attempt RUN_ID --session prd-design --file prd.md --summary '范围已定，AC-1 涵盖空对账单，下一步设计导出交互。' --artifact prd.md
aep project show --attempt RUN_ID --artifact prd.md
aep project advance --stage 1
```

`--artifact` 可重复，路径相对于领取时的工作目录，文件以提交当时的内容归档。测试与独立验收通过时在结果末行写 `AEP: PASS`，有阻断时用 `submit --blocked` 提交原因。取消外部领取会关闭该次交接，原客户端独立运行。失败或取消后用 `claim --retry` 重新领取。

直接从命令行托管模型时，用 `project configure --stage N` 设置客户端、模型、工作仓库和说明，再用 `project run` 执行当前阶段。人工步骤可用 `advance --stage N --note '结论'` 完成。`list` 跨项目查看当前负责人，`status` 查看事项和历次执行，`metrics` 查看每次执行的实际输入字节数、耗时和客户端返回的用量字段。外部会话的推理用量由原客户端记录。

默认共用本机 Studio 项目库。需要单独存储时，在启动 Studio 和 CLI 前设置相同的绝对目录 `AEP_PROJECT_HOME`，CLI 也接受 `--home`。在其他目录中可用 `--project` 和 `--work` 指定名称或 ID。上述本地交接要求参与方可访问同一份项目库和档案。

可在支持 Metal 的 macOS 上复验两套模板。游戏场景使用可重复的本机测试替身，覆盖预检失败、重开重试和记忆修订。软件场景调用本机已配置的三个真实模型客户端，在输出目录中创建独立测试仓库并保存七个阶段的回执。可以追加 `--codex-model` 和 `--zcode-model` 选择账号可用的模型。

```sh
cargo run --locked --manifest-path desktop/Cargo.toml --example ui_debug -- --scenario workflows --screenshots --output-dir /tmp/aep-game-workflow
cargo run --locked --manifest-path desktop/Cargo.toml --example ui_debug -- --live-project-workflow --screenshots --output-dir /tmp/aep-software-workflow
```

在事项下点击仓库的阶段入口，可创建独立的本地交付任务，或关联该仓库已有的任务。完整的运行记录、共享记忆、接力和试点工具可从「关联仓库」进入。项目库保存在本机的 `~/Library/Application Support/AEP Studio/projects.json`，任务与证据保存在对应仓库。首次启动也可以用已有仓库建立初始项目。

```sh
cargo run --locked --manifest-path desktop/Cargo.toml -- --repo /path/to/repo
```

在「运行记录」筛选待审查或需要处理的 Run。详情列出 Spec、候选提交、验收命令、退出码和审查报告。「Agent 记忆交接」按角色列出读写的记忆 ID。点 ID 查看当时引用的版本，再点「来源 Run」回到写入它的那次运行。「共享记忆」支持搜索正文、查看历史版本和修订链。执行新任务或写入记忆后，点击「刷新」读取最新记录。

在「共享记忆」点「新建记忆」可写入仓库约定，也可限定到任务与 Spec 或某个候选提交。从运行详情点「为此任务写记忆」会预填任务名和提交。保存时会把 Git 引用解析为固定 SHA。启用 `--shared-memory` 的后续执行会按这些范围和上下文预算选取记录。

在当前记忆的详情点「修订」，可以更正正文，或将已回答的「未决问题」改为「决策」。新版本记为人工写入，继承原范围。历史 Run 仍链接到当时读写的版本。如果两人同时修订同一条记录，先保存的版本生效，另一位的草稿留在编辑器中。

记忆详情的「读取记录」列出读取过所打开版本的 Run 和 Agent 角色。点击 Run 可回到对应的执行证据。打开上一版本可查看它自己的读取记录。

桌面端和 CLI 使用所选克隆的同一个本地 SQLite 库，关联 Worktree 共用记录。CLI 可独立安装。

### 在 Studio 发起两轮接力

使用上面的打包版 Studio。先在本机登录 Codex CLI 和 Claude Code，并用 `aep dispatch` 将任务派发到仓库的 `origin`。从源码启动 Studio 时，可将同版本的 `aep` 放入 `PATH`，或将 `AEP_STUDIO_RUNNER` 设为该执行器的绝对路径。

1. 打开仓库，进入「两轮接力」，选中已派发任务并点击「发起第一轮」。可填写 Codex 模型，留空使用客户端默认配置。
2. 第一轮验收和审查完成后，点击未决问题旁的「修订为决策」，编辑并保存。若两个 Agent 都未留下问题，可新建任务决策，或选用当前 Spec 已有的决策。
3. 点击「用此决策发起第二轮」。第二轮从第一轮候选提交继续，沿用同一个 Spec SHA。执行器会把选中的决策版本优先放入两个 Agent 的上下文。如果该版本已被替换或不再适用，本轮会报告具体原因。
4. 接力页自动刷新准备、实现、验收和审查进度。每轮的「执行证据」可打开提交、验收日志、审查报告和记忆读写 ID。失败时可以查看原因和日志，并按轮重试。重试保留之前的尝试，重新启动 Studio 后可从「接力记录」继续查看。两轮完成后候选仍等待人工审查。

### 人工审查并创建 GitHub PR

第二轮通过后，点「查看 Diff 与创建 PR」。确认 `origin` 的取回与推送地址指向同一个 GitHub 仓库，并在本机使用 `gh auth login` 登录有推送和建 PR 权限的账号。

在「最终 Diff」「验收日志」和「审查报告」之间切换即可核对候选。默认使用 GitHub 默认分支，也可以填写现有目标分支并点「加载目标 Diff」。Diff 使用所选分支与候选的合并基点，右侧显示仓库、目标分支和两个固定 SHA。核对后勾选确认并点击「确认并创建 GitHub PR」。修改目标名称或发现远端目标已更新时，重新加载 Diff 再确认。

Studio 将候选推送到 `aep/pr/<task>/<run-id>`，并保存确认账号、候选 SHA 和 PR 链接。推送成功而建 PR 失败时，可重开该审查页后重试。Studio 会复用匹配的远端提交，并检查该分支是否已有 PR。GitHub CI 使用已派发的 Spec 和 PR 当前提交独立验收。

### 在 Studio 跟踪 GitHub PR 并确认合并

创建 PR 后，从接力页或发布页点「查看 GitHub CI 与审查」。页面会自动刷新，也可以手动刷新。「CI 检查」显示当前 PR head 的检查状态。失败时打开原因，可查看 GitHub 提供的摘要、注释及 Actions 日志末尾。「审查意见」显示近期审查、普通评论和行内意见，可跳到 GitHub 查看完整上下文。

检查通过且 GitHub 允许合并后，核对当前检查和审查意见，勾选确认并点击「确认合并」。Studio 会重新读取 GitHub 状态，并向 GitHub 提交匹配人审候选 SHA 的合并请求。状态或提交变化时，刷新后重新确认；分支保护和权限由 GitHub 执行。完成后可查看合并提交、账号和时间，本地 Run 的 `merge.json` 保存确认记录。

### 合并后收尾

GitHub 确认已合并后，在同一 PR 页点「同步目标分支并清理」。Studio 从 `origin` 同步目标分支，先确认远端仍包含本次合并提交，再快进本地分支。若目标分支有独有提交，或检出它的 Worktree 有未提交或未跟踪文件，界面会显示原因，处理后可重试。

同步完成后，Studio 会删除仍指向该候选 SHA 的远端 AEP 候选分支，并清理相应的本地远端追踪引用。用户创建的本地同名分支可继续使用。Studio 仅移除本次接力 Run 记录中已经结束、可核对归属且干净的临时 Worktree。含未提交或未跟踪文件、新提交或被锁定的 Worktree 会列在结果中，供处理后重试。

两轮证据、候选归档引用、共享记忆和合并记录继续存放在原仓库。第二轮 Run 的 `cleanup.json` 保存每步结果。可以重开 Studio 查看接力的收尾状态和两轮证据。

### 重复派发与本地恢复

派发方可以给一次执行指定稳定的 `--run-id`。在同一设备和同一仓库（包括它的其他 Worktree）重送消息时，沿用该 ID。以下示例可执行两次；第二次复用第一次的证据。

```sh
aep run --task market-volume-dedup-rust --candidate origin/aep-task/market-volume-dedup-rust --run-id market-review-001
aep status --run-id market-review-001
```

`status` 离线读取本地 JSON。并发重复派发返回退出码 75，表示执行权暂时被占用。运行结束后再次派发，成功返回 0，失败返回原失败信息及退出码 1。相同 ID 绑定任务、Spec 提交、候选提交或实现客户端、审查方式和超时。候选分支移动或参数变化时，使用新的 ID。

若执行器在写入最终结果前退出，`status` 显示 `needs_reconciliation`。先检查报告中的 `worktree`、候选提交和本地子进程，再决定是否以新 ID 发起下一次尝试。成功结束的工作区会被清理。省略 ID 仍会创建新的 Run。历史版本的证据可用 `status` 读取。

### 新任务

在 Studio 的「隔离全流程」选择仓库和目标分支，点「创建并派发任务…」。填写任务 ID、Spec、每行一个的允许修改文件，以及 JSON 数组形式的验收命令，例如 `["cargo", "test", "--locked"]`。数组里的每项是单独的参数。

「准备派发」在独立克隆中生成并固定 Spec 提交。确认目标和 Spec SHA 后，点「派发并加入队列」。轮到任务执行时，Studio 通过现有的派发协议创建 `aep-task/<task>` 引用，随后启动第一轮。重试会使用已准备的同一份 Spec。任务 ID 已占用或目标分支更新时，Studio 会提示重新准备。派发记录和 Spec 随最终证据归档。

在 `specs/<task>/` 编写 `spec.md` 和 `task.json`，声明准确的可修改文件和验收命令，提交后由维护者派发。首次派发新需求时，指定经过评审的版本：

```sh
aep dispatch --task <task> --rev <reviewed-sha>
```

派发将远端任务分支固定到本次 Spec 提交。相同版本可重复派发，已存在的任务引用不能通过该命令改指其他版本。修订需求需使用新的任务 ID。维护者应限制任务分支的写权限。

### 客户端执行

已在本地配置 Codex CLI、Claude Code 或 ZCode、且仓库内容获准发送给对应模型供应商时，可选择实际客户端。该模式使用各自的模型账户与额度。实现端可选 Codex 或 ZCode，审查端可选 Claude 或 ZCode。

```sh
aep run --task market-volume-dedup-rust --implementer codex --reviewer claude
aep run --task market-volume-dedup-rust --implementer zcode --reviewer manual
```

Codex 在独立 Worktree 生成候选提交。Claude 接收该版本的 Spec 和 Diff，禁用工具，审查文本保存在同一次运行的 `review.txt`。ZCode 实现端以编辑模式执行附件中的任务，审查端以计划模式只读核对 Spec 与 Diff。指定模型时 ZCode 改走本机 app-server 会话并锁定所选模型。人工在 GitHub 查看 Diff、CI 和审查意见并决定是否批准。选用现有候选提交时可单独加 `--reviewer claude` 或 `--reviewer zcode`。默认走人工审查，不会调用模型。协议测试用本地替身验证适配器调用，不消耗模型额度。

### 两个 Agent 如何共享记忆

在一次执行中加上 `--shared-memory`，Codex 会收到相关工程约定，完成修改后产出交接摘要和可选的未决问题。候选提交验收通过后才会发布这两条内容。Claude 随后读取同一候选提交的摘要，再写下自己的审查摘要。验收失败时，可在该 Run 的证据目录里查看原始交接。独立 Run 或关联 Worktree 也可以读取适用的已发布记录。

```sh
base=$(git rev-parse origin/aep-task/market-volume-dedup-rust)
aep memory add --task market-volume-dedup-rust --spec "$base" --kind decision --text '同一来源的重复事件采用首次记录。'
aep run --task market-volume-dedup-rust --implementer codex --reviewer claude --shared-memory --run-id market-handoff-001
aep status --run-id market-handoff-001
```

Studio 的「两轮接力」可分别选择实现端（Codex 或 ZCode）和审查端（Claude 或 ZCode）。第二轮或重试前可以更换客户端。两端都选 ZCode 时共用一个模型配置，各自在独立会话中执行。模型框留空使用 headless 默认配置，填写 `provider/model@档位` 会通过 app-server 锁定模型。模型与档位需已在本机 ZCode 配置；需要交互授权时，请查看该轮执行日志。已保存的接力和本地队列会保留所选配置，旧记录沿用 Codex 实现、Claude 审查。

如果本机配置的模型不在当前 Codex 账号的可用列表中，可在上述 `run` 命令加上 `--codex-model <模型名>`。该选择只对这次 Run 生效，并记入执行证据。ZCode 的实现端或审查端可用 `--zcode-model provider/model` 或 `provider/model@推理档位` 指定模型，同样只对本次 Run 生效并记入证据。ZCode 的交接摘要和未决问题由最终答复末行的 `AEP-RELAY: {json}` 返回，验收通过后与 Codex 的结构化交接进入同一发布链路，审查端同样按 criterion、文件和行号校验阻断发现。

| 范围 | 适合记录的内容 | 何时会被选中 |
| --- | --- | --- |
| 仓库 | 操作人通过 `memory add --project` 写入的工程约定 | 任一任务可读 |
| 任务与 Spec | 需求决策、跨轮次仍需回答的问题 | Task 和 Spec 提交都相同 |
| 候选提交 | 实现交接、审查摘要 | Task、Spec 和候选 SHA 都相同 |

`memory list --task <task> --spec <sha> --candidate <sha>` 展示该版本可用的上下文。Run 报告中的 `memory_inputs`、`memory_outputs` 分别保存每个角色读到和写入的 ID；用 `memory get --id <id>` 可查原文、来源角色、Run 和提交。用 `memory add ... --replaces <id>` 修订当前范围内的记录，被替代版本仍可按 ID 查询。

记忆保存在 Git 公共目录的 `aep/memory.sqlite3`，同一克隆的 Worktree 共用，独立克隆各自保存。自动传递需显式开启，启用后所选文本会作为提示词发送给该角色所用的模型。每条摘要最多 1,000 个字符，每次上下文最多 12 条、8 KiB，优先选本轮交接和当前候选记录。完整日志、原始会话与详细审查报告保存在本次 Run 的本地证据目录。Agent 留言不代表人工批准。

## 试点边界

- Git 远端固定 Spec，Studio 管理本机接力和队列。也可启动 Rust 控制面和独立 Worker，在一个仓库中向独立 Worker 派发和恢复任务。HTTPS 控制面支持多 Worker 并行。Worker 主动连接，控制面持久化 Lease、执行事件和批准记录。[控制面启动与试点手册](docs/pilot.md)
- 只在受信任的仓库运行。Worktree 隔开文件变更，测试命令和待测代码会以本机用户身份执行。选择外部模型客户端前，须确认其环境与数据权限。
- 同一任务可以有多个 Run。每次都针对固定版本生成证据。用户可以在 Studio 核对证据并确认发布候选，CI 在 PR 当前提交上独立复验。
- 本仓库用作流程试点与基线。业务收益、费用和可靠性由后续真实需求测量。[AEP 架构提案与八周路线图](https://app.notion.com/p/41a9d9f6f60581d08560fa33ed8c4810)

## 开发验证

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked --all-targets
```

[试点 Spec 与六条验收用例](specs/market-volume-dedup-rust/spec.md) · [任务清单与验收命令](specs/market-volume-dedup-rust/task.json)

### 离屏调试桌面界面

在 macOS 的仓库根目录运行以下命令，即可用生产版 Studio 视图验证记忆修订、并发冲突和 Run 与记忆的双向跳转。每个场景自建临时 Git 仓库，用 GPUI 模拟点击和键盘输入。测试使用本地 SQLite 和合成的运行证据，无需桌面控制权限或模型账号。

```sh
cargo run --locked --manifest-path desktop/Cargo.toml --example ui_debug
```

可用 `--scenario history`、`conflict`、`handoff` 或 `studio-relay` 单独运行。后者从 Studio 选任务发起两轮，并回归失败重试、记忆修订、证据跳转、PR 发布恢复、GitHub CI、人审合并和合并后收尾。要查看某一步的实际画面，添加输出目录和截图选项。

```sh
cargo run --locked --manifest-path desktop/Cargo.toml --example ui_debug -- \
  --scenario conflict --output-dir desktop/target/ui-debug --screenshots
```

`handoff` 会在临时仓库中通过 AEP CLI 派发 Rust 小任务，由两个本地替身进程完成实现和审查。审查进程只有在收到实现 Agent 的交接标识后才会返回报告。UI 随后打开执行器生成的 SQLite 记忆、验收日志和审查报告。调试流程还会在 Studio 将第一轮的未决问题修订为决策，再运行同一 Spec 的新候选提交。第二轮的两个 Agent 必须收到新决策。每个版本的读取记录都可以跳回对应 Run。

要用真实的 Codex 和 Claude 复跑两轮，在已登录两个客户端且支持 Metal 的 Mac 上运行以下命令。命令会使用模型账户额度。可以省略 `--codex-model` 来使用本机 Codex 配置的默认模型。

```sh
cargo run --locked --manifest-path desktop/Cargo.toml --example ui_debug -- \
  --live-relay --codex-model gpt-6.1-sol --output-dir desktop/target/live-relay
```

每次运行会在指定目录下创建新的 `live-relay-*` 子目录。Studio 将第一轮的问题修订为带唯一标识的决策。命令会核对第二轮实现和审查的输入 ID、两个客户端对该标识的确认，以及两个版本的 Studio 读取记录。成功结果包括 `verification.json`、两轮 Run 的证据和报告、`studio-revision.png` 及新旧版本的截图。失败会返回非零退出码，保留已有证据与截图，并将原因写入 `failure.txt`。

结果目录里的 `workspace/repo` 保存了合成 Git 仓库、两个候选提交、SQLite 记忆和原始 Run 证据。可在 Studio 点「打开仓库」选择这个目录，继续查看两轮证据。终端会打印完整路径，也可传给以下命令。

```sh
cargo run --locked --manifest-path desktop/Cargo.toml -- \
  --repo /path/to/live-relay-result/workspace/repo
```

已在本机用模型客户端跑完一次交接时，可以验证那次运行的界面。将 `--run-id` 改为实际运行 ID。

```sh
cargo run --locked --manifest-path desktop/Cargo.toml --example ui_debug -- \
  --repo /path/to/repo --run-id market-handoff-001 --output-dir desktop/target/ui-debug
```

每个检查点输出一份 JSON，包含控件的 ID、标签、值、可见性和窗口坐标。`--screenshots` 使用离屏渲染器生成同名 PNG，需要可用的 Metal 设备。断言失败时命令返回非零退出码。指定输出目录后会尝试写入 `failure.json` 和所选截图。

新交互可以用 JSON 步骤回放。下面的示例从界面创建一条记忆，然后验证搜索结果会随输入变化。

```sh
cargo run --locked --manifest-path desktop/Cargo.toml --example ui_debug -- \
  --script desktop/examples/memory-search.json --output-dir desktop/target/ui-debug
```

脚本从包含一条 `sample-run` 的新仓库开始。初始状态存为 `script-000.json`，之后每步递增。使用快照中的唯一 ID 做 `click`、`present`、`absent`，或用唯一标签做 `click_label`。此外支持 `input`、`press`、`scroll`（`id` 和像素增量 `dy`）和 `expect_value`（`label` 和 `equals`）。macOS CI 会运行四个内置场景和仓库内的搜索脚本。

### 用真实客户端验证 Studio 到 GitHub 的完整流程

在 Studio 进入「隔离全流程」，选择仓库、已派发任务和 GitHub 目标分支，再点「启动真实任务」。Studio 固定任务与目标提交，在独立克隆运行。Spec 必须基于所选目标提交，预置文件限于自己的 `specs/<task>/` 目录。派发分支或目标版本变动时，按界面提示刷新或从新目标重新派发。

准备完成后，发起第一轮，修订任务记忆并继续第二轮。最终审查页锁定所选目标，仍需在 Studio 分别确认发布与合并。合并后点「同步目标分支并清理」，Studio 会同步会话内的本地目标、清理候选并归档证据。真实任务保留派发引用。在结果页可直接看日志和截图，在 Finder 定位归档，或回到原仓库重新打开历史会话。

### 恢复仓库任务

「隔离全流程」顶部的「仓库任务记录」按所打开的本地 Git 仓库显示任务、目标分支、进度和最近的失败原因。重开 Studio 会载入上次打开的仓库。点「继续任务」会根据已有凭据直接打开待派发、接力、人工审查、GitHub PR 或收尾页面。打开 PR 会重新读取 GitHub 状态，发布和合并仍需当场确认。

关闭 Studio 后，已经启动的本机执行器可以继续运行。运行失败时可在接力页重试该轮，重试会保留原尝试的证据并新建执行记录。已发布的候选可在 PR 页重试归属匹配的失败 GitHub Actions 检查。已完成的任务可用「查看归档」读取日志、截图和核验结果。

### 执行队列与并发

「隔离全流程」的执行队列按仓库保存，默认同时执行一轮，可设为 1 至 4 轮。第一轮、带已确认决策的第二轮和失败重试都进入队列。等待人工修订、PR 确认或 GitHub CI 的任务会释放本机执行名额。点「暂停派发」可暂缓启动等待项，已经启动的轮次会继续。用等待项的「前移」「后移」调整「恢复派发」后的启动顺序。Studio 重开后会恢复暂停状态、并发上限和排队顺序。

任务记录显示排队位置和运行阶段。确认取消等待项后，Studio 会保存取消记录并将它移出待派发顺序。确认取消运行项后，Studio 会停止该轮本机执行，保留已有日志、共享记忆及可恢复的工作区。停止完成后会释放并发名额。重开 Studio 后，可从任务记录查看取消状态，在任务详情归档取消证据，或显式发起新一轮执行。每个任务使用自己的克隆、共享记忆库和证据目录，归档收录该任务自己的排队和执行记录。

点「启动合成验证」可按原流程新建测试任务和隔离目标分支。

Studio 截图只渲染自己的窗口画面，使用 Metal。此流程使用已登录的 Codex、Claude 和 GitHub CLI 账号。通过 Finder 启动 Studio 时，客户端也可放在常用的 `~/.local/bin`、`~/.cargo/bin` 或 Homebrew 的安装目录。

也可在支持 Metal 的 macOS 终端、仓库根目录运行合成验证。需要 Rust、Git、已登录的 Codex CLI 和 Claude Code，以及对 `origin` 有推送、创建和合并 PR 权限的 GitHub CLI 账号。合成验证需要 AEP pilot 工作流。真实任务会核验所选仓库的 PR 检查，配置了 AEP pilot 工作流时还会核验独立验收。

```sh
cargo run --locked --manifest-path desktop/Cargo.toml --example ui_debug -- --live-lifecycle
```

命令为每次测试创建独立克隆、合成 Rust 任务和唯一的 `test/studio-lifecycle-*` 目标分支。Studio UI Debug 通过生产界面的控件选任务、发起两轮、保存修订并执行发布和合并。流程会在三个操作人步骤暂停。

1. 第一轮将未决问题交给操作人。输入舍入规则，或按 Enter 接受示例。命令将决策填入 Studio 的修订表单并保存，然后发起第二轮。
2. 查看终端给出的最终 Diff、验收日志、Claude 报告和 Studio 截图。输入 `publish` 后，命令操作 Studio 的发布确认，向该次隔离目标创建 GitHub PR。
3. 等同一候选 SHA 的 GitHub 检查和独立验收通过。核对 Studio 的检查和审查页截图后输入 `merge`，命令操作 Studio 的合并确认，再点击收尾。

成功时会核对第二轮两个真实 Agent 的决策输入 ID 和各自输出中的唯一标识、GitHub 合并提交、目标分支同步、候选分支与临时 Worktree 的清理，并逐字节比较收尾前后的两轮证据、共享记忆及确认凭据。隔离目标会留在 GitHub 供检查，该次任务派发引用会在收尾后回收。

终端会输出结果目录。默认在 `desktop/target/live-lifecycle`，也可加 `--output-dir <路径>` 和 `--codex-model <模型名>`。打开结果目录内的 `evidence/index.md` 可查看各步 PNG、控件快照和日志。`evidence.tar.gz` 保存同一批证据，`workspace/repo` 可用 Studio 重新打开。运行失败或拒绝确认会返回非零退出码，保留已有证据、失败原因和测试引用以便排查。
