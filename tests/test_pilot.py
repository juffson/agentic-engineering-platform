"""Exercise the CLI against a disposable clone and bare dispatch repository."""
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


class PilotTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.env = dict(os.environ, TMPDIR=tmp.name)
        self.repo = Path(tmp.name) / "repo"
        self.repo.mkdir()
        subprocess.run(["git", "init", "--bare", str(Path(tmp.name) / "remote")],
                       check=True, capture_output=True)
        self.git("init", "-b", "main")
        self.git("config", "user.email", "aep@example.invalid")
        self.git("config", "user.name", "AEP Test")
        self.git("config", "commit.gpgsign", "false")
        self.git("remote", "add", "origin", str(Path(tmp.name) / "remote"))
        self.spec = self.repo / "specs" / "sample"
        self.spec.mkdir(parents=True)
        (self.spec / "spec.md").write_text("Write ok in result.txt.")
        (self.spec / "task.json").write_text(json.dumps({
            "title": "sample", "allowed_paths": ["result.txt"],
            "check": [sys.executable, "-I", "-c",
                      "from pathlib import Path; assert Path('result.txt').read_text() == 'ok'"],
        }))
        (self.repo / "result.txt").write_text("old")
        self.git("add", ".")
        self.git("commit", "-m", "base")
        self.base = self.git("rev-parse", "HEAD")
        self.cli("dispatch")

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.repo, check=True,
                              capture_output=True, text=True, env=self.env).stdout.strip()

    def cli(self, command, *args, status=0):
        result = subprocess.run([sys.executable, str(ROOT / "aep.py"), command,
                                 "--task", "sample", *args], cwd=self.repo,
                                capture_output=True, text=True, env=self.env)
        self.assertEqual(result.returncode, status, result.stdout + result.stderr)
        return result

    def evidence(self, result):
        path = Path(result.stdout.splitlines()[0].split("Evidence: ", 1)[1])
        return json.loads(path.read_text())

    def status(self, run_id):
        result = subprocess.run([sys.executable, str(ROOT / "aep.py"), "status", "--run-id", run_id],
                                cwd=self.repo, env=self.env, capture_output=True, text=True, check=True)
        return json.loads(result.stdout)

    def blocking_run(self, command="verify"):
        # A real check process leaves a marker and waits, so the second delivery
        # and the crash take place after execution actually started.
        marker = self.repo.parent / "check-starts"
        release = self.repo.parent / "release"
        script = ("import pathlib, sys, time\n"
                  "marker, release = map(pathlib.Path, sys.argv[1:])\n"
                  "with marker.open('a') as f: f.write('start\\n')\n"
                  "until = time.monotonic() + 20\n"
                  "while not release.exists() and time.monotonic() < until: time.sleep(0.02)\n"
                  "assert release.exists()\n")
        manifest = json.loads((self.spec / "task.json").read_text())
        manifest["check"] = [sys.executable, "-I", "-c", script, str(marker), str(release)]
        (self.spec / "task.json").write_text(json.dumps(manifest))
        self.git("commit", "-am", "freeze blocking check")
        sha = self.git("rev-parse", "HEAD")
        args = ["--candidate", sha, "--run-id", "delivery-1"]
        if command == "run":
            remote = str(self.repo.parent / "blocking.git")
            self.git("init", "--bare", remote)
            self.git("remote", "add", "blocking", remote)
            self.cli("dispatch", "--remote", "blocking")
            args.extend(["--remote", "blocking"])
        else:
            args.extend(["--base", sha])
        child = subprocess.Popen([sys.executable, str(ROOT / "aep.py"), command, "--task", "sample", *args],
                                 cwd=self.repo, env=self.env, stdout=subprocess.DEVNULL,
                                 stderr=subprocess.DEVNULL)
        def stop():
            release.touch()
            if child.poll() is None:
                child.terminate()
            child.wait(timeout=10)
        self.addCleanup(stop)
        deadline = time.monotonic() + 10
        while not marker.exists() and time.monotonic() < deadline and child.poll() is None:
            time.sleep(0.02)
        self.assertTrue(marker.exists(), f"check did not start, runner exit: {child.poll()}")
        return child, args, marker, release

    def test_concurrent_delivery_then_terminal_replay(self):
        child, args, marker, release = self.blocking_run("run")
        self.assertEqual(self.status("delivery-1")["status"], "running")
        self.cli("run", *args, status=75)
        release.touch()
        self.assertEqual(child.wait(timeout=10), 0)
        original = self.status("delivery-1")
        self.cli("run", *args)
        self.assertEqual(self.status("delivery-1"), original)
        self.assertEqual(marker.read_text().splitlines(), ["start"])

    def test_process_death_preserves_worktree_and_does_not_restart(self):
        child, args, marker, release = self.blocking_run()
        child.kill()
        child.wait(timeout=10)
        release.touch()
        interrupted = self.status("delivery-1")
        self.assertEqual(interrupted["status"], "needs_reconciliation")
        self.assertTrue(Path(interrupted["worktree"]).is_dir())
        self.assertEqual(interrupted["requested_candidate_sha"], self.git("rev-parse", "HEAD"))
        self.cli("verify", *args, status=1)
        self.assertEqual(marker.read_text().splitlines(), ["start"])

    def test_failed_delivery_replays_failure_and_rejects_changed_input(self):
        args = ["--candidate", "HEAD", "--run-id", "retry-me"]
        self.cli("run", *args, status=1)
        failed = self.status("retry-me")
        self.assertEqual(failed["status"], "failed")
        self.cli("run", *args, status=1)
        self.assertEqual(self.status("retry-me"), failed)
        result = self.cli("run", *args, "--timeout", "20", status=1)
        self.assertIn("different request", result.stderr)
        (self.repo / "result.txt").write_text("ok")
        self.git("commit", "-am", "new revision")
        result = self.cli("run", *args, status=1)
        self.assertIn("different candidate", result.stderr)
        self.assertEqual(self.status("retry-me"), failed)
        self.cli("run", "--candidate", "HEAD", "--run-id", "new-attempt")

    def test_status_reads_legacy_evidence_and_rejects_invalid_id(self):
        folder = self.repo / ".git" / "aep" / "runs" / "legacy"
        folder.mkdir(parents=True)
        legacy = {"run_id": "legacy", "status": "awaiting_human_review", "approved": False}
        (folder / "evidence.json").write_text(json.dumps(legacy))
        self.assertEqual(self.status("legacy"), legacy)
        self.cli("run", "--candidate", "HEAD", "--run-id", "legacy", status=1)
        self.assertEqual(self.status("legacy"), legacy)
        self.cli("verify", "--base", "HEAD", "--candidate", "HEAD", "--run-id", "../invalid", status=1)

    def test_dispatch_is_idempotent_and_rejects_revision_change(self):
        self.cli("dispatch")
        (self.repo / "result.txt").write_text("ok")
        self.git("commit", "-am", "candidate")
        self.cli("dispatch", status=1)
        self.assertEqual(self.git("ls-remote", "origin", "refs/heads/aep-task/sample").split()[0], self.base)

    def test_failed_check_and_verified_candidate(self):
        result = self.cli("run", "--candidate", "HEAD", status=1)
        failed = self.evidence(result)
        self.assertEqual(failed["check_exit_code"], 1)
        self.assertEqual(failed["status"], "failed")
        (self.repo / "result.txt").write_text("ok")
        self.git("commit", "-am", "candidate")
        passed = self.evidence(self.cli("run", "--candidate", "HEAD"))
        self.assertEqual(passed["spec_sha"], self.base)
        self.assertEqual(passed["candidate_sha"], self.git("rev-parse", "HEAD"))
        self.assertEqual(passed["status"], "awaiting_human_review")
        self.assertFalse(passed["approved"])
        self.assertEqual(passed["changed_files"], ["result.txt"])

    def test_changed_acceptance_is_blocked_before_execution(self):
        (self.spec / "task.json").write_text("{}")
        self.git("commit", "-am", "change acceptance")
        result = self.cli("verify", "--base", self.base, "--candidate", "HEAD", status=1)
        self.assertIn("outside the task", self.evidence(result)["error"])

    def test_adapter_contract_with_local_standins(self):
        bindir = self.repo.parent / "bin"
        bindir.mkdir()
        for name, script in {
            "codex": "import pathlib, sys\nassert 'Implement' in sys.stdin.read()\npathlib.Path('result.txt').write_text('ok')",
            "claude": "import sys\nassert 'DIFF' in sys.stdin.read()\nprint('Review fixture: assess manually')",
        }.items():
            entry = bindir / name
            entry.write_text("#!" + sys.executable + "\n" + script + "\n")
            entry.chmod(0o755)
        self.env["PATH"] = str(bindir) + os.pathsep + self.env["PATH"]
        result = self.cli("run", "--implementer", "codex", "--reviewer", "claude")
        report = self.evidence(result)
        self.assertEqual(report["review_status"], "findings_recorded")
        self.assertFalse(report["approved"])
        self.assertNotEqual(report["candidate_sha"], self.base)
        self.assertEqual((self.repo / "result.txt").read_text(), "old")
