"""Independent, synthetic acceptance cases pinned to the dispatched spec."""
import importlib.util
import unittest
from pathlib import Path

path = Path(__file__).resolve().parents[2] / "examples/marketdata/aggregate.py"
module_spec = importlib.util.spec_from_file_location("aep_market_fixture", path)
fixture = importlib.util.module_from_spec(module_spec)
module_spec.loader.exec_module(fixture)


def event(origin, number, quantity):
    return {"source": origin, "event_id": number, "time": 1000, "quantity": quantity}


class Acceptance(unittest.TestCase):
    def test_window(self):
        a = event("A", "E1", 10)
        b = event("A", "E2", 5)
        cases = [
            ("AC-1", [a, b], 15, 2),
            ("AC-2", [a, b, a], 15, 2),
            ("AC-3", [a, event("A", "E3", 10)], 20, 2),
            ("AC-4", [a, event("B", "E1", 5)], 15, 2),
            ("AC-5", [a, event("A", "E1", 12)], 10, 1),
            ("AC-6", [], 0, 0),
        ]
        for label, rows, quantity, count in cases:
            with self.subTest(criterion=label):
                self.assertEqual(fixture.aggregate(rows), {"quantity": quantity, "count": count})


if __name__ == "__main__":
    unittest.main(verbosity=2)
