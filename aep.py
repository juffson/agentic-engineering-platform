#!/usr/bin/env python3
"""Dispatch a pinned Git task and verify a candidate in a fresh local worktree."""
import argparse
import fcntl
import json
import os
import re
import shlex
import subprocess
import sys
import tempfile
import uuid
from contextlib import contextmanager
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


def run_logged(command, cwd, logfile, timeout, prompt=None):
    # A failed check still needs a complete log. Limit each child process group as a unit.
    import signal
    with logfile.open("w") as out:
        with subprocess.Popen(command, cwd=cwd, stdin=subprocess.PIPE, stdout=out,
                              stderr=subprocess.STDOUT, text=True, start_new_session=True) as proc:
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


def execute(repo, args):
    if args.timeout <= 0:
        raise PilotError("timeout must be positive")
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
        candidate = base if args.implementer else commit(repo, args.candidate)
        if args.candidate:
            record["requested_candidate_sha"] = candidate
            write_report(folder, record)
        check_scope(repo, base, candidate, task["allowed_paths"])
        git(repo, "worktree", "add", "--detach", str(work), candidate)
        attached = True
        if args.implementer:
            prompt = spec + "\nImplement this spec in the allowed files only: " + ", ".join(task["allowed_paths"])
            rc = run_logged(["codex", "exec", "--ephemeral", "--sandbox", "workspace-write", "--json", "-"],
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
            rc = run_logged(["claude", "-p", "--tools", "", "--no-session-persistence",
                             "--output-format", "text"], work, folder / "review.txt", args.timeout, prompt)
            if rc or not (folder / "review.txt").read_text(errors="replace").strip():
                raise PilotError("reviewer did not produce a review; see review.txt")
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
        source = cmd.add_mutually_exclusive_group(required=True)
        source.add_argument("--candidate", help="existing local candidate ref or SHA")
        if name == "run":
            source.add_argument("--implementer", choices=["codex"])
        cmd.set_defaults(implementer=None)
        cmd.add_argument("--reviewer", choices=["manual", "claude"], default="manual")
        cmd.add_argument("--timeout", type=int, default=600, help="seconds per child process")
    args = parser.parse_args()
    repo = Path(git(Path(args.repo), "rev-parse", "--show-toplevel").strip())
    if args.command == "dispatch":
        dispatch(repo, args)
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
    except (PilotError, OSError, ValueError, KeyError, subprocess.TimeoutExpired) as error:
        sys.exit(str(error))
