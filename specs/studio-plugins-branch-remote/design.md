# 技术方案：Studio · 插件菜单与分支管理插件化

依据：`spec.md`（S1–S9、FR-1–18、E1–14、AC-1–17）、`interaction.md`（§2 统一文本、§3 插件页、§4 分支页、§6 mock、§7 数据需求）。只读联动；不 push，也不创建、修改或合并 PR/MR。

## 0. 仓库边界

| 区域 | 改动 |
|---|---|
| `aep` 库（`src/`） | 新增 `src/project/remote.rs`（git 层：remote 解析、上游比较、仅远端分支、fetch）、`src/project/forge.rs`（GitHub/GitLab 只读 API，经系统 `curl`）、`src/project/secret.rs`（令牌存放：macOS 钥匙串 / 内存）。`src/project.rs` 加三行 `pub mod`。 |
| Studio 后端（`studio/src-tauri/`） | 新增 `src/plugins.rs`（插件注册、持久化、徽标、远端状态与后台刷新、`open_url`）；`src/lib.rs` 注册命令。`api.rs` 不改（`git_tools` / `open_git_tool` / `branch_graph` 原样）。 |
| Studio 前端（`studio/src/`） | `api/types.ts`、`api/mock.ts`、`state/store.tsx`、`App.tsx`、`components/Sidebar.tsx`、`components/Branches.tsx`、新增 `components/Plugins.tsx`、`lib/remote.ts`、`styles/app.css`。 |
| 测试 | 新增 `tests/studio_plugins_remote.rs`（根 crate）、`studio/src-tauri/tests/plugins.rs`；`studio/tests/ui.spec.ts` 增加用例。 |
| 不改 | `desktop/`（GPUI）、`src/project/branches.rs`、`src/project/merge.rs`、工作流和交付逻辑。 |

**不新增 crate 依赖**。`studio/src-tauri` 用 `default-features = false` 依赖 `aep`，不带 reqwest；Studio 锁文件里的 reqwest 0.13 也没有 TLS。如果加 TLS 依赖，需要更新两份 `Cargo.lock`，开发时还要联网，而 `--locked` 构建对此很敏感。所以：
- HTTP 调用系统 `/usr/bin/curl`。它走 macOS 系统信任库，用企业 CA 的 GitHub Enterprise 也能正常校验。
- 钥匙串调用系统 `/usr/bin/security`。
- 两者都只通过 **stdin** 传令牌，令牌不出现在命令行参数里，`ps` 看不到。

## 1. 对 PRD 待确认问题的取舍

| 问题 | 结论 |
|---|---|
| Q1 只读 | 是。`forge.rs` 只实现 GET；`remote.rs` 只有 `fetch`。AC-17 用 §7.4 的 grep 检查。 |
| Q2 自动 fetch | 否。打开分支页只读本地引用和缓存；只有点「刷新远端」才 fetch。 |
| Q3 登录方式 | 只用个人访问令牌。 |
| Q4 自签名证书 | 不提供跳过选项。curl 证书类退出码 → `cert`。 |
| Q5 fetch 凭据 | 用系统 git 凭据。设 `GIT_TERMINAL_PROMPT=0`，stdin 置空，不改 git 配置，不把令牌交给 git。 |
| Q6 全局/项目 | 全局：`<catalog>/plugins/plugins.json`。 |
| Q7 git-tools | 不迁移。`<catalog>/plugins/git-tools.json` 与 `api::tool_list` 保持原样，分支管理配置区直接调用现有 `gitTools()` 展示（自定义项按 `id` 前缀 `custom:` 标记）。 |

## 2. 数据与存储

### 2.1 `<catalog>/plugins/plugins.json`

用 `aep::project::save_json` 原子写入；文件不存在时取默认值。

```json
{
  "version": 1,
  "branches": { "enabled": true },
  "github":   { "enabled": false, "host": "https://github.com", "token_hint": "a1b2",
                "error": { "kind": "auth", "message": "…", "at": 1764000000 } },
  "gitlab":   { "enabled": false, "host": "https://gitlab.com", "token_hint": null, "error": null }
}
```

