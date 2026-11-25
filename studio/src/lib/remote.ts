import type { ForgePluginId, PluginBadge, PluginInfo, Pull, PullState, RemoteBranchComparison, RemoteError, RemoteStatus } from "../api/types";

export const forgeName: Record<ForgePluginId, string> = { github: "GitHub", gitlab: "GitLab" };
export const defaultHost: Record<ForgePluginId, string> = { github: "github.com", gitlab: "gitlab.com" };
export const branchPluginRequired = "需要先启用分支管理";

export const badgeText: Record<PluginBadge, string> = {
  enabled: "已启用",
  disabled: "已停用",
  needs_config: "需要配置",
  error: "出错",
  unavailable: "不可用",
};

export function pluginBadge(plugin: PluginInfo, branchesOn: boolean): PluginBadge {
  if (plugin.id !== "branches" && !branchesOn) return "unavailable";
  if (!plugin.enabled) return "disabled";
  if (plugin.id !== "branches" && !plugin.token?.set) return "needs_config";
  return plugin.error ? "error" : "enabled";
}

export function errorText(error: RemoteError, plugin: ForgePluginId | null, path?: string): string {
  const { host, status, reset_at: resetAt } = error;
  switch (error.kind) {
    case "unreachable": return `无法连接 ${host}，请检查地址和网络`;
    case "auth": return plugin
      ? `令牌无效或过期，请在 ${forgeName[plugin]} 重新生成后更换`
      : "令牌无效或过期，请重新生成后更换";
    case "forbidden": return "令牌权限不足：GitHub 需要仓库读取和 Pull requests 读取；GitLab 需要 read_api";
    case "cert": return `${host} 的 HTTPS 证书不受信任（暂不支持自签名证书）`;
    case "rate_limited": {
      const time = resetAt === null ? null : new Date(resetAt * 1000).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit", hourCycle: "h23" });
      return time ? `请求过多已被限流，约 ${time} 后恢复` : "请求过多已被限流，请稍后重试";
    }
    case "timeout": return `${host} 15 秒内没有响应`;
    case "not_found": return `在 ${host} 上找不到${path ? ` ${path}` : "仓库"}，或令牌无权访问`;
    case "other": return `${host} 返回${status === null ? "错误" : ` ${status}`}：${error.message.slice(0, 120)}`;
  }
}

type NormalizedHost = { url: string; error?: never } | { url?: never; error: string };

/** Match the backend's HTTPS origin rules for field feedback. The backend validates again. */
export function normalizeHost(input: string, plugin: ForgePluginId): NormalizedHost {
  const value = input.trim() || defaultHost[plugin];
  const separator = value.indexOf("://");
  if (separator !== -1 && value.slice(0, separator).toLowerCase() !== "https") {
    return { error: "只支持 https 地址" };
  }
  const authority = (separator === -1 ? value : value.slice(separator + 3)).replace(/\/+$/, "");
  if (!authority || /[/\\?#@\p{White_Space}\p{Cc}]/u.test(authority)) return { error: "主机地址无效" };
  try {
    const url = new URL(`https://${authority}`);
    const labels = url.hostname.replace(/\.$/, "").split(".");
    if (!url.hostname.startsWith("[") && !labels.every((label) => /^[a-z\d](?:[a-z\d-]*[a-z\d])?$/i.test(label))) {
      return { error: "主机地址无效" };
    }
    return { url: url.origin };
  } catch {
    return { error: "主机地址无效" };
  }
}

const pullStateText: Record<PullState, string> = { open: "打开", draft: "草稿", merged: "已合并", closed: "已关闭" };

export function pullLabel(pull: Pull, plugin: ForgePluginId | null): string {
  return `${plugin === "gitlab" ? "!" : "#"}${pull.number} ${pullStateText[pull.state]}`;
}

export function upstreamText(branch: RemoteBranchComparison): string {
  const { upstream, present, ahead, behind, same_name: sameName } = branch;
  if (!upstream) return sameName ? `未跟踪 · 远端有同名分支 ${sameName}` : "仅本地";
  if (!present || ahead === null || behind === null) return `相对 ${upstream} 未知 · 刷新远端后再看`;
  if (ahead === 0 && behind === 0) return `与 ${upstream} 一致`;
  return `↑${ahead} ↓${behind}（相对 ${upstream}）`;
}

export function remoteFor(status: RemoteStatus | null, branchName: string) {
  return status?.active ? status.branches.find((branch) => branch.name === branchName) ?? null : null;
}
