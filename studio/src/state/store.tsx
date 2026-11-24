import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { api, errorMessage } from "../api/client";
import type { ProjectSummary, Snapshot, ZCodeModel } from "../api/types";

export type Page = "work" | "memory" | "repos" | "branches";

export interface ZCodeState {
  models: ZCodeModel[] | null;
  loading: boolean;
  error: string | null;
}

export interface Toast {
  id: number;
  text: string;
  tone: "error" | "info";
}

interface StudioState {
  snapshot: Snapshot | null;
  project: ProjectSummary | null;
  selected: string | null;
  page: Page;
  setPage(page: Page): void;
  selectWork(id: string): void;
  openProject(id: string): void;
  /** Bumped after every mutation so pollers refetch immediately. */
  revision: number;
  refresh(): void;
  /** Run a mutation; errors become a toast and resolve to undefined. */
  act<T>(fn: () => Promise<T>, success?: string): Promise<T | undefined>;
  toasts: Toast[];
  notify(text: string, tone?: Toast["tone"]): void;
  dismiss(id: number): void;
  /** ZCode's model list, loaded lazily the first time a ZCode stage is shown. */
  zcode: ZCodeState;
  loadZcode(refresh?: boolean): void;
}

const Ctx = createContext<StudioState | null>(null);

export function useStudio(): StudioState {
  const value = useContext(Ctx);
  if (!value) throw new Error("useStudio outside StudioProvider");
  return value;
}

/** Calls `fn` now and every `ms`, skipping overlapping ticks. */
export function usePoll(fn: () => Promise<void> | void, ms: number, deps: unknown[]) {
  const ref = useRef(fn);
  ref.current = fn;
  useEffect(() => {
    let alive = true;
    let busy = false;
    const tick = async () => {
      if (busy || !alive) return;
      busy = true;
      try {
        await ref.current();
      } finally {
        busy = false;
      }
    };
    void tick();
    const timer = window.setInterval(tick, ms);
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ms, ...deps]);
}

export function StudioProvider({ children }: { children: ReactNode }) {
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [page, setPage] = useState<Page>("work");
  const [revision, setRevision] = useState(0);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const toastSeq = useRef(0);
  const snapshotError = useRef<string | null>(null);
  const [zcode, setZcode] = useState<ZCodeState>({ models: null, loading: false, error: null });
  const zcodeBusy = useRef(false);
  const zcodeTried = useRef(false);
  const loadZcode = useCallback((refresh = false) => {
    if (zcodeBusy.current || (!refresh && zcodeTried.current)) return;
    zcodeBusy.current = true;
    zcodeTried.current = true;
    setZcode((z) => ({ ...z, loading: true, error: null }));
    api
      .zcodeModels(refresh ? { refresh: true } : {})
      .then((models) => setZcode({ models, loading: false, error: null }))
      .catch((e) => setZcode((z) => ({ ...z, loading: false, error: errorMessage(e) })))
      .finally(() => {
        zcodeBusy.current = false;
      });
  }, []);

  const dismiss = useCallback((id: number) => setToasts((t) => t.filter((x) => x.id !== id)), []);
  const notify = useCallback(
    (text: string, tone: Toast["tone"] = "error") => {
      const id = ++toastSeq.current;
      setToasts((t) => [...t.filter((x) => x.text !== text), { id, text, tone }].slice(-4));
      if (tone === "info") window.setTimeout(() => dismiss(id), 3200);
    },
    [dismiss],
  );

  const loadSnapshot = useCallback(async () => {
    try {
      const s = await api.snapshot();
      snapshotError.current = null;
      setSnapshot(s);
      setSelected((cur) => {
        const project = s.projects.find((p) => p.id === s.active);
        if (cur && project?.work.some((w) => w.id === cur)) return cur;
        return s.selected ?? project?.work[0]?.id ?? null;
      });
    } catch (e) {
      const text = errorMessage(e);
      if (snapshotError.current !== text) notify(`无法读取工作台状态：${text}`);
      snapshotError.current = text;
    }
  }, [notify]);

  usePoll(loadSnapshot, 2000, [revision]);

  const refresh = useCallback(() => setRevision((r) => r + 1), []);

  const act = useCallback(
    async <T,>(fn: () => Promise<T>, success?: string): Promise<T | undefined> => {
      try {
        const result = await fn();
        if (success) notify(success, "info");
        return result;
      } catch (e) {
        notify(errorMessage(e));
        return undefined;
      } finally {
        refresh();
      }
    },
    [notify, refresh],
  );

  const project = useMemo(
    () => snapshot?.projects.find((p) => p.id === snapshot.active) ?? null,
    [snapshot],
  );

  const selectWork = useCallback(
    (id: string) => {
      setSelected(id);
      setPage("work");
      if (project) void api.selectWork({ project: project.id, work: id }).catch((e) => notify(errorMessage(e)));
    },
    [project, notify],
  );

  const openProject = useCallback(
    (id: string) => {
      setSelected(null);
      setPage("work");
      void act(() => api.openProject({ project: id }));
    },
    [act],
  );

  const value: StudioState = {
    snapshot,
    project,
    selected,
    page,
    setPage,
    selectWork,
    openProject,
    revision,
    refresh,
    act,
    toasts,
    notify,
    dismiss,
    zcode,
    loadZcode,
  };
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}
