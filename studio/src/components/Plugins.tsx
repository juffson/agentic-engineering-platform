import { useEffect, useRef, useState, type ReactNode } from "react";
import { GitBranch, Github, Gitlab } from "lucide-react";
import { api, errorMessage } from "../api/client";
import type { Api, ForgePluginId, PluginId, PluginInfo } from "../api/types";
import { relative } from "../lib/format";
import { badgeText, branchPluginRequired, defaultHost, errorText, forgeName, normalizeHost } from "../lib/remote";
import { useStudio } from "../state/store";
import { Button, Spinner } from "./ui";

const CARDS = [
  { id: "branches", name: "分支管理", Icon: GitBranch, description: "查看本地分支、工作区和提交图，用外部 git 工具打开工作区" },
  { id: "github", name: forgeName.github, Icon: Github, description: "在分支页显示 GitHub 远端分支、领先落后和 PR" },
  { id: "gitlab", name: forgeName.gitlab, Icon: Gitlab, description: "在分支页显示 GitLab 远端分支、领先落后和 MR" },
] as const;

export function PluginsPage() {
  const { plugins, pluginsError, reloadPlugins, pluginFocus, openPlugin, setPluginEnabled, pluginMutation } = useStudio();
  const [focusToken, setFocusToken] = useState<ForgePluginId | null>(null);

  function toggle(plugin: PluginInfo) {
    const enabled = !plugin.enabled;
    if (enabled && plugin.id !== "branches" && !plugin.token?.set) {
      setFocusToken(plugin.id);
      openPlugin(plugin.id);
    } else {
      setFocusToken(null);
    }
    void setPluginEnabled(plugin.id, enabled);
  }

  function toggleConfig(id: PluginId) {
    setFocusToken(null);
    openPlugin(pluginFocus === id ? null : id);
  }

  return (
    <div className="page">
      <div className="page-inner">
        <header className="page-head">
          <h1>插件</h1>
          <p>Studio 内置的扩展功能。只读取远端信息，不会推送或修改 PR/MR。</p>
        </header>
        {pluginsError && (
          <div className="plugin-read-error">
            <p className="inline-error" role="alert">读取插件状态失败：{pluginsError}</p>
            <Button size="sm" onClick={() => void reloadPlugins()}>重试</Button>
          </div>
        )}
        <div className="plugin-list" aria-busy={!plugins && !pluginsError}>
          {CARDS.map((card) => (
            <PluginCard
              key={card.id}
              card={card}
              plugin={plugins?.find((plugin) => plugin.id === card.id)}
              loading={!plugins && !pluginsError}
              expanded={pluginFocus === card.id}
              focusToken={focusToken === card.id}
              mutating={pluginMutation}
              onConfigure={() => toggleConfig(card.id)}
              onToggle={toggle}
            />
          ))}
        </div>
      </div>
    </div>
  );
}