- `token_hint`：保存时取令牌末 4 位，**只存这 4 位**，供界面显示「••••a1b2」时不用每次读钥匙串。完整令牌不落盘（FR-7，AC-5）。
- `error`：只记 `auth` / `forbidden` / `cert`（interaction §2.1）。用**已保存**的令牌测试连接或刷新远端时写入；同类调用成功时清除。
- 文件损坏（JSON 解析失败）：`plugins()` 返回错误（界面 §3.2 失败态），**不覆盖**原文件。之后在界面上切换开关时，按默认值加这次修改写回，并把旧文件另存为 `plugins.json.bad`。

### 2.2 令牌（`src/project/secret.rs`）

```rust
pub trait Secrets: Send + Sync {
    fn get(&self, plugin: &str) -> Result<Option<String>>;
    fn set(&self, plugin: &str, token: &str) -> Result<()>;
    fn delete(&self, plugin: &str) -> Result<()>;
}
pub struct Keychain { pub service: String }        // 默认 "AEP Studio plugin token"
pub struct Memory(std::sync::Mutex<HashMap<String, String>>); // 测试与非 macOS
pub fn valid_token(t: &str) -> bool;                // ^[A-Za-z0-9_.\-]{8,512}$
```

- Keychain 的账号名是插件 id（`github` / `gitlab`）。主机换了令牌不跟着变，界面的「更换」负责（interaction §3.3）。
- `set`：`security -i`，stdin 写一行 `add-generic-password -U -s "<service>" -a "<plugin>" -w "<token>"`。`valid_token` 先挡掉引号、空白等字符，不需要转义；GitHub 的 `ghp_` / `github_pat_` 和 GitLab 的 `glpat-` 都符合。
- `get`：`security find-generic-password -s <service> -a <plugin> -w`，令牌从 stdout 读出；退出码 44 表示没有，返回 `None`。
- `delete`：`security delete-generic-password -s … -a …`；退出码 44 视为成功。
- 命令行参数由纯函数 `keychain_argv(op, …) -> (Vec<String>, Option<String> /*stdin*/)` 生成。单元测试断言 argv 里不含令牌。
- **开发时先核实**（本阶段无法在沙箱里跑探测）：`printf 'add-generic-password …\n' | security -i` 能写入，且不会弹窗或卡住。如果不行，退路是 `security-framework`（需要更新 `studio/src-tauri/Cargo.lock`，把 `Keychain` 放进 `studio/src-tauri/src/plugins.rs`），接口不变。
- 非 macOS（`cfg!(not(target_os = "macos"))`）：用 `Memory`。界面照常显示，但令牌只在本次运行有效；不在本期范围，只保证能编译。

### 2.3 远端缓存 `<catalog>/plugins/remote/<key>.json`

`key` = `sha256(仓库根绝对路径)` 的前 16 个十六进制字符。

```json
{ "repository": "/path/repo", "refreshed_at": 1764000000,
  "remote": "origin", "plugin": "github",
  "pulls_checked": true,
  "pulls": { "feat/x": { "number": 123, "title": "…", "state": "open", "url": "https://…", "created_at": 1763990000, "others": 1 } },
  "fetch_error": null,
  "api_error": { "kind": "rate_limited", "message": "…", "host": "github.com", "status": 403, "reset_at": 1764000600, "at": 1764000000 } }
```

- 刷新成功的部分覆盖对应字段；失败的部分只写对应的 `*_error`，保留上次成功的数据（FR-15）。`refreshed_at` 是最近一次 fetch 成功的时间。
- 不含令牌，也不含请求头。

## 3. 接口

### 3.1 `src/project/remote.rs`（git 层，无网络依赖可测）

