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
            "::tracing::warn!(\"x\");",
            "warn !(\"x\");",
            "tracing :: error ! (\"x\");",
            "crate::tracing::warn!(\"x\");",
            "self::tracing::info!(\"x\");",
            "super::tracing::debug!(\"x\");",
            "tracing::log::warn!(\"x\");",
        ]:
            with self.subTest(line=line):
                self.assertEqual(len([f for f in findings(f"fn f() {{ {line} }}") if "plain" in f]), 1, line)

    def test_imports_that_get_around_the_rule_are_refused(self):
        for line in [
            "use tracing::{info, warn};",
            "use tracing::warn;",
            "use tracing as t;",
            "use ::tracing::error;",
            "use tracing::*;",
            "extern crate tracing as t;",
            "use tracing::instrument as trace_it;",
            "pub use tracing;",
            "use tracing::{self as t};",
            "use {tracing::warn};",
            "use ::{tracing as t};",
            "use tracing::log::warn;",
            "use tracing::{log};",
            "pub(crate) use crate::tracing::warn;",
            "use self::{tracing::{info}};",
        ]:
            with self.subTest(line=line):
                self.assertEqual(len(findings(line)), 1, line)
        for line in [
            "use tracing::Level;",
            "use tracing::field::{Field, Visit};",
            "use tracing_subscriber::layer::Layer;",
            "use {tracing::Level, std::fmt};",
            "use crate::settings::{self, log};",
            "use std::io::{self as io2};",
        ]:
            with self.subTest(line=line):
                self.assertEqual(findings(line), [], line)

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
            "#[::tracing::instrument]\nfn f(name: &str) {}",
            "#[cfg_attr(feature = \"x\", instrument(level = \"debug\"))]\nfn f(name: &str) {}",
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
        # `err` and `ret` record the error and the return value.
        for code in [
            "#[instrument(skip_all, err)]\nfn f() -> Result<(), E> {}",
            "#[instrument(skip_all, ret)]\nfn f() -> u8 {}",
            "#[instrument(skip_all, err(Display))]\nfn f() -> Result<(), E> {}",
        ]:
            with self.subTest(code=code):
                self.assertEqual(len(findings(code)), 1, code)
        self.assertEqual(findings("#[derive(Debug)]\nstruct Instrumented;"), [])


