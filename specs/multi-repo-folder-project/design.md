# 技术方案：项目 · 从多仓库文件夹创建项目

依据：`spec.md`（R1–R18、E1–E16、AC-1–AC-12）、`interaction.md`（P1–P7、§2–§8）。
仓库边界：只改本仓库的 `src/`、`tests/`、`studio/`。`desktop/`（GPUI）不改，不新增依赖。

## 1. 现状要点（决定方案的事实）

| # | 事实 | 位置 | 对方案的影响 |
|---|---|---|---|
| F1 | `Library::change` 先加锁、重新读取，闭包返回 Err 时不写 `projects.json` | `src/project.rs:991-1011` | 先校验全部仓库，再在一次 `change` 里写入，就能做到整体成功或整体失败（interaction §3.6、§4.2），不需要回滚逻辑 |
| F2 | `Project` 只在 `Library::create` 里有一处结构体字面量 | `src/project.rs:970` | 新字段加 `#[serde(default)]`，只需改这一处 |
| F3 | Studio 的 `repository()` 用 `.context("不是 Git 仓库")` 包住 git 原始报错 | `studio/src-tauri/src/api.rs:266` | 改为调用友好描述，界面不再出现 `fatal` |
| F4 | 阶段 prompt 由 `handoff::packet` 拼接，`since` 有值时提前返回（续接） | `src/handoff.rs:242-283` | 工作区说明行放在 `since == None` 的路径里，天然满足「续接不重复」（R14） |
| F5 | `Request` 由 `Work::request` 生成，然后在 `Project::start`（handoff.rs:439-451）补上 `upstream`；CLI `context` 预览在 `project_cli.rs:1316` 也调用 `w.request` | `src/handoff.rs`、`src/project_cli.rs` | 新增 `Request.workspace_root`，和 `upstream` 在同一位置赋值，旧的 request.json 默认为 None |
| F6 | Claude 阶段的 cwd 是 `<catalog>/worktrees/...`，只有执行目录和 receipt 目录被 `--add-dir`，runs 目录用 `--allowedTools Read(//<runs>/**)` 授权读取（default-sandbox-write-check T2 已实测有效，不需要 add-dir） | `src/workflow_stage.rs:1215-1232` | 工作区说明文件也用同样的方式，对每个文件授权 `Read(//<file>)`，只读，并且只授权这些文件（spec Q5） |
| F7 | Codex 的 `read-only` 和 `workspace-write` 沙箱都只限制写，不限制读 | `src/workflow_stage.rs:1309-1345` | Codex 不需要改动 |
| F8 | ZCode 通过 app-server 以 cwd 为 workspace 运行。prompt 里已经引用了 cwd 以外的执行目录，平台没有为它做额外授权；遇到 `permission.requested` 时阶段失败，并给出明确报错 | `src/zcode_stage.rs:96,140` | ZCode 与执行目录的处理方式一致，不做额外配置。测试验证阶段如有 ZCode 环境，就实测一次；没有就记录为已知限制 |
| F9 | `pick_folder` 用 `rev-parse --show-toplevel` 把子文件夹解析成仓库根，非仓库时原样返回 | `api.rs:316-338` | 不改（spec 不包含项） |

## 2. 对待确认项和数值的决定

| 项 | 决定 |
|---|---|
| 默认深度 / 上限 | 默认 2 层，取值 1–4。Studio 的「再查一层」每次加 1，最多到 4。CLI 用 `--depth N`，由 clap 校验范围，超出时报「--depth 只能是 1 到 4」 |
| 扫描上限（E7） | 最多访问 5000 个目录（不含被跳过的目录）。达到上限就停止，结果里 `truncated = true`。不设耗时上限：目录数上限已经能约束耗时，测试也更确定 |
| 读取授权（Q5） | 只授权顶层存在的 `AGENTS.md`、`CLAUDE.md` 两个文件，不授权整个工作区根 |
| 多个工作区根（Q3） | 一个项目只有一个 `workspace_root`。已有时不覆盖，接口返回 `kept_root`，界面和 CLI 据此给出说明 |
| 导入扫描并发 | 前端最多同时 3 个 `importScan` |
| CLI 不带 `--all` 的列表输出 | 作为错误输出：stderr，退出码 1，沿用 CLI 现有「错误写 stderr」的习惯（interaction §6 的「错误按现有习惯写到 stderr」）。文字与 interaction §6 一致 |