```rust
pub struct RemoteUrl { pub host: String /*小写，不含端口*/, pub path: String /*owner/repo 或 g/sub/repo，无 .git*/ }
pub fn parse_url(url: &str) -> Option<RemoteUrl>;
// https://host[:port]/p(.git)  http://…  ssh://[user@]host[:port]/p  user@host:p  git://host/p
// 本地路径、file:// → None；去掉末尾 "/" 和 ".git"（AC-7、E3）

pub struct Remote { pub name: String, pub url: String, pub parsed: Option<RemoteUrl> }
pub fn remotes(root: &Path) -> Result<Vec<Remote>>;
// 读原始配置：git config --get-regexp '^remote\..*\.url$'
// 不用 `git remote get-url`：它会展开 insteadOf，主机就对不上插件了（测试也依赖这一点，见 §7.2）

pub struct Upstream { pub branch: String, pub upstream: Option<String> /*origin/feat*/,
                      pub present: bool, pub ahead: Option<u64>, pub behind: Option<u64>,
                      pub same_name: Option<String> /*无上游时远端的同名引用，E9*/ }
pub fn upstreams(root: &Path) -> Result<Vec<Upstream>>;
// git for-each-ref refs/heads --format=%(refname:short)%1f%(upstream:short)%1f%(upstream:track,nobracket)
// track 为 "gone" → present=false、ahead/behind=None（E10）；"" → 0/0；"ahead 2, behind 1" → 2/1

pub struct RemoteBranch { pub name: String /*origin/feat-y*/, pub sha: String }
pub fn remote_only(root: &Path, ups: &[Upstream]) -> Result<Vec<RemoteBranch>>;
// git for-each-ref refs/remotes --format=%(refname:short)%1f%(objectname)%1f%(symref)
// 排除 symref（origin/HEAD）、被任何本地分支当作上游的、与本地分支同名的（FR-12）

pub fn fetch(root: &Path, remote: &str, timeout: Duration) -> Result<(), FetchError>;
// git fetch --prune --no-tags --quiet <remote> +refs/heads/*:refs/remotes/<remote>/*
// 显式 refspec：只动 refs/remotes/<remote>/*，本地分支和工作区不受影响（AC-11）
// env：GIT_TERMINAL_PROMPT=0，去掉 CONTROL_* 令牌（同 workspace::git）；stdin=null；超时 60 秒后 kill
pub struct FetchError { pub message: String /*stderr 首行*/, pub auth: bool /*含 Authentication failed / Permission denied / could not read Username*/ }
```

- **开发时先核实** `%(upstream:track,nobracket)` 的三种输出（ahead/behind 组合、`gone`、空）。如果某个 git 版本不同，退路是对 `%(upstream)` 用 `git rev-list --left-right --count <b>...<up>` 计算。
- 选查 PR 用的 remote（E2）：`origin` 的主机能匹配一个已启用的插件就用 `origin`；否则用第一个能匹配的 remote；都匹配不上时，状态栏显示 `origin`（没有就显示第一个），`plugin = null`。
- 主机匹配（FR-9、E13）：`RemoteUrl.host` 与插件 host（去掉 `https://` 和端口，小写）相等。

### 3.2 `src/project/forge.rs`（只读 API）

```rust
pub enum Kind { GitHub, GitLab }
pub struct Forge { kind: Kind, api_base: String, host: String }
impl Forge {
    pub fn new(kind: Kind, host_url: &str) -> Forge;
    // GitHub: host=github.com → https://api.github.com，否则 https://<host>/api/v3
    // GitLab: https://<host>/api/v4
    pub fn with_base(kind: Kind, host: &str, base: &str) -> Result<Forge>;
    // 测试替身；只接受 https:// 或 http://127.0.0.1:<port>
    pub fn account(&self, token: &str) -> Result<String, ApiError>;               // GET /user → login / username
    pub fn pulls(&self, token: &str, path: &str) -> Result<Vec<Pull>, ApiError>;  // 见下
}
pub struct Pull { pub branch: String, pub number: u64, pub title: String, pub state: State /*Open|Draft|Merged|Closed*/, pub url: String, pub created_at: i64 }
pub struct ApiError { pub kind: ErrorKind, pub message: String, pub status: Option<u16>, pub reset_at: Option<i64> }
pub enum ErrorKind { Unreachable, Auth, Forbidden, Cert, RateLimited, Timeout, NotFound, Other }
pub fn latest_by_branch(pulls: Vec<Pull>) -> BTreeMap<String, (Pull, usize /*others*/)>;  // E8：按 created_at 取最新
pub fn normalize_host(input: &str) -> Result<String>;  // FR-8：去空白 → 无协议补 https:// → 拒绝 http:// 和非法主机 → 去掉末尾 /；空 → Err
```

