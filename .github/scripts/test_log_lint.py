#!/usr/bin/env python3
"""Tests for log_lint.py: each rule, what it leaves alone, and the
explicit path list. Run: python3 .github/scripts/test_log_lint.py"""

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import log_lint  # noqa: E402


def findings(code: str, path: str = "crates/x/src/lib.rs") -> list[str]:
    return log_lint.check_source(path, code)


class Macros(unittest.TestCase):
    def test_plain_tracing_macros_are_refused(self):
        for line in [
            'tracing::warn!("x {e}");',
            'warn!("x");',
            "tracing::info!(n, \"y\");",
            "debug!{ \"z\" }",
            "tracing::event!(Level::INFO, \"x\");",
            "let _s = tracing::info_span!(\"fetch\").entered();",
            "tracing::trace!(target: \"t\", \"x\");",
        ]:
            with self.subTest(line=line):
                self.assertEqual(len([f for f in findings(f"fn f() {{ {line} }}") if "plain" in f]), 1, line)

    def test_log_event_and_lookalikes_pass(self):
        for line in [
            "lodestar_log::log_event!(E, n = 1u64);",
            "log_event!(events::X);",
            "my_warn!(x);",
            "self.event!(x);",
            "let warn = 1; let info = warn + 1;",
        ]:
            with self.subTest(line=line):
                self.assertEqual(findings(f"fn f() {{ {line} }}"), [], line)

    def test_comments_and_strings_are_ignored(self):
        code = '''
        // tracing::warn!("in a comment")
        /* info!("in a block /* nested */ comment") */
        /// Doc: `tracing::error!(...)` is refused.
        fn f() { let s = "tracing::warn!(\\"x\\")"; let r = r#"debug!("y")"#; let c = '%'; }
        '''
        self.assertEqual(findings(code), [])

    def test_tests_are_left_alone(self):
        code = '''
        fn real() {}
        #[cfg(test)]
        mod tests {
            fn helper() { tracing::warn!("fine in tests"); }
            #[test]
            fn t() { tracing::info!(error = %e, "x"); }
        }
        #[cfg(test)]
        use tracing::info;
        fn after() { tracing::warn!("caught"); }
        '''
        got = findings(code)
        self.assertEqual(len(got), 1, got)
        self.assertIn(":11:", got[0])
        self.assertTrue(log_lint.is_test_file(Path("crates/a/src/settings/logging_tests.rs")))
        self.assertTrue(log_lint.is_test_file(Path("crates/a/src/logging/tests.rs")))
        self.assertTrue(log_lint.is_test_file(Path("crates/a/tests/canary.rs")))
        self.assertFalse(log_lint.is_test_file(Path("crates/a/src/lib.rs")))

    def test_the_fallback_is_the_one_exemption(self):
        code = 'fn fallback() { tracing::warn!(target: "lodestar_log", "{line}"); }'
        self.assertEqual(findings(code, "crates/log/src/dispatch.rs"), [])
        self.assertEqual(len(findings(code, "crates/log/src/lib.rs")), 1)


class Instrument(unittest.TestCase):
    def test_instrument_needs_skip_all(self):
        refused = [
            "#[instrument]\nfn f(name: &str) {}",
            "#[tracing::instrument(level = \"debug\")]\nfn f(name: &str) {}",
            "#[instrument(skip(name, path))]\nfn f(name: &str, path: &Path) {}",
            "#[instrument(fields(x = 1))]\nfn f(name: &str) {}",
            "#[instrument(\n    level = \"debug\",\n    skip(a)\n)]\nfn f(a: u8, b: u8) {}",
        ]
        for code in refused:
            with self.subTest(code=code):
                self.assertEqual(len([f for f in findings(code) if "skip_all" in f]), 1, code)
        allowed = [
            "#[instrument(skip_all)]\nfn f(name: &str) {}",
            "#[tracing::instrument(skip_all, fields(id = n))]\nfn f(n: u8) {}",
            "#[instrument(level = \"debug\", skip_all)]\nfn f() {}",
        ]
        for code in allowed:
            with self.subTest(code=code):
                self.assertEqual(findings(code), [], code)


class ErrorFields(unittest.TestCase):
    def test_untyped_error_fields_are_refused(self):
        for args in ["error = %e", "err = ?err", "%e", "?err", "% e", "%error", "?io_err", "cause = %x", "error = ?self.last"]:
            with self.subTest(args=args):
                code = f"#[instrument(skip_all, fields({args}))]\nfn f() {{}}"
                self.assertTrue(any("error field" in f for f in findings(code)), args)

    def test_operators_and_other_fields_pass(self):
        for code in [
            "fn f() { let r = a % e; let x = g(e)?; let y = h()?; }",
            "fn f() -> Result<(), E> { let n = m % 2; Ok(()) }",
            "#[instrument(skip_all, fields(n = %count))]\nfn f() {}",
            "fn f() { lodestar_log::log_event!(E, error = lodestar_log::Error::new(&e)); }",
        ]:
            with self.subTest(code=code):
                self.assertEqual(findings(code), [], code)


class Paths(unittest.TestCase):
    def test_a_missing_listed_path_fails(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "crates/a/src").mkdir(parents=True)
            (root / "crates/a/src/lib.rs").write_text("fn f() {}\n")
            (root / "crates/a/src/tests.rs").write_text('fn t() { tracing::warn!("x"); }\n')
            self.assertEqual(log_lint.run(root, ["crates/a/src/"]), [])
            got = log_lint.run(root, ["crates/a/src/", "crates/b/src/x.rs", "crates/c/"])
            self.assertEqual(len(got), 2, got)
            self.assertTrue(all("missing" in g for g in got))
            (root / "crates/a/src/lib.rs").write_text('fn f() { tracing::warn!("x"); }\n')
            self.assertEqual(len(log_lint.run(root, ["crates/a/src/"])), 1)

    def test_every_enforced_path_exists_and_is_clean(self):
        root = Path(__file__).resolve().parents[2]
        self.assertEqual(log_lint.run(root), [])

    def test_unlisted_paths_are_not_checked(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "crates/a/src").mkdir(parents=True)
            (root / "crates/a/src/lib.rs").write_text('fn f() { tracing::warn!("x"); }\n')
            (root / "crates/b").mkdir(parents=True)
            (root / "crates/b/ok.rs").write_text("fn f() {}\n")
            self.assertEqual(log_lint.run(root, ["crates/b/ok.rs"]), [])


if __name__ == "__main__":
    unittest.main()