function PluginCard({ card, plugin, loading, expanded, focusToken, mutating, onConfigure, onToggle }: {
  card: (typeof CARDS)[number];
  plugin?: PluginInfo;
  loading: boolean;
  expanded: boolean;
  focusToken: boolean;
  mutating: boolean;
  onConfigure(): void;
  onToggle(plugin: PluginInfo): void;
}) {
  const { id, name, Icon, description } = card;
  const badge = plugin?.badge;
  const unavailable = badge === "unavailable";
  const configId = `plugin-${id}-config`;
  let note: string | null = null;
  if (unavailable) note = branchPluginRequired;
  if (badge === "needs_config") note = "填写访问令牌后才能读取远端信息";
  if (badge === "error" && plugin?.error && id !== "branches") {
    const error = plugin.error;
    const message = error.kind === "auth" ? "令牌无效或过期" : errorText(error, id);
    note = `${message} · ${relative(new Date(error.at * 1000).toISOString())}`;
  }

  return (
    <section className="plugin-card" aria-labelledby={`plugin-${id}-title`}>
      <div className="plugin-card-head">
        <h2 id={`plugin-${id}-title`}><Icon size={17} aria-hidden="true" />{name}</h2>
        <div className="plugin-card-actions">
          {badge && <span className={`plugin-badge plugin-badge-${badge}`}>{badgeText[badge]}</span>}
          {loading && <span className="plugin-loading-slot"><Spinner size={12} /></span>}
          <Button size="sm" variant="ghost" aria-expanded={!!plugin && expanded} aria-controls={expanded && plugin ? configId : undefined} disabled={!plugin} onClick={onConfigure}>
            配置
          </Button>
          {loading ? <span className="plugin-loading-slot"><Spinner size={12} /></span> : (
            <span title={unavailable ? branchPluginRequired : undefined}>
              <button
                type="button"
                className="plugin-switch"
                role="switch"
                aria-label={`启用 ${name}`}
                aria-checked={plugin?.enabled ?? false}
                disabled={mutating || unavailable || !plugin}
                onClick={() => plugin && onToggle(plugin)}
              />
            </span>
          )}
        </div>
      </div>
      <p className="plugin-description">{description}{plugin?.host && <> · {plugin.host.replace(/^https:\/\//, "")}</>}</p>
      {note && <p className={`plugin-note${badge === "error" ? " inline-error" : " muted"}`}>{note}</p>}
      {expanded && plugin && (
        <div className="plugin-config" id={configId}>
          {id === "branches" ? <BranchToolsConfig /> : <ForgeConfig plugin={plugin} id={id} focusToken={focusToken} />}
        </div>
      )}
    </section>
  );
}

function ForgeConfig({ plugin, id, focusToken }: { plugin: PluginInfo; id: ForgePluginId; focusToken: boolean }) {
  const { pluginMutation, mutatePlugins, reloadPlugins, notify } = useStudio();
  const name = forgeName[id];
  const savedHost = plugin.host ?? `https://${defaultHost[id]}`;
  const [hostDraft, setHostDraft] = useState<string | null>(null);
  const host = hostDraft ?? (savedHost === `https://${defaultHost[id]}` ? defaultHost[id] : savedHost);
  const [hostChecked, setHostChecked] = useState(false);
  const [replacing, setReplacing] = useState(false);
  const [token, setToken] = useState("");
  const tokenInput = useRef<HTMLInputElement>(null);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<{ ok: boolean; message: string } | null>(null);
  const testVersion = useRef(0);
  const [writing, setWriting] = useState<"save" | "clear" | null>(null);
  const [writeError, setWriteError] = useState<string | null>(null);

  const normalized = normalizeHost(host, id);
  const tokenSet = plugin.token?.set ?? false;
  const hostChanged = !!normalized.url && normalized.url !== savedHost;
  const showToken = !tokenSet || hostChanged || replacing || token.length > 0;
  const draftToken = token.trim() || null;
  const canTest = !!draftToken || (tokenSet && !hostChanged);
  const changed = hostChanged || !!draftToken;
  const hostHint = hostChecked && normalized.error
    ? normalized.error
    : !host.trim() ? `留空将使用 ${defaultHost[id]}`
    : hostChecked && normalized.url !== host ? `将保存为 ${normalized.url}` : null;

  useEffect(() => {
    if ((focusToken || replacing) && showToken) tokenInput.current?.focus();
  }, [focusToken, replacing, showToken]);

  function clearTest() {
    ++testVersion.current;
    setTestResult(null);
  }

  function editToken(value: string) {
    setToken(value);
    setWriteError(null);
    clearTest();
  }

  async function testConnection() {
    setHostChecked(true);
    if (!normalized.url || !canTest || testing || pluginMutation) return;
    const version = ++testVersion.current;
    setTestResult(null);
    setTesting(true);
    try {
      const result = await api.testPlugin({ plugin: id, host: normalized.url, token: draftToken });
      if (version === testVersion.current) {
        setTestResult(result.ok
          ? { ok: true, message: `✓ 已连接为 ${result.account}` }
          : { ok: false, message: errorText(result.error, id) });
      }
      if (!draftToken) void reloadPlugins();
    } catch (e) {
      if (version === testVersion.current) setTestResult({ ok: false, message: `测试连接失败：${safeError(e, draftToken)}` });
    } finally {
      setTesting(false);
    }
  }

  async function save() {
    setHostChecked(true);
    if (!normalized.url || !changed || pluginMutation || testing) return;
    const hostUrl = normalized.url;
    setWriteError(null);
    setWriting("save");
    try {
      const result = await mutatePlugins(() => api.savePlugin({ plugin: id, host: hostUrl, token: draftToken }));
      if (!result) return;
      setHostDraft(null);
      setHostChecked(false);
      setReplacing(false);
      editToken("");
      notify(`${name} 配置已保存`, "info");
    } catch (e) {
      setWriteError(`保存失败：${safeError(e, draftToken)}`);
    } finally {
      setWriting(null);
    }
  }

  async function clearToken() {
    if (pluginMutation || testing) return;
    const info = id === "github" ? "PR" : "MR";
    if (!window.confirm(`清除 ${name} 的访问令牌？分支页将不再显示 ${info} 信息。`)) return;
    setWriteError(null);
    setWriting("clear");
    try {
      const result = await mutatePlugins(() => api.clearPluginToken({ plugin: id }));
      if (!result) return;
      setReplacing(false);
      editToken("");
      notify("令牌已清除", "info");
    } catch (e) {
      setWriteError(`清除令牌失败：${safeError(e, draftToken)}`);
    } finally {
      setWriting(null);
    }
  }

  return (
    <>
      <div className="field">
        <label className="field-label" htmlFor={`${id}-host`}>主机地址</label>
        <input
          className="input"
          id={`${id}-host`}
          value={host}
          disabled={!!writing}
          spellCheck={false}
          autoComplete="off"
          aria-invalid={hostChecked && !!normalized.error}
          aria-describedby={hostHint ? `${id}-host-hint` : undefined}
          onChange={(e) => {
            setHostDraft(e.target.value);
            setHostChecked(false);
            setWriteError(null);
            clearTest();
          }}
          onBlur={() => setHostChecked(true)}
        />
        {hostHint && <span id={`${id}-host-hint`} className={normalized.error ? "inline-error" : "field-hint"}>{hostHint}</span>}
        {id === "github" && hostChecked && normalized.url && new URL(normalized.url).hostname !== defaultHost.github && (
          <span className="field-hint">GitHub Enterprise 会使用 <code>{normalized.url}/api/v3</code>。</span>
        )}
      </div>
      <div className="field">
        <label className="field-label" htmlFor={showToken ? `${id}-token` : undefined}>访问令牌</label>
        {showToken ? (
          <div className="plugin-token-row">
            <input
              className="input"
              id={`${id}-token`}
              ref={tokenInput}
              type="password"
              autoComplete="off"
              spellCheck={false}
              aria-describedby={`${id}-token-hint`}
              value={token}
              disabled={!!writing}
              onChange={(e) => editToken(e.target.value)}
            />
            {tokenSet && !hostChanged && <Button size="sm" disabled={!!writing} onClick={() => { setReplacing(false); editToken(""); }}>取消</Button>}
          </div>
        ) : (
          <div className="plugin-token-row">
            <span>已设置 · <code>••••{plugin.token?.hint}</code></span>
            <Button size="sm" onClick={() => setReplacing(true)} disabled={!!writing}>更换</Button>
            <Button size="sm" onClick={() => void clearToken()} disabled={pluginMutation || testing} busy={writing === "clear"}>清除令牌</Button>
          </div>
        )}
        <span className="field-hint" id={`${id}-token-hint`}>
          {hostChanged && <>主机已更改，请为新主机重新输入访问令牌。保存时留空将清除旧令牌。<br /></>}
          {id === "github" ? <>需要仓库读取和 Pull requests 读取权限，classic 令牌使用 <code>repo</code>。</> : <>需要 <code>read_api</code> 权限。</>}
          令牌存放在系统钥匙串，不写入任何文件。
        </span>
      </div>
      <div className="plugin-config-actions">
        <span title={!canTest ? "先填写访问令牌" : undefined}>
          <Button size="sm" busy={testing} aria-busy={testing} disabled={!normalized.url || !canTest || pluginMutation} onClick={() => void testConnection()}>
            {testing ? "测试中…" : "测试连接"}
          </Button>
        </span>
        <Button size="sm" variant="primary" busy={writing === "save"} disabled={!normalized.url || !changed || pluginMutation || testing} onClick={() => void save()}>
          保存
        </Button>
        {testResult && <span role="status" className={testResult.ok ? "plugin-test-ok" : "inline-error"}>{testResult.message}</span>}
      </div>
      {writeError && <p className="inline-error plugin-note" role="alert">{writeError}</p>}
    </>
  );
}

function safeError(error: unknown, token: string | null) {
  const message = errorMessage(error);
  return token ? message.replaceAll(token, "••••") : message;
}

function BranchToolsConfig() {
  const [data, setData] = useState<ReturnType<Api["gitTools"]> | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [revealError, setRevealError] = useState<string | null>(null);
  const [configMissing, setConfigMissing] = useState(false);

  useEffect(() => {
    let active = true;
    api.gitTools().then((tools) => {
      if (active) setData(tools);
    }).catch((e) => {
      if (active) setError(`读取外部工具失败：${errorMessage(e)}`);
    });
    return () => { active = false; };
  }, []);

  async function revealConfig() {
    setRevealError(null);
    setConfigMissing(false);
    try {
      const target = await api.revealGitToolsConfig();
      setConfigMissing(target !== "file");
    } catch (e) {
      setRevealError(errorMessage(e));
    }
  }

  let tools: ReactNode = <Spinner />;
  if (error) tools = <p className="inline-error" role="alert">{error}</p>;
  if (data) {
    tools = data.tools.length === 0 ? "没有检测到已安装的工具" : (
      <div className="plugin-token-row">
        已检测到：{data.tools.map((tool, index) => (
          <span key={tool.id}>
            {index > 0 && "· "}{tool.label}{tool.id.startsWith("custom:") && <> <span className="plugin-badge">自定义</span></>}
          </span>
        ))}
      </div>
    );
  }

  return (
    <div className="plugin-tools">
      <h3>外部 git 工具</h3>
      <div>{tools}</div>
      {data && (
        <>
          <div className="plugin-token-row">
            <span>自定义工具：<code>{data.config}</code></span>
            <Button size="sm" title="在访达中选中 git-tools.json" onClick={() => void revealConfig()}>在访达中显示</Button>
            {configMissing && <span className="field-hint" role="status">文件还不存在，按上面格式新建即可</span>}
          </div>
          <p className="field-hint">格式：<code>{`[{"label":"Fork","command":["open","-a","Fork","{path}"]}]`}</code></p>
          {revealError && <p className="inline-error" role="alert">{revealError}</p>}
        </>
      )}
    </div>
  );
}