**请求**：只有 `fn get(&self, token, path_and_query) -> Result<(u16, HeaderMap, Vec<u8>), ApiError>` 一个出口，不实现其他 HTTP 方法。
- argv：`/usr/bin/curl --silent --show-error --max-time 15 --proto =https --dump-header - --write-out '\n%{http_code}' --config -`。替身用 `with_base` 构造时，`--proto` 换成 `=http`。
- stdin（curl 配置）：`url = "<base><path>"` 和 `header = "Authorization: Bearer <token>"`（GitLab 用 `PRIVATE-TOKEN: <token>`），另加 `Accept: application/json` 和 `User-Agent: aep-studio`。令牌已经过 `valid_token` 检查，不含引号。
- **退出码分类**：6/7 → Unreachable；28 → Timeout；35/51/58/59/60/77/83/90/91 → Cert；其他非 0 → Other（消息取 stderr 首行）。

**HTTP 状态分类**：
- 401 → Auth
- 403 且 `x-ratelimit-remaining: 0`，或 429 → RateLimited。`reset_at` 依次取 `x-ratelimit-reset`（epoch）、`ratelimit-reset`（GitLab）、`retry-after`（秒，加上当前时间）。
- 其他 403 → Forbidden；404 → NotFound；其他非 2xx → Other（消息取响应 JSON 的 `message`，截到 120 字）。

**PR / MR 列表**（只取最近一批，避免每个分支一次请求）：
- **GitHub**：`GET /repos/{owner}/{repo}/pulls?state=all&sort=updated&direction=desc&per_page=100&page=1..3`。只保留 `head.repo.full_name == path` 的（同仓库分支），`branch = head.ref`。状态：`merged_at` 非空 → Merged；`state == closed` → Closed；`draft` → Draft；否则 Open。
- **GitLab**：`GET /projects/{percent-encode(path, 含 /)}/merge_requests?state=all&order_by=updated_at&sort=desc&per_page=100&page=1..3`。只保留 `source_project_id == target_project_id` 的，`branch = source_branch`，`number = iid`，`url = web_url`。状态：`merged` → Merged；`closed` → Closed；`draft` 或 `work_in_progress` → Draft；其他（opened、locked）→ Open。
- 返回不足 100 条就停止翻页；最多 3 页。超出范围的旧 PR 不显示，在 §6 风险里注明。
- 分支名在本地做字符串比较，不进 URL，所以带 `/` 或中文的分支名（E7）不需要编码；只有 GitLab 的项目路径要编码，用 `url` crate 的 `percent_encoding`（aep 已依赖 `url`）。

### 3.3 `studio/src-tauri/src/plugins.rs`

```rust
pub struct Ctx { pub home: PathBuf, pub secrets: Arc<dyn Secrets>, pub api_base: Option<String> /*替身*/ }
impl Ctx { pub fn real(home: PathBuf) -> Ctx }  // Keychain；读环境变量 AEP_FORGE_API_BASE，仅当它是 http://127.0.0.1:* 时生效
```

lib.rs 里每个命令都用 `run(|h| plugins::xxx(&Ctx::real(h), …))`。测试直接构造 `Ctx { secrets: Arc::new(Memory::default()), api_base: Some(stub) }`。

| 命令（前端名） | Rust | 返回 |
|---|---|---|
| `plugins()` | `list(ctx)` | `PluginInfo[]` |
| `setPluginEnabled({plugin, enabled})` | `set_enabled` | `PluginInfo[]` |
| `savePlugin({plugin, host, token})` | `save`：`host` 经 `normalize_host`；`token: string \| null`，null 表示保留原令牌 | `PluginInfo[]` |
| `clearPluginToken({plugin})` | `clear_token` | `PluginInfo[]` |
| `testPlugin({plugin, host, token})` | `test`：token 为 null 时读已保存的令牌，并按结果更新 `error`；传入的令牌不保存，也不影响徽标 | `TestResult` |
| `remoteStatus({project, path})` | `remote_status` | `RemoteStatus` |
| `remoteRefresh({project, path})` | `refresh`：已在刷新时直接返回当前状态，否则起后台线程 | `RemoteStatus`（`refreshing: true`） |
| `openUrl({url})` | `open_url`：只接受 `https://`，并能被 `url::Url` 解析；`/usr/bin/open <url>` | `null` |

