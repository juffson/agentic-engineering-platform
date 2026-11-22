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

async function serve(ctx, name, id) {
  const socket = createConnection(join(folder, name));
  active.set(name, socket);
  try {
    const lines = createInterface({ input: socket, crlfDelay: Infinity });
    for await (const line of lines) {
      const job = JSON.parse(line);
      if (job.requestId !== id || !plans.has(job.providerId) ||
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
