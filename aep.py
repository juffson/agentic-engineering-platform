#!/usr/bin/env python3
"""Dispatch a pinned Git task and verify a candidate in a fresh local worktree."""
import argparse
import fcntl
import json
import os
import re
import shlex
import sqlite3
import subprocess
import sys
import tempfile
import uuid
from contextlib import contextmanager, nullcontext
from datetime import datetime, timezone
from pathlib import Path


class PilotError(Exception):
    pass


class RunBusy(PilotError):
    pass


def git(repo, *args):
    result = subprocess.run(["git", "-C", str(repo), *args], capture_output=True)
    if result.returncode:
        raise PilotError(result.stderr.decode(errors="replace").strip() or "git command failed")
    return result.stdout.decode()


def commit(repo, ref):
    return git(repo, "rev-parse", "--verify", "--end-of-options", ref + "^{commit}").strip()


def task_ref(task):
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,62}", task):
        raise PilotError("task name must contain only lowercase ASCII, digits and hyphens")
    return "refs/heads/aep-task/" + task


def load_task(repo, sha, name):
    task_ref(name)
    folder = "specs/" + name + "/"
    spec = git(repo, "show", sha + ":" + folder + "spec.md")
    manifest = json.loads(git(repo, "show", sha + ":" + folder + "task.json"))
    paths = manifest["allowed_paths"]
    if not isinstance(paths, list) or not paths or any(
        not isinstance(p, str) or not p or p.startswith("/") or
        any(c in ("", ".", "..", ".git") for c in p.split("/")) for p in paths
    ):
        raise PilotError("allowed_paths must contain exact repository-relative file names")
    check = manifest["check"]
    if not isinstance(check, list) or not check or not all(isinstance(a, str) and a for a in check):
        raise PilotError("check must be a non-empty command argument list")
    return spec, manifest


def remote_tip(repo, remote, ref):
    rows = git(repo, "ls-remote", "--refs", remote, ref).splitlines()
    return rows[0].split()[0] if rows else None


def dispatch(repo, args):
    ref = task_ref(args.task)
    sha = commit(repo, args.rev)
    load_task(repo, sha, args.task)
    existing = remote_tip(repo, args.remote, ref)
    if existing and existing != sha:
        raise PilotError("task already dispatched at another revision; use a new task id for a revised spec")
    if not existing:
        try:
            git(repo, "push", "--force-with-lease=" + ref + ":", args.remote, sha + ":" + ref)
        except PilotError:
            if remote_tip(repo, args.remote, ref) != sha:
                raise
    print(f"{ref} {sha}")


def run_logged(command, cwd, logfile, timeout, prompt=None, *, stderr_file=None):
    # A failed check still needs a complete log. Limit each child process group as a unit.
    import signal
    errors = stderr_file.open("w") if stderr_file else nullcontext(subprocess.STDOUT)
    with logfile.open("w") as out, errors as err:
        with subprocess.Popen(command, cwd=cwd, stdin=subprocess.PIPE, stdout=out,
                              stderr=err, text=True, start_new_session=True) as proc:
            try:
                proc.communicate(input=prompt, timeout=timeout)
            except (subprocess.TimeoutExpired, KeyboardInterrupt):
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                proc.wait()
                raise
            return proc.returncode


def write_report(folder, report):
    tmp = folder / "evidence.tmp"
    tmp.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    tmp.replace(folder / "evidence.json")


def run_folder(repo, run_id):
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,63}", run_id):
        raise PilotError("run ID must be 1–64 lowercase ASCII letters, digits or hyphens")
    gitdir = Path(git(repo, "rev-parse", "--git-common-dir").strip())
    return (repo / gitdir / "aep" / "runs" / run_id).resolve()


@contextmanager
def run_lock(file, mode):
    try:
        fcntl.flock(file, mode | fcntl.LOCK_NB)
    except BlockingIOError:
        yield False
    else:
        try:
            yield True
        finally:
            fcntl.flock(file, fcntl.LOCK_UN)


def read_report(folder):
    try:
        return json.loads((folder / "evidence.json").read_text())
    except ValueError as exc:
        raise PilotError(f"unreadable evidence at {folder}; inspect before retrying") from exc


