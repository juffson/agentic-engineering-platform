import { useEffect, useRef, useState } from "react";
import { BookOpen, Check, ChevronDown, ChevronRight, FolderGit2, Plus } from "lucide-react";
import type { Group, WorkSummary } from "../api/types";
import { useStudio } from "../state/store";
import { isLive } from "../lib/work";

const GROUPS: { id: Group; label: string }[] = [
  { id: "active", label: "进行中" },
  { id: "delivered", label: "已交付" },
  { id: "shelved", label: "已归档" },
];

export function Sidebar({ onNewWork, onNewProject }: { onNewWork(): void; onNewProject(): void }) {
  const { snapshot, project, selected, selectWork, page, setPage } = useStudio();
  const [folded, setFolded] = useState<Record<Group, boolean>>({ active: false, delivered: false, shelved: true });
  const work = project?.work ?? [];

  return (
    <nav className="sidebar" aria-label="事项">
      <ProjectSwitcher onNewProject={onNewProject} />
      <div className="sidebar-actions">
        <button type="button" className="new-work" onClick={onNewWork} disabled={!project}>
          <Plus size={15} />
          新建事项
        </button>
      </div>
      <div className="sidebar-scroll">
        {GROUPS.map((g) => {
          const items = work.filter((w) => w.group === g.id);
          const isFolded = folded[g.id];
          return (
            <section key={g.id} className="work-group">
              <button
                type="button"
                className="group-head"
                aria-expanded={!isFolded}
                onClick={() => setFolded((f) => ({ ...f, [g.id]: !f[g.id] }))}
              >
                {isFolded ? <ChevronRight size={13} /> : <ChevronDown size={13} />}
                <span>{g.label}</span>
                <span className="group-count">{items.length}</span>
              </button>
              {!isFolded && (
                <ul className="work-list">
                  {items.length === 0 && <li className="work-empty">暂无</li>}
                  {items.map((w) => (
                    <li key={w.id}>
                      <WorkRow item={w} active={page === "work" && w.id === selected} onSelect={() => selectWork(w.id)} />
                    </li>
                  ))}
                </ul>
              )}
            </section>
          );
        })}
        {snapshot && !project && <p className="sidebar-hint">还没有打开的项目。点击上方创建一个项目。</p>}
      </div>
      <div className="sidebar-foot">
        <button type="button" className={`foot-link${page === "memory" ? " on" : ""}`} onClick={() => setPage("memory")} disabled={!project}>
          <BookOpen size={14} />
          项目记忆
        </button>
        <button type="button" className={`foot-link${page === "repos" ? " on" : ""}`} onClick={() => setPage("repos")} disabled={!project}>
          <FolderGit2 size={14} />
          仓库
          {project && <span className="foot-count">{project.repositories.length}</span>}
        </button>
      </div>
    </nav>
  );
}

function WorkRow({ item, active, onSelect }: { item: WorkSummary; active: boolean; onSelect(): void }) {
  const failed = item.status === "failed";
  const delivered = item.group !== "active";
  const done = Math.min(item.stage, item.stage_count);
  const dot = item.running || isLive(item.status) ? "running" : failed ? "failed" : null;
  return (
    <button type="button" className={`work-row${active ? " on" : ""}`} onClick={onSelect} aria-current={active ? "true" : undefined} data-work={item.id}>
      <span className="work-row-top">
        <span className="work-name" title={item.name}>
          {item.name}
        </span>
        {item.autopilot && <span className="tag tag-auto">自动</span>}
        {dot && <span className={`dot dot-${dot}`} aria-label={dot === "running" ? "运行中" : "失败"} />}
      </span>
      <span className="work-row-bottom">
        <span className="seg-bar" aria-hidden>
          {Array.from({ length: item.stage_count }, (_, i) => (
            <span
              key={i}
              className={i < done ? "seg done" : i === done && !delivered ? `seg cur${failed ? " fail" : ""}` : "seg"}
            />
          ))}
        </span>
        <span className="work-stage">
          {delivered ? item.stage_name : `${done + 1}/${item.stage_count} · ${item.stage_name}`}
        </span>
      </span>
    </button>
  );
}

function ProjectSwitcher({ onNewProject }: { onNewProject(): void }) {
  const { snapshot, project, openProject } = useStudio();
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    const esc = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    window.addEventListener("mousedown", close);
    window.addEventListener("keydown", esc);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("keydown", esc);
    };
  }, [open]);

  return (
    <div className="project-switcher" ref={ref}>
      <button type="button" className="project-btn" onClick={() => setOpen((o) => !o)} aria-expanded={open} aria-haspopup="menu">
        <span className="project-mark">{(project?.name ?? "A").slice(0, 1)}</span>
        <span className="project-name">{project?.name ?? "选择项目"}</span>
        <ChevronDown size={14} className="project-chevron" />
      </button>
      {open && (
        <div className="menu" role="menu">
          {snapshot?.projects.map((p) => (
            <button
              key={p.id}
              type="button"
              role="menuitem"
              className="menu-item"
              onClick={() => {
                setOpen(false);
                if (p.id !== project?.id) openProject(p.id);
              }}
            >
              <span className="menu-item-label">{p.name}</span>
              <span className="menu-item-meta">{p.work.length} 个事项</span>
              {p.id === project?.id && <Check size={14} className="menu-check" />}
            </button>
          ))}
          <div className="menu-sep" />
          <button
            type="button"
            role="menuitem"
            className="menu-item"
            onClick={() => {
              setOpen(false);
              onNewProject();
            }}
          >
            <Plus size={14} />
            <span className="menu-item-label">新建项目</span>
          </button>
        </div>
      )}
    </div>
  );
}
