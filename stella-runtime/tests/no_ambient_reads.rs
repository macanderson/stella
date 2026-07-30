//! The crate's central invariant, enforced executably.
//!
//! `stella-runtime` exists so that N sessions with N workspace roots and N
//! trust postures can be assembled in one process. That property survives
//! exactly as long as no construction step consults process-global state,
//! because a process global is by definition the same for all N. A doc
//! comment saying so is worth nothing the first time someone reaches for
//! `std::env::var` to avoid threading one more parameter through — so this
//! test reads the crate's own sources and fails if they do.
//!
//! Lives in `tests/` rather than a `#[cfg(test)]` module on purpose: an
//! in-crate test would have to exempt its own needles from the scan, and an
//! exemption list is the crack the next violation slips through.

use std::path::{Path, PathBuf};

/// Runtime reads of ambient process state. `env!` (compile-time, cargo's own
/// vars) is deliberately absent: it is resolved by the compiler and cannot
/// differ between two sessions in one process, which is the property under
/// test.
const FORBIDDEN: &[&str] = &[
    "std::env::var",
    "std::env::var_os",
    "env::var(",
    "env::var_os(",
    "current_dir",
    "std::env::vars",
    "set_current_dir",
];

fn source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).expect("crate src/ is readable");
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            source_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_ambient_reads() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    source_files(&src, &mut files);
    assert!(!files.is_empty(), "found no sources to scan under {src:?}");

    let mut violations = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("source file is valid UTF-8");
        for (index, line) in text.lines().enumerate() {
            // A line that merely *names* the rule in prose is not a read.
            if line.trim_start().starts_with("//") {
                continue;
            }
            for needle in FORBIDDEN {
                if line.contains(needle) {
                    violations.push(format!(
                        "{}:{}: reads ambient process state (`{needle}`)\n    {}",
                        file.display(),
                        index + 1,
                        line.trim()
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "stella-runtime must not read process-global state — take it as an explicit \
         `RuntimeSpec` field instead, so two sessions in one process can disagree about \
         it:\n\n{}",
        violations.join("\n")
    );
}