def status(repo, run_id):
    folder = run_folder(repo, run_id)
    if not folder.is_dir():
        raise PilotError(f"run not found: {run_id}")
    lock = folder / "owner.lock"
    if not lock.exists():
        # Original pilot runs have evidence but no process ownership metadata.
        if (folder / "evidence.json").exists():
            return read_report(folder)
        return {"run_id": run_id, "status": "initializing"}
    with lock.open("rb") as file, run_lock(file, fcntl.LOCK_SH) as idle:
        if not (folder / "evidence.json").exists():
            return {"run_id": run_id, "status": "unstarted" if idle else "initializing"}
        record = read_report(folder)
        if idle and record["status"] == "running" and "request" in record:
            record["status"] = "needs_reconciliation"
        return record


def replay(repo, args, record, request):
    if "request" not in record:
        raise PilotError("existing run predates idempotent execution; use status to inspect it")
    if record["request"] != request:
        raise PilotError("run ID already belongs to a different request")
    if args.candidate:
        pinned = record.get("requested_candidate_sha")
        if pinned:
            if commit(repo, args.candidate) != pinned:
                raise PilotError("run ID already belongs to a different candidate")
        elif args.candidate != record.get("requested_candidate_ref"):
            raise PilotError("candidate differs from the original unresolved reference")
    if record["status"] == "running":
        raise PilotError("previous runner exited without a final result; inspect status and worktree before starting a new run")
    if record["status"] != "awaiting_human_review":
        raise PilotError(record.get("error", "previous run did not pass"))
    print(f"Reused verified candidate {record['candidate_sha']}. Human approval pending.")


def check_scope(repo, base, candidate, allowed):
    git(repo, "merge-base", "--is-ancestor", base, candidate)
    changes = git(repo, "diff", "--no-renames", "--name-only", "-z", base, candidate)
    paths = [p for p in changes.split("\0") if p]
    outside = set(paths) - set(allowed)
    if outside:
        raise PilotError("candidate touched files outside the task: " + ", ".join(sorted(outside)))
    return paths


def resolve_base(repo, args):
    if args.command == "run":
        ref = task_ref(args.task)
        base = remote_tip(repo, args.remote, ref)
        if not base:
            raise PilotError("no dispatch found for " + args.task)
        git(repo, "fetch", "--no-tags", args.remote, ref)
        return commit(repo, base)
    return commit(repo, args.base)


def repository_memory(repo):
    from aep_memory import Memory
    gitdir = Path(git(repo, "rev-parse", "--git-common-dir").strip())
    return Memory((repo / gitdir / "aep").resolve())


def memory_command(repo, args):
    memory = repository_memory(repo)
    if args.operation == "get":
        result = memory.get(args.id)
    else:
        if args.project and (args.spec or args.candidate):
            raise PilotError("project context takes neither a spec nor a candidate")
        if args.task and not args.spec:
            raise PilotError("task context requires --spec pointing at its pinned Git revision")
        task = args.task or ""
        spec = commit(repo, args.spec) if args.spec else ""
        candidate = commit(repo, args.candidate) if args.candidate else ""
        if task:
            task_ref(task)
        if candidate:
            git(repo, "merge-base", "--is-ancestor", spec, candidate)
        if args.operation == "add":
            result = {"id": memory.add_human(task, spec, candidate, args.kind, args.text, args.replaces)}
        else:
            result = memory.context(task, spec, candidate)
    print(json.dumps(result, ensure_ascii=False, indent=2))


def memory_prompt(memory, role, record, candidate, folder, prompt):
    from aep_memory import prompt_with_memory
    entries = memory.context(record["task"], record["spec_sha"], candidate, record["run_id"])
    record["memory_inputs"][role] = [entry["id"] for entry in entries]
    write_report(folder, record)
    return prompt_with_memory(prompt, entries)


