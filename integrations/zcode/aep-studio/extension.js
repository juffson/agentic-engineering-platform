import { readdir } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import { createConnection } from "node:net";
import { createInterface } from "node:readline";

const folder = join(homedir(), ".aep", "zcode");
const socketName = /^([0-9a-f]{32})\.sock$/;
const plans = new Set(["account:bigmodel-start-plan", "account:bigmodel-individual-coding-plan"]);
const active = new Map();
let timer;

function describe(row) {
  const input = row.input ?? {};
  const detail = [input.command, input.file_path, input.path, row.inputText]
    .find(value => typeof value === "string" && value.trim()) ?? "";
  return { tool: row.toolName, detail, id: row.toolCallId };
}

// Query only the operation admitted for this AEP attempt. A later user turn in
// the same desktop session must never supply this card's commands.
async function approvalState(ctx, job) {
  const result = await ctx.sessions.waitSessions({
    operations: [job.operation], mode: "any", timeoutMs: 0
  });
  const current = result.operations.find(op => op.operation.commandId === job.operation.commandId);
  if (!current) throw new Error("ZCode 未返回该操作的状态");
  if (current.attention?.kind !== "approval") return { waiting: false, tools: [] };

  const pending = [];
  let beforeRowId;
  for (;;) {
    const page = await ctx.sessions.readRows({
      sessionId: job.session.sessionId, limit: 32, ...(beforeRowId === undefined ? {} : { beforeRowId })
    });
    for (const row of [...page.rows].reverse()) {
      if (row.kind === "toolCall" && row.status === "pendingApproval") pending.push(row);
      if (row.sourceCommandId === job.operation.commandId && row.kind === "turnHeader") {
        return { waiting: true, tools: pending.filter(item => item.turnId === row.turnId).map(describe) };
      }
    }
    const first = page.rows[0]?.rowId;
    if (!page.hasMore || first === undefined || first === beforeRowId) break;
    beforeRowId = first;
  }
  return { waiting: true, tools: [] };
}

async function serve(ctx, name, id) {
  const socket = createConnection(join(folder, name));
  active.set(name, socket);
  try {
    const lines = createInterface({ input: socket, crlfDelay: Infinity });
    for await (const line of lines) {
      const job = JSON.parse(line);
      if (job.requestId !== id) throw new Error("Invalid request ID");
      if (job.action === "projectSessions") {
        const { projects } = await ctx.sessions.listProjects({});
        const project = projects.find(row => row.project.sourceServerId === "local" &&
          row.project.projectId === job.projectId)?.project;
        if (!project || !Array.isArray(job.sessions)) throw new Error("Invalid project verification");
        const wanted = new Set(job.sessions);
        const found = [];
        let cursor;
        do {
          const page = await ctx.sessions.listSessions({ project, archived: "include", limit: 50, ...(cursor ? { cursor } : {}) });
          for (const row of page.sessions) {
            if (wanted.delete(row.session.sessionId)) found.push(row.session);
          }
          if (page.nextCursor && page.nextCursor === cursor) throw new Error("Invalid project sessions cursor");
          cursor = page.nextCursor;
        } while (cursor && wanted.size);
        socket.end(JSON.stringify({ requestId: id, projectId: project.projectId, sessions: found }) + "\n");
        return;
      }
      if (job.action === "open" || job.action === "approvals") {
        const session = job.session;
        if (session?.sourceServerId !== "local" || !session.sessionId ||
            session.sessionId !== job.operation?.sessionId) throw new Error("Invalid desktop session");
        const reply = job.action === "open" ? await ctx.sessions.openSession({ session })
          : await approvalState(ctx, job);
        socket.end(JSON.stringify({ ...reply, sessionId: session.sessionId, requestId: id }) + "\n");
        return;
      }
      if (!plans.has(job.providerId) ||
          typeof job.modelId !== "string" || !job.modelId || typeof job.prompt !== "string") {
        throw new Error("Invalid AEP dispatch");
      }
      const { projects } = await ctx.sessions.listProjects({});
      const project = projects.find(row => row.project.kind === "app" &&
        row.project.sourceServerId === "local" && row.project.projectId === job.projectId)?.project;
      if (!project) throw new Error("AEP 项目未出现在 ZCode 本机项目列表");
      if (socket.destroyed) return;
      const reply = await ctx.sessions.createSession({
        project, prompt: job.prompt, model: job.providerId + "/" + job.modelId,
        requestId: id
      });
      socket.end(JSON.stringify(reply) + "\n");
      return;
    }
  } catch (error) {
    if (!socket.destroyed) socket.end(JSON.stringify({
      error: String(error.message), code: String(error.code ?? "extension-error")
    }) + "\n");
  } finally {
    active.delete(name);
  }
}

export function activate(ctx) {
  async function scan() {
    let names;
    try { names = await readdir(folder); } catch { return; }
    for (const name of names) {
      const match = socketName.exec(name);
      if (match && !active.has(name)) void serve(ctx, name, match[1]).catch(() => {});
    }
  }
  timer = setInterval(() => void scan(), 600);
  void scan();
}

export function deactivate() {
  clearInterval(timer);
  for (const socket of active.values()) socket.destroy();
  active.clear();
}
