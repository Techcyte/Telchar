"""Connected benchmark summary contracts; no timing assertions."""
import contextlib
import importlib.util
import io
import json
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location("summarize_reuse", Path(__file__).with_name("summarize-reuse.py"))
assert spec is not None and spec.loader is not None
summarize_reuse = importlib.util.module_from_spec(spec)
spec.loader.exec_module(summarize_reuse)


class SummarizeReuseTests(unittest.TestCase):
    def record(self, mode, value):
        return "MICROVM_BENCH " + json.dumps({"mode": mode, **dict.fromkeys((
            "job_reset_ms", "hook_start_ms", "connect_auth_ms", "input_transfer_ms",
            "after_input_through_result_ms", "reset_ms", "input_nar_bytes",
            "output_nar_bytes"), value)})

    def test_summary_excludes_seed_and_reports_observed_tail(self):
        text = "\n".join([self.record("seed", 1000), self.record("cold", 10),
                          self.record("cold", 30), self.record("warm", 2),
                          self.record("warm", 6)])
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            summarize_reuse.summarize(text)
        self.assertIn("cold: n=2\n  job_reset_ms: 20.000 / 30.000", output.getvalue())
        self.assertIn("warm: n=2\n  job_reset_ms: 4.000 / 6.000", output.getvalue())
        self.assertIn("median cold/warm ratio: 5.000x", output.getvalue())
        self.assertIn("not a p95/p99 estimate", output.getvalue())

    def test_incomplete_samples_are_rejected(self):
        with self.assertRaisesRegex(ValueError, "equal nonempty"):
            summarize_reuse.summarize(self.record("cold", 10))

    def test_empty_log_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "equal nonempty"):
            summarize_reuse.summarize("test failed")


if __name__ == "__main__":
    unittest.main()
