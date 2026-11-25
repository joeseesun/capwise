#!/usr/bin/env python3
import contextlib
import io
import json
import plistlib
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).resolve().parent))
import bench_perf


class LogParsing(unittest.TestCase):
    def test_reads_ansi_log_lines_and_finds_after(self):
        with tempfile.TemporaryDirectory() as root:
            sandbox = bench_perf.Sandbox(root, "s")
            sandbox.out.write_text(
                "\x1b[2m2026-11-07T17:22:09.000000Z\x1b[0m \x1b[32m INFO\x1b[0m a: auto-record start requested\n"
                "2026-11-07T17:22:10.250000Z  INFO b: Start gate admitted first video frame held_frames=0\n"
                "2026-11-07T17:22:14.000000Z  INFO c: auto-record stop requested\n"
                "2026-11-07T17:22:15.000000Z DEBUG d: editor frame number=0\n"
                "not a log line\n"
            )
            start = sandbox.find(r"auto-record start requested")
            gate = sandbox.find(r"Start gate admitted first video frame")
            self.assertEqual(bench_perf.ms(gate[0], start[0]), 1250.0)
            stop = sandbox.find(r"auto-record stop requested")
            frame = sandbox.find(r"editor frame number=0", stop[0])
            self.assertEqual(bench_perf.ms(frame[0], stop[0]), 1000.0)
            self.assertIsNone(sandbox.find(r"auto-record start requested", stop[0]))

    def test_seeded_store_skips_onboarding_and_captions(self):
        with tempfile.TemporaryDirectory() as root:
            store = bench_perf.json.loads(bench_perf.Sandbox(root, "s").root.joinpath("store").read_text())
            general = store["general_settings"]
            self.assertTrue(general["hasCompletedOnboarding"])
            self.assertTrue(general["hasCompletedStartup"])
            self.assertFalse(general["autoCaptionsAfterRecording"])
            self.assertEqual(general["recordingCountdown"], 0)


class BundleProvenance(unittest.TestCase):
    def test_uses_app_commit_and_separate_checkout(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            app = root / "Capwise.app"
            binary = app / "Contents" / "MacOS" / "Capwise"
            binary.parent.mkdir(parents=True)
            binary.write_bytes(b"abc")
            app_commit = "b" * 40
            with (app / "Contents" / "Info.plist").open("wb") as output:
                plistlib.dump(
                    {"CFBundleShortVersionString": "1.0", "CapwiseLocalTestCommit": app_commit},
                    output,
                    fmt=plistlib.FMT_BINARY,
                )
            result_path = root / "report.json"
            args = SimpleNamespace(
                app=app, stages="", work=root / "work", fixture=None, keep=False, out=result_path
            )
            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                bench_perf.run(args)
            meta = json.loads(result_path.read_text())["meta"]
            self.assertEqual(meta["app_commit"], app_commit)
            self.assertNotEqual(meta["checkout_commit"], app_commit)
            self.assertEqual(
                meta["app_binary_sha256"],
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            )
            self.assertIsNone(meta["cli_binary_sha256"])


class Verdicts(unittest.TestCase):
    def test_directions(self):
        self.assertEqual(bench_perf.verdict("record.start_to_first_frame_ms", 150), "✅")
        self.assertEqual(bench_perf.verdict("record.start_to_first_frame_ms", 250), "❌")
        self.assertEqual(bench_perf.verdict("export.app_speed_x", 6), "✅")
        self.assertEqual(bench_perf.verdict("export.app_speed_x", 4), "❌")
        self.assertEqual(bench_perf.verdict("export.app_speed_x", None), "")

    def test_median_ignores_missing(self):
        self.assertEqual(bench_perf.median([None, 3, 1, 2]), 2)
        self.assertIsNone(bench_perf.median([None]))


if __name__ == "__main__":
    unittest.main()
