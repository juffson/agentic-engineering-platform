import tempfile
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from aep_memory import MAX_CONTEXT_BYTES, MAX_ITEMS, Memory, pack


class MemoryTest(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.path = Path(temp.name)
        self.memory = Memory(self.path)

    def test_scopes_and_historical_correction(self):
        mem = self.memory
        def add(task, spec, candidate, text):
            return mem.add_human(task, spec, candidate, "decision", text)
        project = add("", "", "", "project convention")
        task = add("one", "s", "", "task decision")
        previous = add("one", "s", "c", "old note")
        updated = mem.add_human("one", "s", "c", "decision", "corrected note", previous)
        add("two", "s", "", "other task")
        add("one", "new-spec", "c", "other spec")
        add("one", "s", "other-code", "other code")
        self.assertEqual({n["id"] for n in mem.context("one", "s", "c")}, {project, task, updated})
        self.assertEqual(mem.get(previous)["body"], "old note")
        self.assertEqual(mem.get(updated)["replaces"], previous)
        with self.assertRaises(ValueError):
            mem.add_human("one", "s", "", "decision", "wrong scope", updated)
        self.assertEqual(Memory(self.path / "separate").context("one", "s", "c"), [])

    def test_concurrent_writes_and_replacements(self):
        first = self.memory.add_human("", "", "", "decision", "original")
        def write(i):
            return Memory(self.path).add_human("", "", "", "decision", str(i))
        def replace(i):
            try:
                return Memory(self.path).add_human("", "", "", "decision", str(i), first)
            except ValueError:
                return None
        with ThreadPoolExecutor(max_workers=5) as pool:
            ids = list(pool.map(write, range(12)))
            replacements = list(pool.map(replace, range(5)))
        self.assertEqual(len(set(ids)), 12)
        self.assertEqual(sum(item is not None for item in replacements), 1)

    def test_atomic_publication_replay_and_bounded_context(self):
        record = {"run_id": "run", "task": "one", "spec_sha": "s"}
        payload = {"note": "tested locally", "question": "which timestamp is canonical?"}
        ids = self.memory.publish("implementer", record, "c", payload)
        self.assertEqual(self.memory.publish("implementer", record, "c", payload), ids)
        with self.assertRaises(ValueError):
            self.memory.publish("implementer", record, "c", dict(payload, question="different"))
        with self.assertRaises(ValueError):
            self.memory.publish("reviewer", record, "c", {"note": "review", "question": "x"*1001})
        self.assertEqual({n["id"] for n in self.memory.context("one", "s", "c")}, set(ids))
        self.assertEqual([n["id"] for n in self.memory.context("one", "s", "other")], [ids[1]])
        for i in range(20):
            self.memory.add_human("", "", "", "decision", "量"*800 + str(i))
        selection = self.memory.context("one", "s", "c", "run")
        self.assertEqual({n["id"] for n in selection[:2]}, set(ids))
        self.assertLessEqual(len(selection), MAX_ITEMS)
        self.assertLessEqual(len(pack(selection).encode("utf-8")), MAX_CONTEXT_BYTES)