## 3. 数据

### 3.1 `Project` 新字段（R9，AC-11）

```rust
/// Folder holding several of this project's repositories (not itself a repository).
/// Its top-level AGENTS.md / CLAUDE.md are pointed out to every stage.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub workspace_root: Option<PathBuf>,
```

- 存储规范化后的绝对路径（`canonicalize`）。
- 旧数据没有这个字段时为 None，序列化时也不写出（AC-11）。

### 3.2 `Request` 新字段（R12）

```rust
/// The project's workspace folder; its top-level notes are listed in the prompt.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub workspace_root: Option<PathBuf>,
```

- 在 `Project::start` 的 `request.upstream = upstream;` 旁边赋值为 `self.workspace_root.clone()`。
- 在 CLI `Action::Context`（预览）的 `w.request(...)` 之后同样赋值。
- 旧的 request.json 反序列化后为 None，行为不变。

## 4. 新模块：`src/project/folder.rs`

在 `src/project.rs` 中增加 `pub mod folder;`。Studio 通过 `aep::project::folder` 使用。

```rust
pub const DEFAULT_DEPTH: u8 = 2;
pub const MAX_DEPTH: u8 = 4;
pub const MAX_DIRS: usize = 5000;
pub const NOTES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Found { pub path: PathBuf, pub relative: String, pub name: String }

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Resolved {
    /// The path is (inside) a git repository: today's behaviour.
    Repository { root: PathBuf },
    /// Not a repository, but repositories were found below it.
    Folder { root: PathBuf, depth: u8, repositories: Vec<Found>, truncated: bool, notes: Vec<String> },
    /// Not a repository and none below it (within `depth`).
    Empty { root: PathBuf, depth: u8, truncated: bool },
    Missing { path: PathBuf },
    NotDir { path: PathBuf },
}

pub fn resolve(path: &Path, depth: u8) -> Result<Resolved>;
pub fn notes(root: &Path) -> Vec<PathBuf>;          // existing top-level AGENTS.md / CLAUDE.md, in NOTES order
pub fn message(r: &Resolved) -> String;               // the fixed texts of interaction §2.1 / spec R3
pub fn unreadable(path: &Path) -> anyhow::Error;      // friendly error for a path that should have been a repo
```

### 4.1 `resolve` 算法

1. 对 `path` 做 `trim`，然后检查：
   - 不存在 → `Missing`；
   - 不是目录 → `NotDir`；
   - `depth` 不在 1..=4 → Err。
2. `Workspace::open(path)` 成功 → `Repository { root: ws.root.canonicalize() }`。这对应 R1、E1、E2，与现在的行为一致。
3. 否则 `root = path.canonicalize()`，用广度优先遍历，队列元素为 `(dir, level)`，`visited: HashSet<PathBuf>` 记录规范化后的路径：
   - 用 `read_dir` 读取目录，读失败（无权限等）就跳过（E7）。条目按文件名排序，保证结果稳定。
   - 只处理目录，指向目录的符号链接也算（E5），跟随前先 `canonicalize`。已经在 `visited` 里的跳过，这同时解决了链接循环和重复。
   - 名字为 `node_modules`、`target` 或以 `.` 开头的跳过。这里按条目自己的名字判断，不看规范化后的名字。
   - 每访问一个目录计数一次，达到 `MAX_DIRS` 就停止，并设置 `truncated = true`。
   - **判定仓库**：`dir/.git` 存在（目录或文件都算），并且 `Workspace::open(dir)` 成功，并且 `ws.root.canonicalize() == canonical(dir)`。满足时记为 `Found`，不再进入它的内部（R2、E3）。
     - 裸仓库没有 `.git`，不会被判定为仓库（E4）。
     - worktree 的 `.git` 是文件，`rev-parse` 能成功，所以会被列出（E3）。
     - `.git` 存在但 `rev-parse` 失败（损坏的仓库），静默跳过。
   - 不是仓库且 `level < depth` 时，把它的子目录加入队列。