- `plugin` 不在 `branches|github|gitlab` 里时返回错误「未知插件」。对 `branches` 调用 save/clear/test 时返回错误「这个插件没有可配置的令牌」。
- 徽标 `badge`（interaction §2.1，后端统一计算）：`unavailable`（远端插件，且分支管理停用）→ `disabled` → `needs_config`（远端插件、启用、无令牌）→ `error`（有 `error`）→ `enabled`。是否有令牌以 `token_hint` 为准，列表不读钥匙串。
- **刷新互斥**：`static REFRESHING: Mutex<HashSet<PathBuf>>`，key 是仓库根目录。线程结束时在 drop guard 里移除，panic 也会移除。

**刷新线程**依次做：
1. 用 `remotes()` 和 `upstreams()` 选定 PR remote，再收集各上游用到的 remote，逐个 `fetch`（60 秒超时），第一个失败写进 `fetch_error`。
2. 有匹配的插件且有令牌，并且缓存里的 `api_error` 不是 `rate_limited` 或其 `reset_at` 已过（E6：不轰炸）时，调用 `pulls()`。成功则写入 `pulls`、`pulls_checked = true`，并清掉 `api_error`；失败只写 `api_error`。`auth` / `forbidden` / `cert` 同时写进 `plugins.json` 的 `error`。
3. 写缓存（`save_json`），从互斥集合中移除。

`remote_status` 只读本地数据，不访问网络：
- 远端功能未生效（分支管理停用，或 GitHub、GitLab 都停用）时，返回 `{active: false}`，不执行任何 git 命令。
- 否则执行 `remotes` + `upstreams` + `remote_only`，合并缓存和 `refreshing` 后返回。

### 3.4 前端契约（`studio/src/api/types.ts`）

```ts
export type PluginId = "branches" | "github" | "gitlab";
export type PluginBadge = "enabled" | "disabled" | "needs_config" | "error" | "unavailable";
export type RemoteErrorKind = "unreachable" | "auth" | "forbidden" | "cert" | "rate_limited" | "timeout" | "not_found" | "other";
export interface RemoteError { kind: RemoteErrorKind; message: string; host: string; status: number | null; reset_at: number | null; at: number }
export interface PluginInfo {
  id: PluginId; enabled: boolean; badge: PluginBadge;
  host: string | null;                                   // branches 为 null
  token: { set: boolean; hint: string | null } | null;   // branches 为 null；永远没有明文
  error: RemoteError | null;
}
export type TestResult = { ok: true; account: string } | { ok: false; error: RemoteError };
export type PullState = "open" | "draft" | "merged" | "closed";
export interface Pull { number: number; title: string; state: PullState; url: string; created_at: number; others: number }
export interface RemoteStatus {
  active: boolean;
  repository: string;
  remotes: { name: string; url: string; host: string | null; path: string | null }[];
  remote: { name: string; host: string | null; path: string | null; plugin: "github" | "gitlab" | null; badge: PluginBadge | null } | null;
  refreshing: boolean;
  refreshed_at: number | null;
  branches: { name: string; upstream: string | null; present: boolean; ahead: number | null; behind: number | null; same_name: string | null }[];
  remote_only: { name: string; sha: string }[];
  pulls: Record<string, Pull>;      // key：分支名，不带 remote 前缀
  pulls_checked: boolean;
  fetch_error: { message: string; auth: boolean } | null;
  api_error: RemoteError | null;
}
```

`Api` 接口加上 §3.3 的 8 个方法。`client.ts` 是 Proxy，不用改。

### 3.5 前端结构

- **`state/store.tsx`**
  - `Page` 加 `"plugins"`。
  - 新增 `plugins: PluginInfo[] | null`、`pluginsError: string | null`、`reloadPlugins()`、`setPlugins(list)`、`pluginFocus: PluginId | null`、`openPlugin(id)`（跳到插件页并展开该插件）。
  - Provider 挂载时加载插件列表。
  - 派生 `branchesOn`（读取失败时为 `true`）和 `remoteOn`（分支管理启用，且 GitHub 或 GitLab 有一个启用）。
  - effect：`page === "branches" && !branchesOn` 时 `setPage("work")`（AC-3）。
