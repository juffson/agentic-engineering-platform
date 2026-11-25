//! Studio's global plugin settings and repository-scoped remote status.
use aep::{
    project::{
        Library,
        forge::{self, ApiError, ErrorKind, Forge},
        remote::{self, Remote, RemoteBranch, Upstream},
        save_json,
        secret::{Secrets, valid_token},
    },
    workspace::{Workspace, git},
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::{ErrorKind as IoError, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, LazyLock, Mutex, MutexGuard},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::Url;

const SETTINGS_FILE: &str = "plugins.json";
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct Ctx {
    pub home: PathBuf,
    pub secrets: Arc<dyn Secrets>,
    pub api_base: Option<String>,
}

impl Ctx {
    pub fn real(home: PathBuf) -> Self {
        #[cfg(target_os = "macos")]
        let secrets = Arc::new(aep::project::secret::Keychain::default());
        #[cfg(not(target_os = "macos"))]
        let secrets = {
            use aep::project::secret::Memory;
            static MEMORY: LazyLock<Arc<Memory>> = LazyLock::new(|| Arc::new(Memory::default()));
            Arc::clone(&MEMORY)
        };
        let api_base = std::env::var("AEP_FORGE_API_BASE").ok().filter(|base| {
            base.starts_with("http://127.0.0.1:")
                && Forge::with_base(forge::Kind::GitHub, "github.com", base).is_ok()
        });
        Self {
            home,
            secrets,
            api_base,
        }
    }

    fn dir(&self) -> PathBuf {
        self.home.join("plugins")
    }

    fn forge(&self, provider: Provider, host: &str) -> Result<Forge> {
        match &self.api_base {
            Some(base) => Forge::with_base(provider.kind(), host, base),
            None => Ok(Forge::new(provider.kind(), host)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Provider {
    Github,
    Gitlab,
}

impl Provider {
    const ALL: [Self; 2] = [Self::Github, Self::Gitlab];

    fn parse(plugin: &str) -> Result<Self> {
        match plugin {
            "github" => Ok(Self::Github),
            "gitlab" => Ok(Self::Gitlab),
            "branches" => bail!("这个插件没有可配置的令牌"),
            _ => bail!("未知插件"),
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Gitlab => "gitlab",
        }
    }

    fn kind(self) -> forge::Kind {
        match self {
            Self::Github => forge::Kind::GitHub,
            Self::Gitlab => forge::Kind::GitLab,
        }
    }

    fn default_host(self) -> &'static str {
        match self {
            Self::Github => "https://github.com",
            Self::Gitlab => "https://gitlab.com",
        }
    }

    fn host(self, input: &str) -> Result<String> {
        forge::normalize_host(if input.trim().is_empty() {
            self.default_host()
        } else {
            input
        })
    }
}

#[derive(Clone, Deserialize, Serialize)]
struct Settings {
    version: u32,
    branches: Switch,
    github: Connection,
    gitlab: Connection,
}

#[derive(Clone, Deserialize, Serialize)]
struct Switch {
    enabled: bool,
}

#[derive(Clone, Deserialize, Serialize)]
struct Connection {
    enabled: bool,
    host: String,
    token_hint: Option<String>,
    error: Option<SavedError>,
}

impl Default for Settings {
    fn default() -> Self {
        let connection = |provider: Provider| Connection {
            enabled: false,
            host: provider.default_host().to_owned(),
            token_hint: None,
            error: None,
        };
        Self {
            version: 1,
            branches: Switch { enabled: true },
            github: connection(Provider::Github),
            gitlab: connection(Provider::Gitlab),
        }
    }
}

impl Settings {
    fn connection(&self, provider: Provider) -> &Connection {
        match provider {
            Provider::Github => &self.github,
            Provider::Gitlab => &self.gitlab,
        }
    }

    fn connection_mut(&mut self, provider: Provider) -> &mut Connection {
        match provider {
            Provider::Github => &mut self.github,
            Provider::Gitlab => &mut self.gitlab,
        }
    }

    fn remote_on(&self) -> bool {
        self.branches.enabled && Provider::ALL.iter().any(|&id| self.connection(id).enabled)
    }

    fn badge(&self, provider: Provider) -> &'static str {
        let connection = self.connection(provider);
        if !self.branches.enabled {
            "unavailable"
        } else if !connection.enabled {
            "disabled"
        } else if connection.token_hint.is_none() {
            "needs_config"
        } else if connection.error.is_some() {
            "error"
        } else {
            "enabled"
        }
    }

    fn view(&self) -> Vec<Value> {
        let mut plugins = vec![json!({
            "id": "branches", "enabled": self.branches.enabled,
            "badge": if self.branches.enabled { "enabled" } else { "disabled" },
            "host": null, "token": null, "error": null,
        })];
        plugins.extend(Provider::ALL.map(|id| {
            let connection = self.connection(id);
            json!({
                "id": id, "enabled": connection.enabled, "badge": self.badge(id),
                "host": connection.host,
                "token": { "set": connection.token_hint.is_some(), "hint": connection.token_hint },
                "error": connection.error.as_ref().map(|error| error.for_host(&connection.host)),
            })
        }));
        plugins
    }
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
struct SavedError {
    kind: ErrorKind,
    message: String,
    at: u64,
}

impl SavedError {
    fn for_host(&self, host: &str) -> RemoteError {
        RemoteError {
            kind: self.kind,
            message: self.message.clone(),
            host: host_name(host),
            status: None,
            reset_at: None,
            at: self.at,
        }
    }
}

#[derive(Deserialize, Serialize)]
struct RemoteError {
    kind: ErrorKind,
    message: String,
    host: String,
    status: Option<u16>,
    reset_at: Option<i64>,
    at: u64,
}

impl RemoteError {
    fn new(host: &str, error: ApiError) -> Self {
        Self {
            kind: error.kind,
            message: error.message,
            host: host_name(host),
            status: error.status,
            reset_at: error.reset_at,
            at: now(),
        }
    }

    fn problem(host: &str, kind: ErrorKind, message: &str) -> Self {
        Self::new(
            host,
            ApiError {
                kind,
                message: message.into(),
                status: None,
                reset_at: None,
            },
        )
    }

    fn is_active_limit(&self) -> bool {
        self.kind == ErrorKind::RateLimited
            && self.reset_at.is_none_or(|reset| reset > now() as i64)
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn host_name(input: &str) -> String {
    Url::parse(input)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_default()
}

enum SettingsFile {
    Ready(Settings, bool),
    Corrupt(Vec<u8>),
}

fn read_settings(ctx: &Ctx) -> Result<SettingsFile> {
    let bytes = match fs::read(ctx.dir().join(SETTINGS_FILE)) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == IoError::NotFound => {
            return Ok(SettingsFile::Ready(Settings::default(), false));
        }
        Err(err) => return Err(err).context("无法读取插件配置"),
    };
    match serde_json::from_slice::<Settings>(&bytes) {
        Ok(settings) => {
            ensure!(settings.version == 1, "不支持此插件配置版本");
            Ok(SettingsFile::Ready(settings, true))
        }
        Err(_) => Ok(SettingsFile::Corrupt(bytes)),
    }
}

fn settings(ctx: &Ctx) -> Result<(Settings, bool)> {
    match read_settings(ctx)? {
        SettingsFile::Ready(settings, exists) => Ok((settings, exists)),
        SettingsFile::Corrupt(_) => bail!("插件配置文件损坏"),
    }
}

fn write_settings(ctx: &Ctx, settings: &Settings) -> Result<()> {
    let dir = ctx.dir();
    fs::create_dir_all(&dir).context("无法创建插件配置目录")?;
    save_json(&dir, SETTINGS_FILE, settings).context("无法保存插件配置")
}

// Serialize settings changes and version the credentials used by in-flight requests.
// Neither HTTP nor fetch holds this lock.
type Revisions = BTreeMap<(PathBuf, Provider), u64>;
static SETTINGS: LazyLock<Mutex<Revisions>> = LazyLock::new(|| Mutex::new(Revisions::new()));

fn settings_lock() -> Result<MutexGuard<'static, Revisions>> {
    SETTINGS.lock().map_err(|_| anyhow!("无法访问插件配置"))
}

fn credential_revision(revisions: &Revisions, ctx: &Ctx, provider: Provider) -> u64 {
    revisions
        .get(&(ctx.home.clone(), provider))
        .copied()
        .unwrap_or_default()
}

fn changed(revisions: &mut Revisions, ctx: &Ctx, provider: Provider) {
    *revisions.entry((ctx.home.clone(), provider)).or_default() += 1;
}

pub fn list(ctx: &Ctx) -> Result<Vec<Value>> {
    let _guard = settings_lock()?;
    Ok(settings(ctx)?.0.view())
}

pub fn set_enabled(ctx: &Ctx, plugin: &str, enabled: bool) -> Result<Vec<Value>> {
    let provider = if plugin == "branches" {
        None
    } else {
        Some(Provider::parse(plugin)?)
    };
    let mut revisions = settings_lock()?;
    let (mut current, recovered) = match read_settings(ctx)? {
        SettingsFile::Ready(current, _) => (current, false),
        SettingsFile::Corrupt(bytes) => {
            fs::write(ctx.dir().join("plugins.json.bad"), bytes)
                .context("无法备份损坏的插件配置")?;
            (Settings::default(), true)
        }
    };
    if let Some(provider) = provider {
        current.connection_mut(provider).enabled = enabled;
    } else {
        current.branches.enabled = enabled;
    }
    write_settings(ctx, &current)?;
    if recovered {
        for provider in Provider::ALL {
            changed(&mut revisions, ctx, provider);
        }
    }
    Ok(current.view())
}

// Commit the settings file first so an unwritable catalog never changes the Keychain.
// Roll the settings back on a rejected Keychain operation before releasing the lock.
fn write_credentials(
    ctx: &Ctx,
    previous: &Settings,
    existed: bool,
    next: &Settings,
    apply: impl FnOnce() -> Result<()>,
) -> Result<()> {
    write_settings(ctx, next)?;
    if let Err(error) = apply() {
        let restored = if existed {
            write_settings(ctx, previous)
        } else {
            fs::remove_file(ctx.dir().join(SETTINGS_FILE)).map_err(Into::into)
        };
        restored.with_context(|| format!("{error}，且无法恢复插件配置"))?;
        return Err(error);
    }
    Ok(())
}

pub fn save(ctx: &Ctx, plugin: &str, host: &str, token: Option<&str>) -> Result<Vec<Value>> {
    let provider = Provider::parse(plugin)?;
    let host = provider.host(host)?;
    if let Some(token) = token {
        ensure!(valid_token(token), "访问令牌格式无效");
    }
    let mut revisions = settings_lock()?;
    let (previous, existed) = settings(ctx)?;
    let mut next = previous.clone();
    let connection = next.connection_mut(provider);
    let changed_host = connection.host != host;
    connection.host = host;
    connection.error = None;
    if let Some(token) = token {
        connection.token_hint = Some(token[token.len() - 4..].to_owned());
        write_credentials(ctx, &previous, existed, &next, || {
            ctx.secrets
                .set(provider.id(), token)
                .map_err(|_| anyhow!("无法保存访问令牌"))
        })?;
    } else {
        write_settings(ctx, &next)?;
    }
    if changed_host || token.is_some() {
        changed(&mut revisions, ctx, provider);
    }
    Ok(next.view())
}

pub fn clear_token(ctx: &Ctx, plugin: &str) -> Result<Vec<Value>> {
    let provider = Provider::parse(plugin)?;
    let mut revisions = settings_lock()?;
    let (previous, existed) = settings(ctx)?;
    let mut next = previous.clone();
    let connection = next.connection_mut(provider);
    connection.token_hint = None;
    connection.error = None;
    write_credentials(ctx, &previous, existed, &next, || {
        ctx.secrets
            .delete(provider.id())
            .map_err(|_| anyhow!("无法清除访问令牌"))
    })?;
    changed(&mut revisions, ctx, provider);
    Ok(next.view())
}

fn read_token(ctx: &Ctx, provider: Provider) -> Result<Option<String>> {
    ctx.secrets
        .get(provider.id())
        .map_err(|_| anyhow!("无法读取访问令牌"))
}

fn record_health(
    ctx: &Ctx,
    provider: Provider,
    revision: u64,
    error: Option<&RemoteError>,
) -> Result<()> {
    let next = match error {
        Some(error)
            if matches!(
                error.kind,
                ErrorKind::Auth | ErrorKind::Forbidden | ErrorKind::Cert
            ) =>
        {
            Some(SavedError {
                kind: error.kind,
                message: error.message.clone(),
                at: error.at,
            })
        }
        Some(_) => return Ok(()),
        None => None,
    };
    let revisions = settings_lock()?;
    if credential_revision(&revisions, ctx, provider) == revision {
        let mut current = settings(ctx)?.0;
        let connection = current.connection_mut(provider);
        if connection.error != next {
            connection.error = next;
            write_settings(ctx, &current)?;
        }
    }
    Ok(())
}

pub fn test(ctx: &Ctx, plugin: &str, host: &str, token: Option<&str>) -> Result<Value> {
    let provider = Provider::parse(plugin)?;
    let host = provider.host(host)?;
    let (token, revision) = match token {
        Some(token) => (Some(token.to_owned()), None),
        None => {
            let revisions = settings_lock()?;
            let revision = credential_revision(&revisions, ctx, provider);
            let token = if settings(ctx)?.0.connection(provider).token_hint.is_some() {
                read_token(ctx, provider)?
            } else {
                None
            };
            (token, Some(revision))
        }
    };
    let Some(token) = token else {
        return Ok(
            json!({ "ok": false, "error": RemoteError::problem(&host, ErrorKind::Auth, "先填写访问令牌") }),
        );
    };
    let result = ctx.forge(provider, &host)?.account(&token);
    let error = result
        .as_ref()
        .err()
        .cloned()
        .map(|error| RemoteError::new(&host, error));
    if let Some(revision) = revision {
        record_health(ctx, provider, revision, error.as_ref())?;
    }
    Ok(match result {
        Ok(account) => json!({ "ok": true, "account": account }),
        Err(_) => json!({ "ok": false, "error": error }),
    })
}

#[derive(Default, Deserialize, Serialize)]
struct Cache {
    repository: PathBuf,
    refreshed_at: Option<u64>,
    remote: Option<String>,
    plugin: Option<Provider>,
    // A remote can keep its name when its URL changes.
    source: Option<Source>,
    pulls_checked: bool,
    pulls: BTreeMap<String, PullInfo>,
    fetch_error: Option<FetchError>,
    api_error: Option<RemoteError>,
}

#[derive(PartialEq, Eq, Deserialize, Serialize)]
struct Source {
    host: Option<String>,
    path: Option<String>,
    plugin_host: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct PullInfo {
    number: u64,
    title: String,
    state: forge::State,
    url: String,
    created_at: i64,
    others: usize,
}

impl From<(forge::Pull, usize)> for PullInfo {
    fn from((pull, others): (forge::Pull, usize)) -> Self {
        Self {
            number: pull.number,
            title: pull.title,
            state: pull.state,
            url: pull.url,
            created_at: pull.created_at,
            others,
        }
    }
}

#[derive(Deserialize, Serialize)]
struct FetchError {
    message: String,
    auth: bool,
}

struct Repo {
    root: PathBuf,
    remotes: Vec<Remote>,
    upstreams: Vec<Upstream>,
    remote_only: Vec<RemoteBranch>,
}

impl Repo {
    fn open(ctx: &Ctx, project: &str, path: &str) -> Result<Self> {
        let library = Library::open(&ctx.home)?;
        let project = library
            .projects
            .iter()
            .find(|p| p.id == project)
            .context("找不到项目")?;
        let root = Workspace::open(Path::new(path.trim()))
            .context("不是 Git 仓库")?
            .root
            .canonicalize()?;
        ensure!(
            project
                .repositories
                .iter()
                .any(|repo| repo == &root || repo.canonicalize().is_ok_and(|repo| repo == root)),
            "仓库未关联到此项目"
        );
        let remotes = remote::remotes(&root)?;
        let upstreams = remote::upstreams(&root)?;
        let remote_only = remote::remote_only(&root, &upstreams)?;
        Ok(Self {
            root,
            remotes,
            upstreams,
            remote_only,
        })
    }

    fn select(&self, settings: &Settings) -> Option<Selection<'_>> {
        let provider = |remote: &Remote| {
            let host = &remote.parsed.as_ref()?.host;
            Provider::ALL.into_iter().find(|&id| {
                let connection = settings.connection(id);
                connection.enabled && host_name(&connection.host) == *host
            })
        };
        let origin = self.remotes.iter().find(|remote| remote.name == "origin");
        origin
            .and_then(|remote| {
                provider(remote).map(|id| Selection {
                    remote,
                    plugin: Some(id),
                })
            })
            .or_else(|| {
                self.remotes.iter().find_map(|remote| {
                    provider(remote).map(|id| Selection {
                        remote,
                        plugin: Some(id),
                    })
                })
            })
            .or_else(|| {
                origin
                    .or_else(|| self.remotes.first())
                    .map(|remote| Selection {
                        remote,
                        plugin: None,
                    })
            })
    }

    fn status(&self, settings: &Settings, cache: &Cache, refreshing: bool) -> Value {
        let selected = self.select(settings);
        let has_token = selected
            .and_then(|s| s.plugin)
            .is_some_and(|provider| settings.connection(provider).token_hint.is_some());
        let remotes: Vec<_> = self
            .remotes
            .iter()
            .map(|remote| {
                json!({
                    "name": remote.name, "url": remote.url,
                    "host": remote.parsed.as_ref().map(|url| &url.host),
                    "path": remote.parsed.as_ref().map(|url| &url.path),
                })
            })
            .collect();
        let branches: Vec<_> = self
            .upstreams
            .iter()
            .map(|up| {
                json!({
                    "name": up.branch, "upstream": up.upstream, "present": up.present,
                    "ahead": up.ahead, "behind": up.behind, "same_name": up.same_name,
                })
            })
            .collect();
        let remote_only: Vec<_> = self
            .remote_only
            .iter()
            .map(|branch| {
                json!({
                    "name": branch.name, "sha": branch.sha,
                })
            })
            .collect();
        json!({
            "active": true, "repository": self.root, "remotes": remotes,
            "remote": selected.map(|s| json!({
                "name": s.remote.name, "plugin": s.plugin, "badge": s.plugin.map(|id| settings.badge(id)),
                "host": s.remote.parsed.as_ref().map(|url| &url.host),
                "path": s.remote.parsed.as_ref().map(|url| &url.path),
            })),
            "refreshing": refreshing, "refreshed_at": cache.refreshed_at,
            "branches": branches, "remote_only": remote_only,
            "pulls": if has_token { json!(cache.pulls) } else { json!({}) },
            "pulls_checked": has_token && cache.pulls_checked,
            "fetch_error": (!refreshing).then_some(cache.fetch_error.as_ref()).flatten(),
            "api_error": (has_token && !refreshing).then_some(cache.api_error.as_ref()).flatten(),
        })
    }
}

#[derive(Clone, Copy)]
struct Selection<'a> {
    remote: &'a Remote,
    plugin: Option<Provider>,
}

fn cache_file(ctx: &Ctx, root: &Path) -> Result<PathBuf> {
    // The Studio crate has no direct hash dependency. Use the platform SHA-256 utility
    // on the absolute path bytes, without a trailing newline.
    let mut command = if cfg!(target_os = "macos") {
        let mut command = Command::new("/usr/bin/shasum");
        command.args(["-a", "256", "-b"]);
        command
    } else {
        Command::new("sha256sum")
    };
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("无法计算远端缓存键")?;
    let written = child
        .stdin
        .take()
        .unwrap()
        .write_all(root.as_os_str().as_encoded_bytes());
    let output = child.wait_with_output().context("无法计算远端缓存键")?;
    ensure!(output.status.success(), "无法计算远端缓存键");
    written.context("无法计算远端缓存键")?;
    let hash = output
        .stdout
        .get(..64)
        .filter(|s| s.iter().all(u8::is_ascii_hexdigit))
        .context("远端缓存键无效")?;
    let key = std::str::from_utf8(&hash[..16])?;
    Ok(ctx.dir().join("remote").join(format!("{key}.json")))
}

fn load_cache(
    file: &Path,
    root: &Path,
    selected: Selection<'_>,
    settings: &Settings,
) -> Result<Cache> {
    let parsed = selected.remote.parsed.as_ref();
    let empty = Cache {
        repository: root.to_owned(),
        remote: Some(selected.remote.name.clone()),
        plugin: selected.plugin,
        source: Some(Source {
            host: parsed.map(|url| url.host.clone()),
            path: parsed.map(|url| url.path.clone()),
            plugin_host: selected
                .plugin
                .map(|id| settings.connection(id).host.clone()),
        }),
        ..Cache::default()
    };
    let bytes = match fs::read(file) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == IoError::NotFound => return Ok(empty),
        Err(err) => return Err(err).context("无法读取远端缓存"),
    };
    let cached: Option<Cache> = serde_json::from_slice(&bytes).ok();
    Ok(cached
        .filter(|entry| {
            entry.repository == empty.repository
                && entry.remote == empty.remote
                && entry.plugin == empty.plugin
                && entry.source == empty.source
        })
        .unwrap_or(empty))
}

static REFRESHING: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

struct RefreshGuard(PathBuf);

impl Drop for RefreshGuard {
    fn drop(&mut self) {
        REFRESHING
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.0);
    }
}

pub fn remote_status(ctx: &Ctx, project: &str, path: &str) -> Result<Value> {
    let settings = {
        let _guard = settings_lock()?;
        settings(ctx)?.0
    };
    if !settings.remote_on() {
        return Ok(json!({ "active": false }));
    }
    let repo = Repo::open(ctx, project, path)?;
    let refreshing = REFRESHING
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .contains(&repo.root);
    let cache = match repo.select(&settings) {
        Some(selected) => load_cache(
            &cache_file(ctx, &repo.root)?,
            &repo.root,
            selected,
            &settings,
        )?,
        None => Cache::default(),
    };
    Ok(repo.status(&settings, &cache, refreshing))
}

pub fn refresh(ctx: &Ctx, project: &str, path: &str) -> Result<Value> {
    let (settings, revisions) = {
        let revisions = settings_lock()?;
        let settings = settings(ctx)?.0;
        let revisions = Provider::ALL.map(|id| (id, credential_revision(&revisions, ctx, id)));
        (settings, revisions)
    };
    if !settings.remote_on() {
        return Ok(json!({ "active": false }));
    }
    let repo = Repo::open(ctx, project, path)?;
    let Some(selected) = repo.select(&settings) else {
        return Ok(repo.status(&settings, &Cache::default(), false));
    };
    let inserted = REFRESHING
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(repo.root.clone());
    let guard = inserted.then(|| RefreshGuard(repo.root.clone()));
    let file = cache_file(ctx, &repo.root)?;
    let cache = load_cache(&file, &repo.root, selected, &settings)?;
    let response = repo.status(&settings, &cache, true);
    let Some(guard) = guard else {
        return Ok(response);
    };
    let dir = file.parent().unwrap().to_owned();
    fs::create_dir_all(&dir).context("无法创建远端缓存目录")?;
    let ctx = ctx.clone();
    thread::Builder::new()
        .name("studio-remote-refresh".to_owned())
        .spawn(move || {
            let _guard = guard;
            let mut cache = cache;
            let selected = repo.select(&settings).unwrap();
            refresh_git(&repo, selected.remote, &mut cache);
            let health = selected.plugin.and_then(|id| {
                let revision = revisions.iter().find(|(provider, _)| *provider == id)?.1;
                let result = refresh_pulls(&ctx, selected, &settings, id, revision, &mut cache);
                result.map(|()| record_health(&ctx, id, revision, cache.api_error.as_ref()))
            });
            let saved = save_json(&dir, file.file_name().unwrap().to_str().unwrap(), &cache);
            if saved.is_err() || health.is_some_and(|result| result.is_err()) {
                eprintln!("Studio 无法保存插件刷新结果");
            }
        })
        .context("无法启动远端刷新")?;
    Ok(response)
}

fn refresh_git(repo: &Repo, selected: &Remote, cache: &mut Cache) {
    cache.fetch_error = None;
    // Ask Git for the configured remote names. A short upstream display name can
    // also name a local branch or include a remote whose name contains a slash.
    let upstreams = match git(
        &repo.root,
        &[
            "for-each-ref",
            "--format=%(upstream:remotename)",
            "refs/heads/",
        ],
    ) {
        Ok(upstreams) => upstreams,
        Err(_) => {
            cache.fetch_error = Some(FetchError {
                message: "无法读取分支的上游远端".into(),
                auth: false,
            });
            String::new()
        }
    };
    let upstreams: HashSet<_> = upstreams.lines().collect();
    let inputs =
        std::iter::once(selected).chain(repo.remotes.iter().filter(|remote| {
            remote.name != selected.name && upstreams.contains(remote.name.as_str())
        }));
    for remote in inputs {
        if let Err(error) = remote::fetch(&repo.root, &remote.name, FETCH_TIMEOUT) {
            cache.fetch_error.get_or_insert(FetchError {
                message: error.message,
                auth: error.auth,
            });
        }
    }
    if cache.fetch_error.is_none() {
        cache.refreshed_at = Some(now());
    }
}

fn refresh_pulls(
    ctx: &Ctx,
    selected: Selection<'_>,
    settings_before: &Settings,
    provider: Provider,
    revision: u64,
    cache: &mut Cache,
) -> Option<()> {
    let connection = settings_before.connection(provider);
    let path = &selected.remote.parsed.as_ref()?.path;
    if connection.token_hint.is_none()
        || cache
            .api_error
            .as_ref()
            .is_some_and(RemoteError::is_active_limit)
    {
        return None;
    }
    let token = (|| -> Result<Option<String>> {
        let revisions = settings_lock()?;
        if credential_revision(&revisions, ctx, provider) != revision {
            return Ok(None);
        }
        read_token(ctx, provider)
    })();
    let host = &connection.host;
    let result = match token {
        Ok(Some(token)) => ctx
            .forge(provider, host)
            .map_err(|_| RemoteError::problem(host, ErrorKind::Other, "远端 API 地址无效"))
            .and_then(|forge| {
                forge
                    .pulls(&token, path)
                    .map_err(|error| RemoteError::new(host, error))
            }),
        Ok(None) => return None,
        Err(_) => Err(RemoteError::problem(
            host,
            ErrorKind::Other,
            "无法读取访问令牌",
        )),
    };
    match result {
        Ok(pulls) => {
            cache.pulls = forge::latest_by_branch(pulls)
                .into_iter()
                .map(|(branch, pull)| (branch, pull.into()))
                .collect();
            cache.pulls_checked = true;
            cache.api_error = None;
        }
        Err(error) => cache.api_error = Some(error),
    }
    Some(())
}

pub fn check_url(input: &str) -> Result<Url> {
    ensure!(input.starts_with("https://"), "只支持 https 地址");
    let url = Url::parse(input).context("链接地址无效")?;
    ensure!(url.host_str().is_some(), "链接地址无效");
    Ok(url)
}

pub fn open_url(_ctx: &Ctx, input: &str) -> Result<()> {
    let url = check_url(input)?;
    let status = Command::new("/usr/bin/open")
        .arg(url.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("无法打开链接")?;
    ensure!(status.success(), "无法打开链接");
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GitToolsTarget {
    File,
    Dir,
    Missing,
}

pub fn git_tools_target(home: &Path) -> (GitToolsTarget, PathBuf) {
    let dir = home.join("plugins");
    let file = dir.join("git-tools.json");
    if file.is_file() {
        (GitToolsTarget::File, file)
    } else if dir.is_dir() {
        (GitToolsTarget::Dir, dir)
    } else {
        (GitToolsTarget::Missing, dir)
    }
}

pub fn reveal_git_tools_config(ctx: &Ctx) -> Result<GitToolsTarget> {
    let (target, path) = git_tools_target(&ctx.home);
    if target == GitToolsTarget::Missing {
        return Ok(target);
    }

    let mut open = Command::new("/usr/bin/open");
    if target == GitToolsTarget::File {
        open.arg("-R");
    }
    let status = open
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| anyhow!("无法在访达中显示"))?;
    ensure!(status.success(), "无法在访达中显示");
    Ok(target)
}