4. `relative` 是相对 `root` 的路径，用 `/` 连接。如果仓库本身通过符号链接到达、真实路径在 root 之外，`relative` 用链接所在的位置，`path` 用真实路径。`name` 取 `path` 的最后一段。结果按 `relative` 排序，并按 `path` 去重，保留第一次出现的。
5. 有仓库 → `Folder { notes: notes(&root) 的文件名 }`；没有 → `Empty`。

### 4.2 `message`（interaction §2.1 的固定文字）

| 结果 | 文字 |
|---|---|
| `Empty` | 「这个文件夹和它的子目录里没有 git 仓库（已查找 {depth} 层）」，`truncated` 时追加「；文件夹太多，结果可能不完整」 |
| `Missing` | 「路径不存在：{path}」 |
| `NotDir` | 「不是文件夹：{path}」 |
| `Folder` | 「{path} 不是 git 仓库，在它下面找到 {n} 个仓库（查找 {depth} 层）」 |

CLI 和 Studio 后端都用这些文字。前端自己有一份同样的格式函数，供 mock 使用，文字必须一致。

### 4.3 `unreadable(path)`

给 Studio `repository()` 和 CLI `root()` 在 `Workspace::open` 失败时使用。它调用 `resolve(path, DEFAULT_DEPTH)`：

- 结果是 `Folder`：返回「{message}；请在清单中选择要添加的仓库」（CLI 中改为「加 --all …」，见 §6）；
- 其他结果：返回 `message`；
- `resolve` 本身失败：返回「无法读取这个文件夹：{err}」。

所有情况都不包含 git 原始输出（AC-3）。

## 5. 阶段 prompt 与读取授权（R12–R14，AC-7、AC-8）

### 5.1 prompt 行

在 `handoff.rs` 中新增：

```rust
pub fn workspace_line(req: &Request) -> String
```

- `req.workspace_root` 为 None，或 `folder::notes(root)` 为空（E11、E12）时，返回空字符串。
- 否则返回：
  `\n项目工作区说明：开始前先读 {绝对路径1}、{绝对路径2}（工作区 {root} 顶层的团队约定，适用于本项目的所有仓库；只读，不要修改）。`
- 文件是否存在在运行时检查，所以文件被删除后这一行自动消失。

`packet` 在 `since == None` 的路径里，于 `memory_body` 之后、`rules` 之前追加这一行。它不占用记忆额度（R14）。`since` 有值时 `packet` 已经提前返回，不会追加（F4）。

### 5.2 Claude 读取授权

`claude_stage_args` 增加一个参数 `notes: &[PathBuf]`。对每个文件追加：

```
--allowedTools Read(/{file})
```

文件是绝对路径，前面再加一个 `/`，就成为 Claude 绝对路径规则的 `//` 形式，与 runs 目录的写法相同。不加 `--add-dir`，所以没有写权限。

调用处 `run`（`workflow_stage.rs:1348`）传入 `req.workspace_root.as_deref().map(folder::notes).unwrap_or_default()`。

### 5.3 Codex / ZCode

- Codex：不改（F7）。
- ZCode：不改（F8）。测试验证阶段有 ZCode 环境时实测一次，没有时在报告里写明是已知限制。

## 6. CLI（R15–R17，AC-9）

`Action::Create` 和 `Action::AddRepo` 都增加两个参数：

```rust
#[arg(long)] all: bool,
#[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u8).range(1..=4))] depth: u8,
```

新增一个辅助函数：

```rust
fn expand(path: &Path, all: bool, depth: u8) -> Result<(Vec<PathBuf>, Option<PathBuf>)>
```