- **`App.tsx`**：先判断 `page === "plugins"`，再判断 `!project`。
- **`Sidebar.tsx`**：foot 末尾加分隔线和「插件」（`Puzzle` 图标，不禁用；有 `badge === "error"` 时显示 `dot dot-failed`）。`branchesOn` 为假时不渲染「分支」。
- **`components/Plugins.tsx`（新）**：`PluginsPage`、`PluginCard`、`ForgeConfig`、`BranchToolsConfig`，按 interaction §3 实现。开关是乐观更新，失败时回退并显示 toast。草稿只保存在组件 state 里。
- **`lib/remote.ts`（新）**：
  - `badgeText`、`errorText(err, plugin)`（§2.2）、`pullLabel`（§2.3）、`upstreamText`（§2.4）
  - `normalizeHost`：与后端规则一致，仅用于「将保存为」提示
  - `remoteFor(status, branchName)`
- **`Branches.tsx`**
  - `remoteOn` 时：加载 `remoteStatus`；`refreshing` 为真时用 `usePoll` 每 1.5 秒轮询。点「刷新远端」调用 `remoteRefresh`，完成（`refreshing` 从 true 变 false）后再调一次 `load()`。
  - 加 `RemoteBar`（状态栏和错误行）、行内比较和 PR 标签、`RemoteDetail`（`graph-remote`）、`RemoteOnly` 分组（展开状态按仓库存进组件 `Map`）。
  - 页底说明改成一行链接（interaction §4.6）。
  - `remoteOn` 为假时不调用 `remoteStatus`，渲染与现在一致（AC-15）。
  - 筛选同时匹配 `#编号` 和 PR 标题。
- **`api/mock.ts`**：按 interaction §6 提供可变的插件状态（内存）、三个仓库的 `remoteStatus`、延迟 1 秒的刷新，以及按令牌内容决定的测试结果（如 `bad` → auth，`slow` → timeout，其他 → `octocat`）。不发任何网络请求（E14）。

## 4. 变更文件清单

| 文件 | 内容 |
|---|---|
| `src/project.rs` | `pub mod forge; pub mod remote; pub mod secret;` |
| `src/project/remote.rs`（新） | §3.1 及单元测试（URL 解析 AC-7） |
| `src/project/forge.rs`（新） | §3.2 及单元测试（用 `std::net::TcpListener` 做替身：401/403 限流/429/超时/四类 PR/E8/GitLab 路径编码） |
| `src/project/secret.rs`（新） | §2.2 及单元测试（argv 不含令牌、`valid_token`、`Memory`） |
| `tests/studio_plugins_remote.rs`（新） | 用真实临时仓库测 `upstreams` / `remote_only` / `fetch`（AC-9、AC-10、AC-11） |
| `studio/src-tauri/src/plugins.rs`（新） | §3.3 |
| `studio/src-tauri/src/lib.rs` | `pub mod plugins;`，加 8 个 `#[tauri::command(async)]` 并注册到 `generate_handler!` |
| `studio/src-tauri/tests/plugins.rs`（新） | 命令层端到端（§7.2） |
| `studio/src/api/types.ts`、`studio/src/api/mock.ts` | §3.4、§3.5 |
| `studio/src/state/store.tsx`、`studio/src/App.tsx`、`studio/src/components/Sidebar.tsx` | §3.5 |
| `studio/src/components/Plugins.tsx`（新）、`studio/src/lib/remote.ts`（新） | §3.5 |
| `studio/src/components/Branches.tsx`、`studio/src/styles/app.css` | §3.5 |
| `studio/tests/ui.spec.ts` | §7.3 |

## 5. 兼容性

