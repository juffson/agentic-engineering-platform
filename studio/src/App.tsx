import { useState, type ReactNode } from "react";
import { Inbox, Plus } from "lucide-react";
import { StudioProvider, useStudio } from "./state/store";
import { Sidebar } from "./components/Sidebar";
import { WorkPage } from "./components/WorkPage";
import { MemoryPage, ReposPage } from "./components/SecondaryPages";
import { BranchesPage } from "./components/Branches";
import { NewProjectDialog, NewWorkDialog } from "./components/Dialogs";
import { Toasts } from "./components/Toasts";
import { Button, Spinner } from "./components/ui";

export function App() {
  return (
    <StudioProvider>
      <Shell />
    </StudioProvider>
  );
}

function Shell() {
  const { snapshot, project, selected, page } = useStudio();
  const [dialog, setDialog] = useState<"work" | "project" | null>(null);

  let main;
  if (!snapshot) {
    main = (
      <div className="page-loading">
        <Spinner size={16} />
      </div>
    );
  } else if (!project) {
    main = (
      <Empty title="开始一个项目" text="项目把若干本地仓库和它们的开发事项放在一起。" action={<Button variant="primary" icon={<Plus size={14} />} onClick={() => setDialog("project")}>新建项目</Button>} />
    );
  } else if (page === "memory") {
    main = <MemoryPage project={project} />;
  } else if (page === "repos") {
    main = <ReposPage project={project} />;
  } else if (page === "branches") {
    main = <BranchesPage project={project} />;
  } else if (selected && project.work.some((w) => w.id === selected)) {
    main = <WorkPage key={`${project.id}/${selected}`} project={project.id} work={selected} />;
  } else {
    main = (
      <Empty title="还没有选中的事项" text="新建一个开发事项，Agent 会按阶段把它从需求推进到交付。" action={<Button variant="primary" icon={<Plus size={14} />} onClick={() => setDialog("work")}>新建事项</Button>} />
    );
  }

  return (
    <div className="app">
      <Sidebar onNewWork={() => setDialog("work")} onNewProject={() => setDialog("project")} />
      <main className="main">{main}</main>
      {dialog === "work" && <NewWorkDialog onClose={() => setDialog(null)} />}
      {dialog === "project" && <NewProjectDialog onClose={() => setDialog(null)} />}
      <Toasts />
    </div>
  );
}

function Empty({ title, text, action }: { title: string; text: string; action: ReactNode }) {
  return (
    <div className="empty">
      <Inbox size={28} className="muted" />
      <h2>{title}</h2>
      <p>{text}</p>
      {action}
    </div>
  );
}