| `resolve` 结果 | 行为 |
|---|---|
| `Repository` | 返回 `(vec![root], None)`，`--all` 不起作用（R17） |
| `Folder` 且 `all` | 返回 `(全部仓库, Some(root))` |
| `Folder` 且不带 `all` | `bail!`，内容为 `message` + 每行「  {name 左对齐}  {path}」+ 提示「加 --all 全部添加，或用 --repository 逐个指定；更深的仓库用 --depth N。」（add-repo 时提示改为「逐个 add-repo」） |
| `Empty`、`Missing`、`NotDir` | `bail!(message)` |

- **create**：
  - 合并各个 `--repository` 展开后的结果，按顺序去重；
  - `workspace_root` 取第一个 `Some`；
  - 在一次 `change` 里完成 `l.create(...)`，然后设置 `project_mut()?.workspace_root`；
  - 输出 `{"project": id}`；有工作区时追加 `"workspace_root"` 和 `"repositories"` 两个键（新增键不影响现有调用方）。
- **add-repo**：
  - `Repository` 的情况：逻辑和输出都完全保持现状（打印路径）；
  - 工作区的情况：在一次 `edit_project` 里分出 `added` 和 `existing`。选中了事项时，对每个仓库调用 `work.include`（R16）。原来没有工作区根时写入；已有不同的工作区根时保留原值，并设置 `note`。输出 `{"added","existing","workspace_root","note"?}`。
- `root()` 失败时改用 `folder::unreadable`，`link-ticket`、`remove-repo` 等路径因此也不再显示 git 原始报错。

## 7. Studio 后端（`studio/src-tauri`）

`api.rs`：

- 新增 `resolve_folder(path: &str, depth: Option<u8>) -> Result<Value>`：调用 `folder::resolve`，结果序列化后带上 `"message": folder::message(&r)`（`Repository` 时为空）。
- 修改 `repository(path)`：`Workspace::open` 失败时返回 `folder::unreadable(path)`（F3）。
- 修改 `create_project(home, name, repositories, workspace_root: Option<&str>)`：
  1. 先把所有仓库映射成 `repository(r)`，任何一个失败就整体返回错误。错误信息里带上仓库目录名，例如「genki-kline：<message>」，满足 E14。
  2. 如果有 `workspace_root`，要求它 `canonicalize` 成功并且是目录，否则报错。
  3. 在同一个 `change` 闭包里调用 `lib.create`，并设置 `workspace_root`。
- 新增 `add_repositories(home, pid, paths: &[String], workspace_root: Option<&str>) -> Result<Value>`：
  1. 先全部校验；
  2. 在一次 `change` 里跳过已关联的仓库，追加其余仓库，按 Q3 规则设置 `workspace_root`；
  3. 返回 `{"added": [...], "existing": [...], "workspace_root": ..., "kept_root": <原根，仅在没有覆盖时>}`。
- `add_repository` 保留，单仓库添加的路径不变。
- snapshot 的项目 JSON 增加 `"workspace": null | {"root", "exists", "notes": ["AGENTS.md", ...]}`。

`lib.rs`：

- 新增 Tauri 命令 `resolveFolder(path, depth: Option<u8>)` 和 `addRepositories(project, paths, workspaceRoot: Option<String>)`；
- `createProject` 增加 `workspaceRoot: Option<String>`，Tauri 的可选参数，旧的调用方不传也兼容；
- 全部注册到 `generate_handler!`。

## 8. Studio 前端（`studio/src`）

- **`api/types.ts`**：
  - 新增 `FolderResolved` 联合类型：`kind: "repository" | "folder" | "empty" | "missing" | "not_dir"`，带 `root`、`path`、`depth`、`repositories: {path, relative, name}[]`、`truncated`、`notes`、`message`；
  - `ProjectSummary.workspace?: {root, exists, notes} | null`；
  - `createProject` 增加 `workspaceRoot?`；
  - 新增 `resolveFolder` 和 `addRepositories` 的签名。