- **没有 `plugins.json` 的老用户**：分支管理默认启用，「分支」入口和现在一样（AC-2、AC-15）。
- **`git-tools.json`**：路径、格式和读取代码都不变（FR-18）。
- **`branch_graph`**：返回结构不变。远端数据走独立的 `remoteStatus`，远端插件全部停用时前端不会调用它，网络和 git 行为与改动前一致。
- **现有 UI 测试** `the branch page draws branches…`：断言的文字没有变（页底说明不在断言里），应原样通过。
- **mock 默认状态**：GitHub 启用（为了演示）。这会让分支页多出状态栏，但不影响现有断言（行文字仍含「Claude 工作区 · 0 改动 · 74 未跟踪」）。如果开发时发现有冲突，改成在新用例里先开启 GitHub，并在验收记录中说明。
- **GPUI `desktop/`** 不改。
- **非 macOS**：令牌存在内存里，能编译；不在验收范围。

## 6. 风险与回退

| 风险 | 处理 |
|---|---|
| `security -i` 行为与预期不符（弹窗或卡住） | T2 先在本机核实；不行就改用 `security-framework`（§2.2 退路），并在结论里注明锁文件变更。 |
| 读钥匙串时 macOS 弹出授权框 | 只有测试连接和刷新远端会读取，列表只看 `token_hint`；由用户操作触发，可以接受。 |
| `curl` 版本不支持 `--config -` 或 `--write-out` | macOS 自带 curl 7.x/8.x 都支持；T2 测试覆盖。 |
| 大仓库 PR 很多，3 页 300 条之外的分支看不到 PR | 本期接受，在详情行里不显示；后续可以改成按 head 精确查询。 |
| fetch 慢或卡住 | 60 秒超时后 kill；后台线程不阻塞 UI；互斥避免重复发起。 |
| 令牌出现在错误信息里 | URL 和 argv 不含令牌，stderr 只取首行；T3 测试用 grep 覆盖缓存、`plugins.json` 和返回给前端的 JSON。 |

## 7. 测试计划

### 7.1 根 crate：`cargo test --locked`

- `remote::tests`：AC-7 的六种 URL（`https://github.com/o/r`、`https://github.com/o/r.git`、`git@github.com:o/r.git`、`ssh://git@gitlab.example.com:2222/g/sub/r.git`、`https://gitlab.example.com/g/sub/r/`、本地路径 → None）。
- `forge::tests`：
  - `normalize_host`（AC-6：`http://x` 拒绝；`gitlab.example.com/` → `https://gitlab.example.com`）
  - 替身返回 200 /user、401、403 加 `x-ratelimit-remaining: 0`、429 加 `retry-after`、不响应（把超时设短，用 `Forge::with_timeout` 测试构造）、四类 PR 与同分支多条（E8）、GitLab 多级路径编码
  - 证书错误用 `https://127.0.0.1:<port>` 指向明文替身，期望 `Cert` 或 `Unreachable` 中的 `Cert`；若本机 curl 返回码不稳定，改为直接测「退出码 → 类别」的纯函数
- `secret::tests`：`keychain_argv` 的 argv 不含令牌；`valid_token`；`Memory` 的读写删。钥匙串真实读写测试标 `#[ignore]`，手动 `cargo test --locked secret -- --ignored` 验证。
- `tests/studio_plugins_remote.rs`：
  - 用 bare 仓库加 clone 构造：本地领先上游 2、落后 1（AC-9）；删除远端跟踪引用后 `present=false`（E10）；本地无上游但远端有同名分支（E9）；有一个仅远端分支（AC-10）
  - `fetch` 前后，所有 `refs/heads` 的 sha 和 `git status --porcelain` 不变（AC-11）

### 7.2 Studio 命令层

