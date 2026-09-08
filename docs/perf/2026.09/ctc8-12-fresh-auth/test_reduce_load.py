"""Small generated fixtures test the offline reducer, without measurement tools."""

import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import reduce_load


class ReduceLoadTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.before = Path(temporary.name) / "before"
        self.after = Path(temporary.name) / "after"
        for root, phase in ((self.before, 0), (self.after, 10)):
            for credential in reduce_load.CREDENTIALS:
                for repetition in reduce_load.REPETITIONS:
                    directory = root / "journey" / "throughput" / f"{credential}-{repetition}"
                    directory.mkdir(parents=True)
                    steps = []
                    for layer in reduce_load.LAYERS:
                        for concurrency in reduce_load.CONCURRENCY:
                            step = {"layer": layer, "concurrency": concurrency}
                            for edge_number, edge in enumerate(reduce_load.EDGES):
                                stem = f"{layer}-c{concurrency}-{edge}"
                                value = phase + repetition + edge_number
                                step[edge] = f"sample-{stem}.json"
                                (directory / step[edge]).write_text(json.dumps({
                                    "taken_at_unix_ms": 1000 + edge_number,
                                }))
                                (directory / f"load-{stem}.txt").write_text(
                                    f"{value}.0 {value + 1}.0 {value + 2}.0 1/100 42\n")
                                (directory / f"memory-machine-{stem}.txt").write_text(
                                    f"MemTotal: 99999 kB\nMemAvailable: {value * 100} kB\n")
                                (directory / f"memory-host-{stem}.txt").write_text(
                                    f"{value * 1024}\n")
                            steps.append(step)
                    (directory / "index.json").write_text(json.dumps({
                        "schema": "wamn-throughput/v0.1", "source": ("a" if phase == 0 else "b") * 40,
                        "concurrency": list(reduce_load.CONCURRENCY),
                        "layers": [{"layer": layer} for layer in reduce_load.LAYERS],
                        "steps": steps,
                    }))
        self.directory = self.before / "journey" / "throughput" / "service-1"

    def test_summary_keeps_coordinates_units_and_source_files(self):
        result = reduce_load.reduce_runs(self.before, self.after)
        self.assertEqual(len(result["samples"]), 432)
        self.assertEqual(len(result["summary"]), 144)
        group = result["summary"][0]
        self.assertEqual({key: group[key] for key in
                          ("run", "credential", "layer", "concurrency", "edge", "repetitions")},
                         {"run": "before", "credential": "service", "layer": "route",
                          "concurrency": 1, "edge": "before", "repetitions": [1, 2, 3]})
        self.assertEqual(group["statistics"]["load1"], {"min": 1.0, "median": 2.0, "max": 3.0})
        self.assertEqual(group["statistics"]["MemAvailable"], {"min": 100, "median": 200, "max": 300})
        self.assertEqual(group["statistics"]["memory.current"],
                         {"min": 1024, "median": 2048, "max": 3072})
        self.assertIn("KiB", result["units"]["MemAvailable"])
        self.assertIn("not peak or RSS", result["units"]["memory.current"])
        self.assertEqual(result["samples"][0]["source"], "a" * 40)
        self.assertEqual(result["samples"][0]["source_files"]["load"],
                         "journey/throughput/service-1/load-route-c1-before.txt")
        candidate = next(item for item in result["summary"]
                         if item["run"] == "after" and item["edge"] == "after")
        self.assertEqual(candidate["statistics"]["load1"],
                         {"min": 12.0, "median": 13.0, "max": 14.0})

    def test_missing_sidecar_is_not_filled(self):
        path = self.directory / "memory-host-route-c1-after.txt"
        path.unlink()
        with self.assertRaisesRegex(ValueError, "memory-host-route-c1-after.txt: cannot read"):
            reduce_load.reduce_runs(self.before, self.after)

    def test_missing_repetition_is_an_error(self):
        (self.before / "journey/throughput/human-3/index.json").unlink()
        with self.assertRaisesRegex(ValueError, "human-3/index.json: cannot read"):
            reduce_load.reduce_runs(self.before, self.after)

    def test_incomplete_or_duplicate_grid_is_an_error(self):
        path = self.directory / "index.json"
        index = json.loads(path.read_text())
        for steps, message in ((index["steps"][:-1], "incomplete layer/concurrency grid"),
                               (index["steps"] + index["steps"][:1], "repeated step")):
            with self.subTest(message=message):
                path.write_text(json.dumps(index | {"steps": steps}))
                with self.assertRaisesRegex(ValueError, message):
                    reduce_load.reduce_runs(self.before, self.after)

    def test_invalid_metrics_are_errors(self):
        cases = (
            ("load-route-c1-before.txt", "NaN 2 3 1/100 42\n", "finite"),
            ("load-route-c1-before.txt", "1 2\n", "incomplete"),
            ("memory-machine-route-c1-before.txt", "MemAvailable: 123 bytes\n", "in kB"),
            ("memory-machine-route-c1-before.txt", "MemAvailable: 1 kB\nMemAvailable: 2 kB\n", "one MemAvailable"),
            ("memory-host-route-c1-before.txt", "-1\n", "in bytes"),
        )
        for name, invalid, message in cases:
            with self.subTest(name=name, message=message):
                path = self.directory / name
                original = path.read_text()
                path.write_text(invalid)
                with self.assertRaisesRegex(ValueError, message):
                    reduce_load.reduce_runs(self.before, self.after)
                path.write_text(original)

    def test_mixed_source_commits_are_an_error(self):
        path = self.directory / "index.json"
        index = json.loads(path.read_text())
        path.write_text(json.dumps(index | {"source": "c" * 40}))
        with self.assertRaisesRegex(ValueError, "different source commits"):
            reduce_load.reduce_runs(self.before, self.after)

    def test_invalid_sample_timestamp_is_an_error(self):
        path = self.directory / "sample-route-c1-after.json"
        path.write_text('{"taken_at_unix_ms": 1000}')
        with self.assertRaisesRegex(ValueError, "after timestamp must follow"):
            reduce_load.reduce_runs(self.before, self.after)

    def test_cli_rejects_same_root_without_partial_stdout(self):
        output = io.StringIO()
        error = io.StringIO()
        args = ["reduce_load.py", "--before", str(self.before), "--after", str(self.before)]
        with patch("sys.argv", args), contextlib.redirect_stdout(output), contextlib.redirect_stderr(error):
            with self.assertRaises(SystemExit) as stopped:
                reduce_load.main()
        self.assertEqual(stopped.exception.code, 2)
        self.assertEqual(output.getvalue(), "")
        self.assertIn("must be different", error.getvalue())


if __name__ == "__main__":
    unittest.main()
