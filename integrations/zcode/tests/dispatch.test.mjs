import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, rm } from "node:fs/promises";
import { once } from "node:events";
import { createServer } from "node:net";
import { join } from "node:path";
import { randomUUID } from "node:crypto";
import { createInterface } from "node:readline";

test("AEP desktop extension protocol", async t => {
  const root = await mkdtemp("/tmp/aep-ext-");
  const previousHome = process.env.HOME;
  process.env.HOME = root;
  const dir = join(root, ".aep", "zcode");
  await mkdir(dir, { recursive: true, mode: 0o700 });
  const ext = await import("../aep-studio/extension.js");
  const local = { kind: "app", sourceServerId: "local", projectId: "aep" };
  const calls = [];
  let permission = true;
  let waiting = true;
  const session = { ...local, workspacePath: "/work/aep", sessionId: "test-session" };
  const operation = { commandId: "command-1", sessionId: session.sessionId, session };
  const opened = [];
  const pending = { kind: "toolCall", turnId: "first", status: "pendingApproval" };
  ext.activate({ sessions: {
    async openSession(input) {
      opened.push(input);
      return { status: "opened" };
    },
    async waitSessions(input) {
      assert.deepEqual(input.operations, [operation]);
      return { operations: [{ operation, attention: waiting ? { kind: "approval" } : undefined }] };
    },
    async readRows({ sessionId, beforeRowId }) {
      assert.equal(sessionId, session.sessionId);
      if (beforeRowId === undefined) return { hasMore: true, rows: [
        { ...pending, rowId: 40, toolCallId: "a", toolName: "Bash", input: { command: "git status" } },
        { ...pending, rowId: 41, toolCallId: "b", toolName: "Read", input: { file_path: "src/lib.rs" } },
        { ...pending, rowId: 42, turnId: "other", toolCallId: "other" },
        { ...pending, rowId: 43, status: "running", toolCallId: "running" }
      ] };
      assert.equal(beforeRowId, 40);
      return { hasMore: false, rows: [
        { rowId: 1, kind: "turnHeader", sourceCommandId: operation.commandId, turnId: "first" }
      ] };
    },
    async listProjects() {
      if (!permission) throw Object.assign(new Error("permission required"), { code: "not-granted" });
      return { projects: [
        { project: { ...local, sourceServerId: "remote" } }, { project: local }
      ] };
    },
    async createSession(input) {
      calls.push(input);
      return { requestId: input.requestId, creation: "created", firstInput: "accepted",
        session: { sessionId: "test-session", project: input.project } };
    }
  } });
  t.after(async () => {
    ext.deactivate();
    if (previousHome === undefined) delete process.env.HOME;
    else process.env.HOME = previousHome;
    await rm(root, { recursive: true, force: true });
  });

  async function dispatch(projectId, action) {
    const requestId = randomUUID().replaceAll("-", "");
    const server = createServer();
    server.listen(join(dir, requestId + ".sock"));
    try {
      const [socket] = await once(server, "connection", {signal: AbortSignal.timeout(5000)});
      socket.write(JSON.stringify({ requestId, projectId, providerId: "account:bigmodel-start-plan",
        modelId: "GLM-5.3-Flash", prompt: "first round\nshared handoff", ...action }) + "\n");
      const reader = createInterface({ input: socket });
      const [line] = await once(reader, "line", {signal: AbortSignal.timeout(5000)});
      socket.destroy();
      return { requestId, reply: JSON.parse(line) };
    } finally { server.close(); }
  }

  const request = { action: "approvals", session, operation };
  await t.test("dispatches the chosen model to the selected project and preserves its request ID", async () => {
    const accepted = await dispatch("aep");
    assert.equal(accepted.reply.firstInput, "accepted");
    assert.deepEqual(calls[0], { requestId: accepted.requestId, project: local,
      prompt: "first round\nshared handoff", model: "account:bigmodel-start-plan/GLM-5.3-Flash" });
    assert.match((await dispatch("unknown")).reply.error, /未出现/);
    permission = false;
    assert.equal((await dispatch("aep")).reply.code, "not-granted");
    assert.equal(calls.length, 1);
  });
  await t.test("reports only pending tools from the admitted turn and clears them after approval", async () => {
    const observed = await dispatch("aep", request);
    assert.equal(observed.reply.waiting, true);
    assert.deepEqual(observed.reply.tools.map(t => t.detail).sort(), ["git status", "src/lib.rs"]);
    waiting = false;
    const cleared = await dispatch("aep", request);
    assert.equal(cleared.reply.waiting, false);
    assert.deepEqual(cleared.reply.tools, []);
  });
  await t.test("opens the exact session and rejects a mismatched operation", async () => {
    const open = await dispatch("aep", { ...request, action: "open" });
    assert.equal(open.reply.requestId, open.requestId);
    assert.equal(open.reply.sessionId, session.sessionId);
    assert.deepEqual(opened, [{ session }]);
    const wrongSession = { ...session, sessionId: "another-session" };
    assert.match((await dispatch("aep", { ...request, action: "open", session: wrongSession })).reply.error, /Invalid desktop session/);
    assert.equal(opened.length, 1);
  });
});
