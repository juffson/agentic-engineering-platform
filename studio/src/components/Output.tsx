import { useEffect, useMemo, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { ChevronDown, ChevronRight, FileText, GitCommit } from "lucide-react";
import type { StageDetail, StageOutput, StageRunOutput, WorkDetail } from "../api/types";
import { api, errorMessage } from "../api/client";
import { clock } from "../lib/format";
import { DiffView } from "./Delivery";
import { SectionLabel, Spinner } from "./ui";

/** What the selected stage produced: its spec documents and, per run, the code it committed. */
export function OutputView({ detail, stage }: { detail: WorkDetail; stage: StageDetail }) {
  // Refetch when a run of this stage finishes or gets a commit.
  const key = stage.attempts.map((a) => `${a.id}:${a.status}:${a.head_sha ?? ""}`).join(",");
  const [out, setOut] = useState<StageOutput | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [doc, setDoc] = useState<string | null>(null);
  const [raw, setRaw] = useState(false);

  useEffect(() => {
    let live = true;
    setError(null);
    api
      .stageOutput({ project: detail.project, work: detail.id, stage: stage.index })
      .then((o) => {
        if (!live) return;
        setOut(o);
        setDoc((d) => (d && o.docs.some((x) => x.path === d) ? d : (o.docs[0]?.path ?? null)));
      })
      .catch((e) => live && setError(errorMessage(e)));
    return () => {
      live = false;
    };
  }, [detail.project, detail.id, stage.index, key]);

  useEffect(() => setOut(null), [detail.id, stage.index]);

  const shown = out?.docs.find((d) => d.path === doc) ?? null;
  const gate = stage.role === "Test" || stage.role === "Accept" || stage.role === "Deliver";

  return (
    <div className="history-scroll" data-testid="output-view">
      <div className="history-inner">
        {error && <p className="inline-error">{error}</p>}
        {!out && !error && (
          <div className="page-loading">
            <Spinner size={16} />
          </div>
        )}
        {out && out.docs.length > 0 && (
          <section className="output-docs" aria-label="文档">
            <SectionLabel
              right={
                <button type="button" className="link-btn" onClick={() => setRaw((r) => !r)}>
                  {raw ? "按格式显示" : "查看源文件"}
                </button>
              }
            >
              文档
            </SectionLabel>
            <div className="output-doc-tabs" role="tablist" aria-label="文档列表">
              {out.docs.map((d) => (
                <button
                  key={d.path}
                  type="button"
                  role="tab"
                  aria-selected={d.path === doc}
                  className={d.path === doc ? "on" : ""}
                  onClick={() => setDoc(d.path)}
                  title={d.path}
                >
                  <FileText size={12} />
                  {d.path.split("/").pop()}
                </button>
              ))}
            </div>
            {shown && (
              <article className="output-doc card">
                <p className="output-doc-path muted">
                  {shown.path}
                  {shown.finished_at && ` · ${clock(shown.finished_at)} 的版本`}
                </p>
                {raw ? (
                  <pre className="output-raw">{shown.content}</pre>
                ) : (
                  <div className="md">
                    <ReactMarkdown remarkPlugins={[remarkGfm]}>{shown.content}</ReactMarkdown>
                  </div>
                )}
              </article>
            )}
          </section>
        )}
        {out && out.runs.some((r) => r.files.length > 0) && (
          <section aria-label="代码改动">
            <SectionLabel right={<span className="muted">{out.runs.filter((r) => r.files.length > 0).length} 次提交</span>}>改动</SectionLabel>
            {out.runs
              .filter((r) => r.files.length > 0)
              .map((r) => (
                <RunChanges key={r.attempt} detail={detail} run={r} />
              ))}
          </section>
        )}
        {out && out.docs.length === 0 && !out.runs.some((r) => r.files.length > 0) && (
          <p className="empty-text">
            {gate
              ? `「${stage.name}」只核对、不改文件，结论在「阶段」视图的对话里。`
              : stage.state === "upcoming"
                ? "这个阶段还没有执行。"
                : "这个阶段还没有提交记录。"}
          </p>
        )}
      </div>
    </div>
  );
}

function runLabel(r: StageRunOutput): string {
  if (r.subtask) return `${r.subtask.id} ${r.subtask.title}`.trim();
  return r.follow_up ? "继续对话" : "执行";
}

function RunChanges({ detail, run }: { detail: WorkDetail; run: StageRunOutput }) {
  const [open, setOpen] = useState(false);
  const [diff, setDiff] = useState<{ text: string; truncated: boolean } | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open || diff) return;
    let live = true;
    api
      .stageDiff({ project: detail.project, work: detail.id, attempt: run.attempt })
      .then((d) => live && setDiff({ text: d.diff, truncated: d.truncated }))
      .catch((e) => live && setError(errorMessage(e)));
    return () => {
      live = false;
    };
  }, [open, diff, detail.project, detail.id, run.attempt]);

  const files = useMemo(() => (diff ? splitDiff(diff.text) : []), [diff]);

  return (
    <div className="output-run card" data-testid="output-run">
      <button type="button" className="output-run-head" onClick={() => setOpen((o) => !o)} aria-expanded={open}>
        {open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
        <span className="output-run-title">{runLabel(run)}</span>
        <span className="muted">
          {run.files.length} 个文件 · <span className="d-add-text">+{run.insertions}</span> <span className="d-del-text">−{run.deletions}</span>
        </span>
        <span className="muted output-run-time">{clock(run.finished_at)}</span>
      </button>
      <ul className="output-commits">
        {run.commits.map((c) => (
          <li key={c.sha} title={c.title}>
            <GitCommit size={12} />
            <code>{c.sha.slice(0, 7)}</code>
            <span>{c.title}</span>
          </li>
        ))}
      </ul>
      {open && (
        <div className="output-files">
          {error && <p className="inline-error">{error}</p>}
          {!diff && !error && <Spinner size={14} />}
          {files.map((f) => (
            <FileDiff key={f.path} path={f.path} text={f.text} add={f.add} del={f.del} single={files.length === 1} />
          ))}
          {diff?.truncated && <p className="diff-trunc">差异过长，已截断显示。</p>}
        </div>
      )}
    </div>
  );
}

