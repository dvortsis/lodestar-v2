//! Golden corpus: raw info dictionaries and the parse each must produce.
//! See `tests/golden/README.md`; `UPDATE_GOLDEN=1` rewrites the expectations.

use std::fmt::Write;
use std::fs;
use std::path::Path;

use lodestar_metainfo::{FileAttrs, Info};

fn render(info: &Info) -> String {
    let mut out = String::new();
    let w = &mut out;
    writeln!(w, "version: {:?}", info.version).unwrap();
    writeln!(
        w,
        "name: {:?} ({})",
        info.name.decode(),
        info.name.encoding.name()
    )
    .unwrap();
    writeln!(w, "path encoding: {}", info.path_encoding.name()).unwrap();
    writeln!(w, "total size: {}", info.total_size).unwrap();
    writeln!(w, "piece count: {}", info.piece_count).unwrap();
    writeln!(w, "warnings: {:?}", info.warnings).unwrap();
    for f in &info.files {
        let attrs: String = [
            (FileAttrs::PADDING, 'p'),
            (FileAttrs::HIDDEN, 'h'),
            (FileAttrs::EXECUTABLE, 'x'),
            (FileAttrs::SYMLINK, 'l'),
        ]
        .iter()
        .filter(|(a, _)| f.attrs.contains(*a))
        .map(|(_, c)| *c)
        .collect();
        write!(
            w,
            "file: {:?} size={} attrs={attrs:?}",
            info.path_string(f),
            f.size
        )
        .unwrap();
        if let Some(target) = &f.symlink_target {
            let target: Vec<_> = target
                .iter()
                .map(|s| info.path_encoding.decode(s))
                .collect();
            write!(w, " target={:?}", target.join("/")).unwrap();
        }
        if f.pieces_root.is_some() {
            w.push_str(" root");
        }
        w.push('\n');
    }
    out
}

#[test]
fn golden_corpus() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    let mut cases: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "info"))
        .collect();
    cases.sort();
    assert!(!cases.is_empty(), "no golden cases in {}", dir.display());
    let mut failures = Vec::new();
    for case in &cases {
        let raw = fs::read(case).unwrap();
        let got = match Info::parse(&raw) {
            Ok(info) => render(&info),
            Err(e) => format!("error: {e}\n"),
        };
        let expected_path = case.with_extension("expected");
        if update {
            fs::write(&expected_path, &got).unwrap();
            continue;
        }
        let expected = fs::read_to_string(&expected_path)
            .unwrap_or_else(|_| panic!("{} is missing", expected_path.display()));
        if got != expected {
            failures.push(format!(
                "{}\n--- expected\n{expected}--- got\n{got}",
                case.display()
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
