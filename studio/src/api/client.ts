// Typed client for the Studio command contract in ./types.ts.
// Inside Tauri every method maps 1:1 to `invoke(<method name>, args)`;
// in a plain browser (dev server, screenshot tests) it talks to the in-memory mock.
import { invoke } from "@tauri-apps/api/core";
import type { Api } from "./types";

type Args<K extends keyof Api> = Parameters<Api[K]>;
type Result<K extends keyof Api> = ReturnType<Api[K]>;

export type AsyncApi = {
  [K in keyof Api]: (...args: Args<K>) => Promise<Result<K>>;
};

export const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

function message(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  if (error && typeof error === "object" && "message" in error) {
    return String((error as { message: unknown }).message);
  }
  return String(error);
}

let mockApi: Promise<Api> | null = null;
function loadMock(): Promise<Api> {
  if (!mockApi) mockApi = import("./mock").then((m) => m.createMockApi());
  return mockApi;
}

async function call(name: keyof Api, args: unknown): Promise<unknown> {
  try {
    if (inTauri) {
      return await invoke(name, (args ?? {}) as Record<string, unknown>);
    }
    const mock = await loadMock();
    // A small delay keeps loading states honest in the browser preview.
    await new Promise((resolve) => setTimeout(resolve, name === "zcodeModels" ? 900 : 40));
    const fn = mock[name] as (a: unknown) => unknown;
    return fn.call(mock, args);
  } catch (error) {
    throw message(error);
  }
}

export const api = new Proxy({} as AsyncApi, {
  get(_target, name: string) {
    return (args?: unknown) => call(name as keyof Api, args);
  },
});

export { message as errorMessage };
