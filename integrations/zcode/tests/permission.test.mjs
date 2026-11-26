import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const hooks = join(dirname(fileURLToPath(import.meta.url)), "..", "aep-studio", "hooks");
const { decide } = await import(join(hooks, "permission.mjs"));

async function grant(home, workspace, pid) {
  await mkdir(join(home, "grants"), { recursive: true });
  const name = createHash("sha256").update(workspace).digest("hex") + ".json";
  await writeFile(join(home, "grants", name), JSON.stringify({ workspace, run: "/runs/r1", pid }));
}

test("approves only a workspace with a live 完全权限 grant", async () => {
  const home = await mkdtemp("/tmp/aep-hook-");
  try {
    const live = "/w/AEP Studio/worktrees/live";
    const stale = "/w/AEP Studio/worktrees/stale";
    await grant(home, live, process.pid);
    await grant(home, stale, 2 ** 22 + 12345);
    assert.equal(decide({ cwd: live }, home).allow, true);
    assert.equal(decide({ workingDirectory: live }, home).allow, true);
    assert.deepEqual(decide({ cwd: stale }, home), { allow: false, why: "stale-grant", workspace: stale });
    assert.equal(decide({ cwd: "/w/other" }, home).why, "no-grant");
    // A tool's own path never selects a grant.
    assert.equal(decide({ tool_input: { file_path: live + "/a" } }, home).allow, false);
  } finally {
    await rm(home, { recursive: true, force: true });
  }
});

test("the hook command answers ZCode only when it approves", async () => {
  const home = await mkdtemp("/tmp/aep-hook-home-");
  try {
    const workspace = join(home, "work");
    await mkdir(workspace);
    await grant(join(home, ".aep", "zcode"), workspace, process.pid);
    const run = cwd => spawnSync("/bin/sh", [join(hooks, "permission.sh")], {
      input: JSON.stringify({ hook_event_name: "PermissionRequest", session_id: "s1", tool_name: "Bash", cwd }),
      env: { ...process.env, HOME: home, ZCODE_PROJECT_DIR: "" }, encoding: "utf8"
    });
    const approved = run(workspace);
    assert.equal(approved.status, 0, approved.stderr);
    assert.deepEqual(JSON.parse(approved.stdout), {
      hookSpecificOutput: { hookEventName: "PermissionRequest", decision: { behavior: "allow" } }
    });
    const other = run(join(home, "elsewhere"));
    assert.equal(other.status, 0, other.stderr);
    assert.equal(other.stdout, "");
  } finally {
    await rm(home, { recursive: true, force: true });
  }
});
