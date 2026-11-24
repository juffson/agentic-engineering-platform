import { useState } from "react";
import { FolderOpen, Plus } from "lucide-react";
import type { Template } from "../api/types";
import { api } from "../api/client";
import { useStudio } from "../state/store";
import { Button, Field, Modal, Segmented } from "./ui";

export function NewWorkDialog({ onClose }: { onClose(): void }) {
  const { snapshot, project, act, selectWork } = useStudio();
  const templates = snapshot?.templates ?? [];
  const [name, setName] = useState("");
  const [template, setTemplate] = useState<Template>(templates.find((t) => t.id === "Software")?.id ?? templates[0]?.id ?? "Software");
  const [release, setRelease] = useState(false);
  const [repos, setRepos] = useState<string[]>(project?.repositories ?? []);
  const [busy, setBusy] = useState(false);
  const stages = templates.find((t) => t.id === template)?.stages ?? [];

  const create = async () => {
    if (!project) return;
    setBusy(true);
    const id = await act(() => api.createWork({ project: project.id, name: name.trim(), template, release, repositories: repos }), "事项已创建");
    setBusy(false);
    if (id) {
      selectWork(id);
      onClose();
    }
  };

  return (
    <Modal
      title="新建事项"
      onClose={onClose}
      width={560}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            取消
          </Button>
          <Button variant="primary" onClick={create} busy={busy} disabled={!name.trim() || repos.length === 0}>
            创建事项
          </Button>
        </>
      }
    >
      <Field label="名称">
        <input
          className="input"
          autoFocus
          value={name}
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && name.trim() && repos.length > 0 && void create()}
          placeholder="例如：自选股分组支持拖拽排序"
          aria-label="事项名称"
        />
      </Field>
      <div className="field-row">
        <Field label="类型">
          <Segmented
            label="类型"
            value={release ? "release" : "feature"}
            options={[
              { value: "feature", label: "功能" },
              { value: "release", label: "发布" },
            ]}
            onChange={(v) => setRelease(v === "release")}
          />
        </Field>
        <Field label="模板">
          <Segmented<Template> label="模板" value={template} options={templates.map((t) => ({ value: t.id, label: t.label }))} onChange={setTemplate} />
        </Field>
      </div>
      <ol className="template-preview" aria-label="阶段预览">
        {stages.map((s, i) => (
          <li key={s}>
            <span className="tp-num">{i + 1}</span>
            {s}
          </li>
        ))}
      </ol>
      <Field label="仓库" hint={repos.length === 0 ? "至少选择一个仓库" : undefined}>
        <div className="repo-checks">
          {(project?.repositories ?? []).map((r) => (
            <label key={r} className="check">
              <input type="checkbox" checked={repos.includes(r)} onChange={(e) => setRepos((cur) => (e.target.checked ? [...cur, r] : cur.filter((x) => x !== r)))} />
              <code>{r}</code>
            </label>
          ))}
          {project?.repositories.length === 0 && <p className="muted">项目还没有仓库，请先在「仓库」页添加。</p>}
        </div>
      </Field>
    </Modal>
  );
}

export function NewProjectDialog({ onClose }: { onClose(): void }) {
  const { act } = useStudio();
  const [name, setName] = useState("");
  const [repos, setRepos] = useState("");
  const [busy, setBusy] = useState(false);
  const pick = async () => {
    const path = await act(() => api.pickFolder({ prompt: "选择项目的 git 仓库文件夹" }));
    if (!path) return;
    setRepos((r) => (r.split("\n").map((x) => x.trim()).includes(path) ? r : r.trim() ? `${r.trim()}\n${path}` : path));
    if (!name.trim()) setName(path.split("/").pop() ?? "");
  };
  const create = async () => {
    setBusy(true);
    const list = repos
      .split("\n")
      .map((s) => s.trim())
      .filter(Boolean);
    const ok = await act(() => api.createProject({ name: name.trim(), repositories: list }), "项目已创建");
    setBusy(false);
    if (ok !== undefined) onClose();
  };
  return (
    <Modal
      title="新建项目"
      onClose={onClose}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            取消
          </Button>
          <Button variant="primary" icon={<Plus size={14} />} onClick={create} busy={busy} disabled={!name.trim()}>
            创建项目
          </Button>
        </>
      }
    >
      <Field label="项目名称">
        <input className="input" autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder="例如：行情终端" aria-label="项目名称" />
      </Field>
      <Field label="仓库路径" hint="选择本地 git 仓库文件夹，或每行填一个绝对路径；之后也可以在「仓库」页添加。">
        <textarea className="input mono" rows={3} value={repos} onChange={(e) => setRepos(e.target.value)} placeholder="/Users/you/code/app" aria-label="仓库路径" />
        <Button variant="secondary" size="sm" icon={<FolderOpen size={13} />} onClick={pick} className="pick-folder">
          选择文件夹…
        </Button>
      </Field>
    </Modal>
  );
}