先在仓库根目录 `cargo build --bin aep`（现有约定），再 `cargo test --locked --manifest-path studio/src-tauri/Cargo.toml --test plugins`：
- AC-2：首次 `list` 的默认值；`set_enabled` 后新建 `Ctx` 再读，状态保持。
- AC-3：停用分支管理后，github 的 `badge = unavailable`，`set_enabled(github, true)` 不改变其生效状态；重新启用后恢复。
- AC-4：替身 `/user` 返回 `octocat` → `ok`；返回 401 → `auth`；`host` 指向未监听端口 → `unreachable`。
- AC-5：用 `Memory` 保存令牌 `ghp_TESTTOKEN1234567890`；在临时 catalog 下递归读所有文件，都不含该字符串；`list`、`remote_status` 序列化后也不含；`token.hint == "7890"`。
- AC-6：`save(host="http://x")` 返回错误；`gitlab.example.com/` 保存为 `https://gitlab.example.com`。
- AC-8：无 remote 的仓库，`remote = null`；remote 主机是 `gitlab.example.com` 而只启用了 GitHub 时，`plugin = null`。
- AC-11 / AC-12 / AC-13 / AC-14：
  - 测试仓库的 `remote.origin.url = https://github.com/acme/demo.git`，同时 `url.<bare路径>.insteadOf = https://github.com/acme/demo.git`。这样 fetch 走本地 bare 仓库，而主机匹配读原始 URL。
  - 替身返回四类 PR（其中一条 head 是 `aep-stage/<id>`，同一分支两条）。
  - 连续调用两次 `refresh`：第二次立即返回 `refreshing = true`，替身计数器只收到一轮请求。
  - 轮询到 `refreshing = false` 后检查 `pulls`、`others`、`refreshed_at`，以及本地 sha 不变。
  - 替身改为 401：`api_error.kind = auth`，插件 `badge = error`，之前的 `pulls` 仍在。
  - 替身改为 429：`rate_limited` 带 `reset_at`；再次 refresh，替身计数不增加（E6）。
  - 替身改为不响应：`timeout`（测试 Ctx 把 API 超时设为 1 秒）。
- `open_url`：`http://…`、`file://…`、`javascript:` 都返回错误；`https://` 校验通过（测试里不真的打开浏览器，抽出 `check_url` 纯函数来测）。

### 7.3 前端：`cd studio && npm run build && npx playwright test`

新用例（mock）：
1. **插件页**（AC-1/2/3）：无项目时也能打开（mock 移除全部项目后点「插件」，或单独 reset）；三张卡的徽标；停用分支管理后「分支」入口消失，GitHub 显示「不可用」，重新启用后恢复；截图 `30-plugins`。
2. **配置**（AC-4/6）：`http://x` 时保存按钮禁用；`gitlab.example.com/` 显示「将保存为 https://gitlab.example.com」；令牌 `bad` 测试得到「令牌无效或过期」；正常令牌显示「已连接为 octocat」；保存后显示「已设置 · ••••」，输入框清空；截图 `31-plugin-config`。
3. **分支页远端**（AC-9/10/12/14）：状态栏、`↑2 ↓1（相对 origin/…）` 与「相对 HEAD 领先」同时出现、「未知」、「未跟踪 · 远端有同名分支」；四类 PR 标签和 `+2`；`aep-stage` 行同时有事项和 PR；「仅远端」默认折叠，展开后数量正确；点 PR 标签调用 `openUrl`（mock 用 `console.info` 记录）；截图 `32-branches-remote`。
4. **刷新与错误**（AC-11/13）：点刷新后按钮显示「正在刷新…」并禁用，完成后显示「上次刷新 刚刚」；mock 错误开关依次给出 auth 和限流两种错误行，本地列表仍可点击。
5. **AC-15**：关闭 GitHub 和 GitLab 后没有状态栏，原有用例的断言仍然成立。

### 7.4 只读检查（AC-17）与 GUI

- `! rg -n -i 'git"?,?\s*"push"|"push"\]|-X\s*(POST|PUT|PATCH|DELETE)|request\s*=\s*"?(POST|PUT|PATCH|DELETE)' src/project/remote.rs src/project/forge.rs studio/src-tauri/src/plugins.rs` 应无输出。
- GUI（可选，需要 macOS 桌面）：`studio/bundle-macos.sh` 打包后，按 AC-1 → AC-3 → AC-9 → AC-12 走一遍并截图。真实 github.com 连接属于可选人工验证；没有做时在结论里注明「未覆盖」。

## 8. 对 interaction.md §7 的答复

| 需求 | 方案 |
|---|---|
| 插件列表字段 | `PluginInfo`（§3.4）；徽标由后端算 |
| 插件操作 | §3.3 前 5 个命令 |
| 远端数据按仓库 | `RemoteStatus`；本地部分实时计算，PR 与时间来自缓存 |
| 刷新立即返回、后台执行 | `remoteRefresh` 起线程，前端用 `usePoll` 查 `remoteStatus.refreshing` |
| 外链打开 | `openUrl`，仅 `https://` |