function FileDiff({ path, text, add, del, single }: { path: string; text: string; add: number; del: number; single: boolean }) {
  const [open, setOpen] = useState(single);
  return (
    <div className="output-file">
      <button type="button" className="output-file-head" onClick={() => setOpen((o) => !o)} aria-expanded={open}>
        {open ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
        <code>{path}</code>
        <span className="muted">
          <span className="d-add-text">+{add}</span> <span className="d-del-text">−{del}</span>
        </span>
      </button>
      {open && <DiffView diff={text} truncated={false} />}
    </div>
  );
}

/** Split `git show` output into per-file sections (commit headers are dropped). */
export function splitDiff(text: string): { path: string; text: string; add: number; del: number }[] {
  const out: { path: string; lines: string[] }[] = [];
  for (const line of text.split("\n")) {
    const m = /^diff --git a\/(.+) b\/(.+)$/.exec(line) ?? /^diff --cc (.+)$/.exec(line);
    if (m) out.push({ path: m[2] ?? m[1], lines: [line] });
    else if (line.startsWith("commit ") && !line.startsWith("commit-")) continue;
    else if (out.length) out[out.length - 1].lines.push(line);
  }
  // A file changed by several commits of the run shows once, its sections in order.
  const merged = new Map<string, string[]>();
  for (const f of out) merged.set(f.path, [...(merged.get(f.path) ?? []), ...f.lines]);
  return [...merged].map(([path, lines]) => ({
    path,
    text: lines.join("\n").replace(/\n+$/, ""),
    add: lines.filter((l) => l.startsWith("+") && !l.startsWith("+++")).length,
    del: lines.filter((l) => l.startsWith("-") && !l.startsWith("---")).length,
  }));
}
