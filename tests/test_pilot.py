"""Exercise the CLI against a disposable clone and bare dispatch repository."""
import json
import os
import subprocess
import sys
import tempfile
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
