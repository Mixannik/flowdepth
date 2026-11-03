"""Regression tests for verdicts and safe selection; no Cargo/GUI/network."""
import os
from pathlib import Path
import sys
import tempfile
import unittest

from quality_gate import documentation, documentation_only, plan, run_check, test_verdict, verdict


class GateTests(unittest.TestCase):
    def test_docs_allowlist_excludes_code_and_infrastructure(self):
        for path in ["docs/state.md", "README.md"]:
            self.assertTrue(documentation(path))
        for path in ["Cargo.toml", "docs/test.py", ".github/workflows/pr-ci.yml",
                     "scripts/README.md", "src/main.rs"]:
            self.assertFalse(documentation(path))

    def test_nonzero_is_failure_even_with_success_text(self):
        self.assertEqual(test_verdict(1, "test result: ok. 10 passed; 0 failed; 0 ignored;", 1)[0], "FAIL")

    def test_empty_or_mixed_diff_does_not_skip_rust(self):
        self.assertFalse(documentation_only([]))
        self.assertFalse(documentation_only(["docs/state.md", "src/main.rs"]))
        self.assertTrue(documentation_only(["docs/state.md", "README.md"]))

    def test_zero_tests_never_pass(self):
        for output in ["", "test result: ok. 0 passed; 0 failed; 0 ignored;"]:
            self.assertEqual(test_verdict(0, output, 1)[0], "NOT VERIFIED")

    def test_missing_named_scenario_never_passes(self):
        self.assertEqual(test_verdict(0, "test result: ok. 10 passed; 0 failed; 0 ignored;", 1, ["critical"])[0], "NOT VERIFIED")

    def test_real_summary_and_named_execution(self):
        text = "test module::critical ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored;"
        self.assertEqual(test_verdict(0, text, 1, ["critical"])[0], "PASS")

    def test_ignored_is_not_verified(self):
        self.assertEqual(test_verdict(0, "test result: ok. 4 passed; 0 failed; 2 ignored;", 1)[0], "NOT VERIFIED")

    def test_workspace_ignored_remain_separate_from_executed_tests(self):
        with tempfile.TemporaryDirectory() as folder:
            command = [sys.executable, "-c", "print('test result: ok. 4 passed; 0 failed; 2 ignored;')"]
            result = run_check("workspace", command, Path(folder), os.environ.copy(), 10, 1)
            self.assertEqual(result["status"], "PASS")
            self.assertEqual(result["ignored"], 2)

    def test_failure_dominates_unverified(self):
        self.assertEqual(verdict([dict(status="PASS"), dict(status="NOT VERIFIED")]), "NOT VERIFIED")
        self.assertEqual(verdict([dict(status="FAIL"), dict(status="NOT VERIFIED")]), "FAIL")

    def test_modes_share_fd001_and_smoke_and_have_unique_names(self):
        fast = plan("fast", "cargo")
        full = plan("full", "cargo")
        self.assertEqual(full[:len(fast)], fast)
        names = [r[0] for r in full]
        self.assertEqual(len(names), len(set(names)))
        self.assertIn("fd001-unbounded", names)
        self.assertEqual(len([n for n in names if n.startswith("smoke-")]), 4)
        self.assertIn("workspace", names)
        self.assertEqual(plan("fast", "cargo", docs_only=True), [])

    def test_online_flag_only_removes_offline(self):
        offline = plan("full", "cargo")
        online = plan("full", "cargo", online=True)
        self.assertEqual([(n, [x for x in c if x != "--offline"], m, e) for n,c,m,e in offline], online)

    def test_actual_subprocess_failure_is_reported(self):
        with tempfile.TemporaryDirectory() as folder:
            result = run_check("failure", [sys.executable, "-c", "raise SystemExit(7)"],
                               Path(folder), os.environ.copy(), 10)
            self.assertEqual(result["status"], "FAIL")
            self.assertEqual(result["exit_code"], 7)

    def test_missing_tool_and_zero_unittest_are_not_verified(self):
        with tempfile.TemporaryDirectory() as folder:
            result = run_check("missing", [str(Path(folder)/"no-such-command")],
                               Path(folder), os.environ.copy(), 10)
            self.assertEqual(result["status"], "NOT VERIFIED")
        self.assertEqual(test_verdict(0, "Ran 0 tests", -1)[0], "NOT VERIFIED")


if __name__ == "__main__":
    unittest.main()