- **`api/client.ts`**：增加对应的 invoke 调用。
- **`api/mock.ts`**：
  - 按 interaction §8 实现：`/mock/workspace` 或路径中包含 `03-行情基础数据计算存储` 时返回 11 个仓库，其中 `services/quote-data-service` 在第 2 层，`notes` 为 `["AGENTS.md"]`；
  - `/mock/empty` 无论哪个深度都返回 `empty`；
  - `/mock/missing` 返回 `missing`；
  - 其他路径返回 `repository`；
  - `createProject` 和 `addRepositories` 记录 `workspace`，`addRepositories` 返回 `existing` 和 `kept_root`；
  - 内置一个项目包含该工作区中的 2 个仓库，供 AC-5 使用。
- **新组件 `components/FolderScan.tsx`**：负责勾选清单，包括清单头、行、脚注和「再查一层」（interaction §2.2–§2.4）。
  - Props：`resolved`、`selected: Set<string>`、`onChange`、`added?: Set<string>`（已添加的仓库）、`onDeeper()`、`deepening`。
  - 「再查一层」后保留已取消的勾选：组件外部记录被取消的绝对路径集合，新出现的仓库默认勾选。
  - `data-testid`：`folder-scan`、`folder-scan-row`、`folder-scan-count`。
- **`Dialogs.tsx` / `NewProjectDialog`**：
  - 每行路径先调用 `resolveFolder`，结果缓存在 `resolved[path]` 里：
    - `repository`：沿用现有导入扫描；
    - `folder`：显示 `FolderScan`；
    - 其他结果：显示错误行，带「移除这一行」。
  - 待创建的仓库集合 = 普通仓库 ∪ 各工作区中勾选的仓库，去重。`workspaceRoot` 取第一个工作区行。
  - 导入区按 interaction §3.4 处理：并发 3、折叠显示、汇总一行；
  - 按钮状态按 interaction §3.5；
  - 创建失败时，对工作区行重新 resolve。
- **`SecondaryPages.tsx` / `ReposPage`**：
  - 「添加仓库」先调用 `resolveFolder`：`repository` 走现有的 `addRepository`；`folder` 时在卡片内显示 `FolderScan`（`added` = 项目已有仓库），确认后调用 `addRepositories`，toast 按 interaction §4.2；
  - 新增工作区卡片（`data-testid="workspace-root"`），包括「重新扫描」和「在访达中显示」；
  - 两处 `pickFolder` 的提示语按 interaction §3.1 改写。
- **`styles/app.css`**：增加清单的样式（最多显示 8 行，超出部分滚动；已添加的行显示为灰色）。

## 9. 兼容性

- 旧的 `projects.json`、旧的 request.json 都能照常读取，新字段为空时不写出（AC-11）。
- 普通仓库的新建、添加、CLI 输出和行为都不变（AC-4、R17）。现有测试不需要修改断言，唯一例外是 `claude_stage_args` 的单元测试要传入新参数。
- `createProject` 的新参数是可选的，旧的前端调用仍然有效。
- 阶段 prompt 只有在项目有工作区根并且存在说明文件时才会变化。所以 stage-token-diet 的基线测试（`tests/stage_token_diet.rs`）用的项目没有工作区根，不受影响。

## 10. 测试计划