class ErrorFields(unittest.TestCase):
    def test_untyped_error_fields_are_refused(self):
        for args in ["error = %e", "err = ?err", "%e", "?err", "% e", "%error", "?io_err", "cause = %x", "error = ?self.last", "reason = %e", "detail = ?err"]:
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
    def tree(self, files: dict[str, str]) -> Path:
        d = tempfile.mkdtemp()
        root = Path(d)
        for rel, text in files.items():
            (root / rel).parent.mkdir(parents=True, exist_ok=True)
            (root / rel).write_text(text)
        return root

    def test_every_crate_is_checked(self):
        root = self.tree({
            "crates/a/src/lib.rs": 'fn f() { tracing::warn!("x"); }\n',
            "crates/a/src/tests.rs": 'fn t() { tracing::warn!("x"); }\n',
            "crates/a/tests/it.rs": 'fn t() { tracing::warn!("x"); }\n',
            "crates/b/src/deep/mod.rs": 'fn f() { info!("y"); }\n',
        })
        got = log_lint.run(root, [])
        self.assertEqual(len(got), 2, got)

    def test_not_yet_swept_files_skip_only_the_plain_macro_rules(self):
        root = self.tree({
            "crates/a/src/old.rs": 'fn f() { tracing::warn!("x"); }\n#[instrument]\nfn g(a: u8) {}\n',
        })
        got = log_lint.run(root, ["crates/a/src/old.rs"])
        self.assertEqual(len(got), 1, got)
        self.assertIn("skip_all", got[0])

    def test_the_list_only_shrinks(self):
        root = self.tree({"crates/a/src/done.rs": "fn f() {}\n"})
        got = log_lint.run(root, ["crates/a/src/done.rs", "crates/a/src/gone.rs"])
        self.assertEqual(len(got), 2, got)
        self.assertTrue(any("no plain macro left" in g for g in got))
        self.assertTrue(any("missing" in g for g in got))

    def test_a_renamed_tracing_dependency_is_refused(self):
        root = self.tree({
            "crates/a/Cargo.toml": '[dependencies]\nlog2 = { package = "tracing", version = "0.1" }\n',
            "crates/a/src/lib.rs": "fn f() {}\n",
        })
        self.assertEqual(len(log_lint.run(root, [])), 1)

    def test_a_log_dependency_is_refused(self):
        for manifest in [
            '[dependencies]\nlog = "0.4"\n',
            '[dependencies]\nlogs = { package = "log", version = "0.4" }\n',
            '[dependencies.log]\nversion = "0.4"\n',
            '[target.\'cfg(windows)\'.dependencies]\nlog = "0.4"\n',
            '[dependencies]\nlog.workspace = true\n',
            '[dependencies]\nlog.version = "0.4"\n',
            '[dependencies]\n"log" = "0.4"\n',
        ]:
            with self.subTest(manifest=manifest):
                root = self.tree({"crates/a/Cargo.toml": manifest, "crates/a/src/lib.rs": "fn f() {}\n"})
                self.assertEqual(len(log_lint.run(root, [])), 1, manifest)
        root = self.tree({
            "crates/a/Cargo.toml": '[dependencies]\nlogos = "1"\n[dev-dependencies]\nlog = "0.4"\n',
            "crates/a/src/lib.rs": "fn f() {}\n",
        })
        self.assertEqual(log_lint.run(root, []), [])

    def test_the_list_may_not_grow_against_the_base(self):
        base = 'NOT_YET_SWEPT = [\n    "a.rs",\n    "b.rs",\n]\n'
        self.assertEqual(log_lint.listed_in(base), ["a.rs", "b.rs"])
        self.assertEqual(log_lint.grown(["a.rs", "b.rs"], ["a.rs"]), [])
        self.assertEqual(len(log_lint.grown(["a.rs"], ["a.rs", "c.rs"])), 1)
        # A base without the script (or the list) has nothing to compare.
        self.assertEqual(log_lint.grown(None, ["c.rs"]), [])
        self.assertIsNone(log_lint.listed_in("X = 1\n"))

    def test_every_dependency_table_is_walked(self):
        for manifest in [
            '[target.\'cfg(target_os = "windows")\'.dependencies.log]\nversion = "0.4"\n',
            '[target.\'cfg(unix)\'.build-dependencies]\nlog = "0.4"\n',
            '[build-dependencies]\nlog = "0.4"\n',
            'dependencies = { log = "0.4" }\n',
            '[dependencies]\nx = { package = "log", version = "0.4" }\n',
            '[target.\'cfg(windows)\'.dependencies]\nt = { package = "tracing", version = "0.1" }\n',
            'build-dependencies = { t = { package = "tracing" } }\n',
            '[dependencies]\nwslog = { workspace = true }\n',
        ]:
            with self.subTest(manifest=manifest):
                root = self.tree({
                    "Cargo.toml": '[workspace.dependencies]\nwslog = { package = "log", version = "0.4" }\n',
                    "crates/a/Cargo.toml": manifest,
                    "crates/a/src/lib.rs": "fn f() {}\n",
                })
                self.assertEqual(len(log_lint.run(root, [])), 1, manifest)
        # Dev-dependencies, under a target too, `tracing` by its name, and
        # a dependency named `log` that is another crate.
        root = self.tree({
            "crates/a/Cargo.toml": '[dependencies]\nlog = { package = "x", version = "1" }\n',
            "crates/a/src/lib.rs": "fn f() {}\n",
        })
        self.assertEqual(log_lint.run(root, []), [])
        root = self.tree({
            "crates/a/Cargo.toml": (
                '[dependencies]\ntracing = "0.1"\n'
                '[target.\'cfg(unix)\'.dev-dependencies]\nlog = "0.4"\n'
                '[dev-dependencies]\nt = { package = "tracing" }\n'
            ),
            "crates/a/src/lib.rs": "fn f() {}\n",
        })
        self.assertEqual(log_lint.run(root, []), [])

    def test_the_bench_manifest_is_checked(self):
        root = self.tree({"bench/Cargo.toml": '[dependencies]\nlog = "0.4"\n'})
        self.assertEqual(len(log_lint.run(root, [])), 1)

    def test_an_unparseable_manifest_fails_closed(self):
        root = self.tree({"crates/a/Cargo.toml": "[dependencies\nlog = \n", "crates/a/src/lib.rs": "fn f() {}\n"})
        got = log_lint.run(root, [])
        self.assertEqual(len(got), 1, got)
        self.assertIn("doesn't parse", got[0])

    def test_an_unreadable_base_list_fails_closed(self):
        exempt = {("crates/log/src/dispatch.rs", "tracing"): "x"}
        ok = 'NOT_YET_SWEPT = ["a.rs"]\nEXEMPT = {("crates/log/src/dispatch.rs", "tracing"): "x"}\n'
        self.assertEqual(log_lint.against_base(ok, ["a.rs"], exempt), [])
        # No script at the base: nothing to compare.
        self.assertEqual(log_lint.against_base(None, ["a.rs"], exempt), [])
        for source in [
            "NOT_YET_SWEPT = sorted(FILES)\nEXEMPT = {}\n",
            'EXEMPT = {}\n',
            "NOT_YET_SWEPT = [\n",
            'NOT_YET_SWEPT = ["a.rs"]\nEXEMPT = dict(x=1)\n',
        ]:
            with self.subTest(source=source):
                self.assertNotEqual(log_lint.against_base(source, ["a.rs"], exempt), [], source)

    def test_exempt_may_not_grow_against_the_base(self):
        base = 'NOT_YET_SWEPT = []\nEXEMPT = {("a.rs", "tracing"): "why"}\n'
        self.assertEqual(log_lint.against_base(base, [], {("a.rs", "tracing"): "why"}), [])
        self.assertEqual(log_lint.against_base(base, [], {}), [])
        got = log_lint.against_base(base, [], {("a.rs", "tracing"): "why", ("b.rs", "tracing"): "new"})
        self.assertEqual(len(got), 1, got)
        self.assertIn("EXEMPT", got[0])

    def test_a_base_that_doesnt_resolve_fails(self):
        root = Path(__file__).resolve().parents[2]
        with self.assertRaises(log_lint.BaseUnreadable):
            log_lint.base_source(root, "refs/heads/no-such-branch-for-log-lint")

    def test_the_repository_is_clean(self):
        root = Path(__file__).resolve().parents[2]
        self.assertEqual(log_lint.run(root), [])


if __name__ == "__main__":
    unittest.main()
