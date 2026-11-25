import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { api, errorMessage } from "../api/client";
import type { ProjectSummary, Snapshot, ZCodeModel } from "../api/types";

export type Page = "work" | "memory" | "repos" | "branches" | "usage" | "flows";
type NavigationGuard = (navigate: () => void) => void;

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
  snapshotError: string | null;
  reloadSnapshot(): Promise<void>;
  project: ProjectSummary | null;
  selected: string | null;
  page: Page;
  setPage(page: Page): void;
  selectWork(id: string): void;
  openProject(id: string): void;
  requestNavigation(navigate: () => void): void;
  registerNavigationGuard(guard: NavigationGuard): () => void;
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
  const [snapshotError, setSnapshotError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [page, setCurrentPage] = useState<Page>("work");
  const [revision, setRevision] = useState(0);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const toastSeq = useRef(0);
  const lastSnapshotError = useRef<string | null>(null);
  const snapshotRequest = useRef(0);
  const navigationGuard = useRef<NavigationGuard | null>(null);
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
    const request = ++snapshotRequest.current;
    try {
      const s = await api.snapshot();
      if (request !== snapshotRequest.current) return;
      lastSnapshotError.current = null;
      setSnapshotError(null);
      setSnapshot(s);
      setSelected((cur) => {
        const project = s.projects.find((p) => p.id === s.active);
        if (cur && project?.work.some((w) => w.id === cur)) return cur;
        return s.selected ?? project?.work[0]?.id ?? null;
      });
    } catch (e) {
      if (request !== snapshotRequest.current) return;
      const text = errorMessage(e);
      if (lastSnapshotError.current !== text) notify(`无法读取工作台状态：${text}`);
      lastSnapshotError.current = text;
      setSnapshotError(text);
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

  const registerNavigationGuard = useCallback((guard: NavigationGuard) => {
    navigationGuard.current = guard;
    return () => {
      if (navigationGuard.current === guard) navigationGuard.current = null;
    };
  }, []);

  const requestNavigation = useCallback((navigate: () => void) => {
    if (navigationGuard.current) navigationGuard.current(navigate);
    else navigate();
  }, []);

  const setPage = (next: Page) => {
    if (next !== page) requestNavigation(() => setCurrentPage(next));
  };

  const selectWork = useCallback(
    (id: string) => requestNavigation(() => {
      setSelected(id);
      setCurrentPage("work");
      if (project) void api.selectWork({ project: project.id, work: id }).catch((e) => notify(errorMessage(e)));
    }),
    [project, notify, requestNavigation],
  );

  const openProject = useCallback(
    (id: string) => requestNavigation(() => {
      setSelected(null);
      setCurrentPage("work");
      void act(() => api.openProject({ project: id }));
    }),
    [act, requestNavigation],
  );

  const value: StudioState = {
    snapshot,
    snapshotError,
    reloadSnapshot: loadSnapshot,
    project,
    selected,
    page,
    setPage,
    selectWork,
    openProject,
    requestNavigation,
    registerNavigationGuard,
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