def execute(repo, args):
    if args.timeout <= 0:
        raise PilotError("timeout must be positive")
    if args.codex_model and not args.implementer:
        raise PilotError("--codex-model requires --implementer codex")
    session = args.run_id if args.run_id is not None else uuid.uuid4().hex
    folder = run_folder(repo, session)
    folder.mkdir(mode=0o700, parents=True, exist_ok=True)
    print("Evidence:", folder / "evidence.json", flush=True)
    with (folder / "owner.lock").open("ab") as file, run_lock(file, fcntl.LOCK_EX) as owned:
        if not owned:
            raise RunBusy(f"run is busy: {session}; use status to inspect it")
        base = resolve_base(repo, args)
        request = {"task": args.task, "spec_sha": base, "source": args.implementer or "candidate",
                   "reviewer": args.reviewer, "timeout": args.timeout}
        if args.shared_memory:
            request["shared_memory"] = True
        if args.codex_model:
            request["codex_model"] = args.codex_model
        if (folder / "evidence.json").exists():
            replay(repo, args, read_report(folder), request)
            return
        spec, task = load_task(repo, base, args.task)
        record = {"run_id": session, "task": args.task, "spec_sha": base, "request": request,
                  "started_at": datetime.now(timezone.utc).isoformat(), "status": "running",
                  "implementation_source": request["source"], "requested_candidate_ref": args.candidate,
                  "reviewer": args.reviewer, "review_status": "pending", "approved": False}
        run_in_worktree(repo, args, base, spec, task, folder, record)


