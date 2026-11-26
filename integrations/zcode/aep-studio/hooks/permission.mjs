// ZCode PermissionRequest hook: approve a tool call only while AEP runs a 完全权限 stage in
// the task's workspace. AEP writes ~/.aep/zcode/grants/<sha256(workspace)>.json for the
// stage's lifetime; any other task, or a stale grant, gets no answer and ZCode asks as usual.
import { createHash } from "node:crypto";
import { appendFileSync, mkdirSync, readFileSync, realpathSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

const root = join(homedir(), ".aep", "zcode");

function alive(pid) {
  if (!Number.isInteger(pid) || pid <= 0) return false;
  try { process.kill(pid, 0); return true; } catch (error) { return error.code === "EPERM"; }
}

function canonical(path) {
  try { return realpathSync(path); } catch { return path; }
}

// The task's directory as ZCode reports it. Tool inputs are never used to pick the grant.
function workspaceOf(input) {
  for (const key of ["cwd", "workingDirectory", "workspacePath", "workspace_path", "projectDir"]) {
    if (typeof input[key] === "string" && input[key]) return input[key];
  }
  if (typeof process.env.ZCODE_PROJECT_DIR === "string" && process.env.ZCODE_PROJECT_DIR) {
    return process.env.ZCODE_PROJECT_DIR;
  }
  return null;
}

export function decide(input, home = root) {
  const workspace = workspaceOf(input);
  if (!workspace) return { allow: false, why: "no-workspace" };
  for (const candidate of new Set([workspace, canonical(workspace)])) {
    const name = createHash("sha256").update(candidate).digest("hex") + ".json";
    let grant;
    try { grant = JSON.parse(readFileSync(join(home, "grants", name), "utf8")); } catch { continue; }
    if (grant.workspace !== candidate) continue;
    if (!alive(grant.pid)) return { allow: false, why: "stale-grant", workspace: candidate };
    return { allow: true, why: "full-access-stage", workspace: candidate, run: grant.run };
  }
  return { allow: false, why: "no-grant", workspace };
}

function log(entry) {
  try {
    mkdirSync(root, { recursive: true, mode: 0o700 });
    appendFileSync(join(root, "permission-hook.log"), JSON.stringify({ at: new Date().toISOString(), ...entry }) + "\n");
  } catch {}
}

async function main() {
  let text = "";
  for await (const chunk of process.stdin) text += chunk;
  let input;
  try { input = JSON.parse(text); } catch { log({ why: "bad-input" }); return; }
  const result = decide(input);
  log({ ...result, session: input.session_id, tool: input.tool_name, keys: Object.keys(input).sort() });
  if (result.allow) {
    process.stdout.write(JSON.stringify({
      hookSpecificOutput: { hookEventName: "PermissionRequest", decision: { behavior: "allow" } }
    }) + "\n");
  }
}

if (process.argv[1] && import.meta.url === new URL("file://" + process.argv[1]).href) await main();