| 层 | 测试 | 覆盖 |
|---|---|---|
| `src/project/folder.rs` 单元测试（tempdir + `git init`） | 第 1、2、3 层仓库（只列出前两层，`depth=3` 时列出第 3 层）；`node_modules`、`target`、`.hidden` 下的仓库不列出；仓库内部的嵌套仓库不列出；worktree 列出；裸仓库不列出；两个符号链接指向同一仓库时只列出一次；符号链接循环能正常结束；`chmod 000` 的子目录被跳过且不报错；`MAX_DIRS` 截断（测试中用可注入的上限参数，例如内部函数 `scan(root, depth, max)`）；路径是仓库或仓库的子目录时返回 `Repository`；`Missing` 和 `NotDir`；`notes` 的顺序和存在性；`message` 的文字 | AC-6、R1–R4、E1–E7 |
| `src/handoff.rs` 单元测试 | 有工作区根且有 AGENTS.md 时，`packet` 包含绝对路径并且不包含文件内容；`since=Some` 时不包含；文件删除后不包含；没有工作区根时 prompt 与改动前逐字节相同 | AC-7、E11、E12、R14 |
| `src/workflow_stage.rs` 单元测试 | `claude_stage_args` 对每个说明文件输出 `Read(//…/AGENTS.md)`（路径含空格和中文）；没有说明文件时参数与现在相同；不会因此增加 `--add-dir` | R13 |
| `src/project.rs` 单元测试 | 没有 `workspace_root` 的旧项目 JSON 往返后不出现这个字段 | AC-11 |
| `tests/workspace_folder.rs`（新建，CLI 集成） | create 不带 `--all`：退出码 1，stderr 列出全部仓库，项目没有创建；带 `--all`：全部仓库加入且记录了 `workspace_root`；add-repo 不带 / 带 `--all`，`existing` 不重复，选中事项时同时加入事项；空文件夹；普通仓库加 `--all` 时输出与现在相同；`--depth 5` 报错；Q3：第二个工作区不覆盖原值 | AC-9、R15–R17 |
| `studio/src-tauri/tests/api.rs` | `resolve_folder` 对工作区、空文件夹、不存在路径的结果；`create_project` 带工作区根并勾选部分仓库；某个仓库在确认前被删除时整体失败，`projects.json` 不变，错误中点名该仓库；`add_repositories` 的 `existing` 和 `kept_root`；snapshot 中的 `workspace` | AC-1、AC-2、AC-3、AC-5、E14 |
| 回归（`studio/src-tauri/tests/api.rs`，沿用 `fake_claude`） | 用工作区根和其中 2 个仓库建项目，新建同时包含这 2 个仓库的事项，然后推进一个阶段。断言两个仓库各自有 `aep-stage/<id>` 分支和工作区；工作区根下没有新目录，也不是 git 仓库；fake claude 收到的 prompt 中有工作区说明行 | AC-10、R18、AC-7 |
| `studio/tests/workspace-folder.spec.ts`（新建，Playwright + mock） | AC-1（11 行全部勾选，按钮显示 11，创建后显示工作区卡片）；AC-2（取消 2 个后显示 9，全不选时禁用）；AC-3（空文件夹和不存在路径的文字，页面不包含 `fatal`）；AC-4（普通仓库不显示清单）；AC-5（2 个「已添加」，「添加所选（9）」）；「再查一层」；每个用例截图 | AC-1–AC-5、AC-12 |
| 真实执行（测试验证阶段） | 用一个工作区根下有 AGENTS.md 的项目，以 Standard 沙箱运行一个 Claude 阶段：stream-json 中有成功读取该文件的 Read 调用，`permission_denials` 中没有它，工作区根没有被写入；记录 run_id。另外在 Studio 中用真实目录 `03-行情基础数据计算存储` 走一遍 AC-1 | AC-8、AC-1 |

整体命令：

- 仓库根目录：`cargo test --locked`；
- `cd studio/src-tauri && cargo test`；
- `cd studio && npm run build && npx playwright test`。

## 11. 风险

- **R-a**：如果用户的上级目录（例如 `~`）本身是 git 仓库，选择工作区文件夹会被解析为外层仓库（E1，与现状一致）。本事项不处理，只在验证报告中提一句。
- **R-b**：Claude 对单个文件的 `Read(//file)` 规则如果在某个版本上不生效，退路是授权 `Read(//<root>/*.md)`，只覆盖顶层的 md 文件。开发阶段以 T2 的参数测试为准，实际效果由 AC-8 的真实执行确认。
- **R-c**：一次导入扫描 11 个仓库时，`importScan` 较慢（每个仓库都要读取 Claude、Codex、ZCode 的记录）。并发 3 加上进度显示可以缓解，不阻塞创建以外的操作。