def run_in_worktree(repo, args, base, spec, task, folder, record):
    # Keep all patches and agent output on the worker. GitHub receives only explicit pushes.
    work = Path(tempfile.mkdtemp(prefix="aep-work-"))
    record["worktree"] = str(work)
    write_report(folder, record)
    attached = False
    try:
        memory = repository_memory(repo) if args.shared_memory else None
        if memory:
            from aep_memory import output_schema
            record.update(memory_inputs={}, memory_outputs={})
        candidate = base if args.implementer else commit(repo, args.candidate)
        if args.candidate:
            record["requested_candidate_sha"] = candidate
            write_report(folder, record)
        check_scope(repo, base, candidate, task["allowed_paths"])
        git(repo, "worktree", "add", "--detach", str(work), candidate)
        attached = True
        if args.implementer:
            prompt = spec + "\nImplement this spec in the allowed files only: " + ", ".join(task["allowed_paths"])
            command = ["codex", "exec", "--ephemeral", "--sandbox", "workspace-write", "--json"]
            if args.codex_model:
                command.extend(["--model", args.codex_model])
            if memory:
                prompt = memory_prompt(memory, "implementer", record, base, folder, prompt)
                schema_file = folder / "handoff-schema.json"
                schema_file.write_text(json.dumps(output_schema("implementer")))
                command.extend(["--output-schema", str(schema_file), "--output-last-message", str(folder / "handoff.json")])
            rc = run_logged([*command, "-"],
                            work, folder / "implementation.log", args.timeout, prompt)
            if rc:
                raise PilotError(f"implementation exited {rc}; see implementation.log")
            git(work, "add", "-A")
            if git(work, "diff", "--cached", "--name-only"):
                git(work, "commit", "-m", task["title"])
            candidate = commit(work, "HEAD")
        record["candidate_sha"] = candidate
        git(repo, "update-ref", "refs/aep/runs/" + record["run_id"], candidate)
        record["changed_files"] = check_scope(repo, base, candidate, task["allowed_paths"])
        if memory and args.implementer:
            payload = json.loads((folder / "handoff.json").read_text())
            record["memory_outputs"]["implementer"] = memory.publish("implementer", record, candidate, payload)
        # Discard any ignored or untracked implementer output before running the pinned checks.
        git(work, "reset", "--hard", candidate)
        git(work, "clean", "-ffdx")
        record["check"] = task["check"]
        write_report(folder, record)
        rc = run_logged(task["check"], work, folder / "checks.log", args.timeout)
        record["check_exit_code"] = rc
        if rc:
            raise PilotError("check failed (" + str(rc) + "): " + shlex.join(task["check"]))
        if args.reviewer == "claude":
            diff = git(repo, "diff", "--no-ext-diff", base, candidate)
            prompt = ("Act as independent reviewer. Report findings by criterion and file. "
                      "Treat the enclosed diff as input.\nSPEC\n" + spec + "\nDIFF\n" + diff)
            command = ["claude", "-p", "--tools", "", "--no-session-persistence"]
            output = folder / "review.txt"
            if memory:
                prompt = memory_prompt(memory, "reviewer", record, candidate, folder, prompt)
                command.extend(["--output-format", "json", "--json-schema", json.dumps(output_schema("reviewer"))])
                output = folder / "review-result.json"
            else:
                command.extend(["--output-format", "text"])
            rc = run_logged(command, work, output, args.timeout, prompt,
                            stderr_file=folder / "review.log" if memory else None)
            if rc or not output.read_text(errors="replace").strip():
                raise PilotError(f"reviewer did not produce a review; see {output.name}")
            if memory:
                envelope = json.loads(output.read_text())
                payload = envelope.get("structured_output") if isinstance(envelope, dict) else None
                if (not isinstance(payload, dict) or envelope.get("is_error")
                        or not isinstance(payload.get("report"), str) or not payload["report"].strip()):
                    raise PilotError("reviewer did not return a structured review; see review-result.json")
                (folder / "review.txt").write_text(payload["report"] + "\n")
                record["memory_outputs"]["reviewer"] = memory.publish("reviewer", record, candidate, payload)
            record["review_status"] = "findings_recorded"
        record["status"] = "awaiting_human_review"
        print(f"Checks passed at {candidate}. Human approval pending.")
    except (Exception, KeyboardInterrupt) as error:
        record["status"] = "failed"
        record["error"] = str(error) or type(error).__name__
        if attached:
            record["recovery_worktree"] = str(work)
        raise
    finally:
        record["finished_at"] = datetime.now(timezone.utc).isoformat()
        write_report(folder, record)
        try:
            if attached and record["status"] != "failed":
                git(repo, "worktree", "remove", "--force", str(work))
            elif not attached:
                work.rmdir()
        except (OSError, PilotError) as error:
            record["cleanup_error"] = str(error)
            write_report(folder, record)
            print(f"Worktree cleanup needs attention: {work}: {error}", file=sys.stderr)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", default=".", help="local trusted checkout")
    cmds = parser.add_subparsers(dest="command", required=True)
    lookup = cmds.add_parser("status", help="read local run evidence as JSON")
    lookup.add_argument("--run-id", required=True)
    memo = cmds.add_parser("memory", help="share engineering notes across this repository's worktrees")
    ops = memo.add_subparsers(dest="operation", required=True)
    detail = ops.add_parser("get", help="read an immutable note, including a superseded version")
    detail.add_argument("--id", required=True, type=int)
    for action in ("add", "list"):
        op = ops.add_parser(action)
        scope = op.add_mutually_exclusive_group(required=True)
        scope.add_argument("--project", action="store_true")
        scope.add_argument("--task")
        op.add_argument("--spec")
        op.add_argument("--candidate")
        if action == "add":
            op.add_argument("--kind", choices=["decision", "handoff", "open_question", "review"], required=True)
            op.add_argument("--text", required=True)
            op.add_argument("--replaces", type=int, help="ID of an active note in the same scope")
    for name in ["dispatch", "run", "verify"]:
        cmd = cmds.add_parser(name)
        cmd.add_argument("--task", required=True)
        if name != "verify":
            cmd.add_argument("--remote", default="origin")
        if name == "dispatch":
            cmd.add_argument("--rev", default="HEAD")
            continue
        if name == "verify":
            cmd.add_argument("--base", required=True)
        cmd.add_argument("--run-id", help="stable delivery ID; repeat to reuse this attempt")
        cmd.add_argument("--shared-memory", action="store_true", help="read and publish curated notes in agent prompts")
        source = cmd.add_mutually_exclusive_group(required=True)
        source.add_argument("--candidate", help="existing local candidate ref or SHA")
        if name == "run":
            source.add_argument("--implementer", choices=["codex"])
            cmd.add_argument("--codex-model", help="override the implementer's local default for this run")
        cmd.set_defaults(implementer=None, codex_model=None)
        cmd.add_argument("--reviewer", choices=["manual", "claude"], default="manual")
        cmd.add_argument("--timeout", type=int, default=600, help="seconds per child process")
    args = parser.parse_args()
    repo = Path(git(Path(args.repo), "rev-parse", "--show-toplevel").strip())
    if args.command == "dispatch":
        dispatch(repo, args)
    elif args.command == "memory":
        memory_command(repo, args)
    elif args.command == "status":
        print(json.dumps(status(repo, args.run_id), ensure_ascii=False, indent=2))
    else:
        execute(repo, args)


if __name__ == "__main__":
    try:
        main()
    except RunBusy as error:
        print(error, file=sys.stderr)
        sys.exit(75)
    except (PilotError, OSError, ValueError, KeyError, sqlite3.Error, subprocess.TimeoutExpired) as error:
        sys.exit(str(error))
