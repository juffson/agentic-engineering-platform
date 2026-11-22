import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, rm } from "node:fs/promises";
import { once } from "node:events";
import { createServer } from "node:net";
import { join } from "node:path";
import { randomUUID } from "node:crypto";
import { createInterface } from "node:readline";

test("dispatches to the selected local project, passing the model and stable request ID", async t => {
  const root = await mkdtemp("/tmp/aep-ext-");
  const previousHome = process.env.HOME;
  process.env.HOME = root;
  const dir = join(root, ".aep", "zcode");
  await mkdir(dir, { recursive: true, mode: 0o700 });
  const ext = await import("../aep-studio/extension.js");
  const local = { kind: "app", sourceServerId: "local", projectId: "aep" };
  const calls = [];
  let permission = true;
  ext.activate({ sessions: {
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

  async function dispatch(projectId) {
    const requestId = randomUUID().replaceAll("-", "");
    const server = createServer();
    server.listen(join(dir, requestId + ".sock"));
    try {
      const [socket] = await once(server, "connection", {signal: AbortSignal.timeout(5000)});
      socket.write(JSON.stringify({ requestId, projectId, providerId: "account:bigmodel-start-plan",
        modelId: "GLM-5.3-Flash", prompt: "first round\nshared handoff" }) + "\n");
      const reader = createInterface({ input: socket });
      const [line] = await once(reader, "line", {signal: AbortSignal.timeout(5000)});
      socket.destroy();
      return { requestId, reply: JSON.parse(line) };
    } finally { server.close(); }
  }

  const accepted = await dispatch("aep");
  assert.equal(accepted.reply.firstInput, "accepted");
  assert.deepEqual(calls[0], { requestId: accepted.requestId, project: local,
    prompt: "first round\nshared handoff", model: "account:bigmodel-start-plan/GLM-5.3-Flash" });
  assert.match((await dispatch("unknown")).reply.error, /未出现/);
  permission = false;
  assert.equal((await dispatch("aep")).reply.code, "not-granted");
  assert.equal(calls.length, 1);
});
