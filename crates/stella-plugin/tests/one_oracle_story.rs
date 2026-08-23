//! The crate tells one story about who runs the oracle.
//!
//! #3511 was settled as Option 2: verification is delivered by an installed
//! plugin, the plugin runs its own oracle in its own process and **reports**
//! the flip and the measurements, and Stella neither executes that oracle nor
//! re-checks what came back. `manifest.rs` and `consent.rs` were corrected
//! then; seven doc comments in four sibling files still said the retired
//! thing, so the crate asserted both at once and a reader landing on either
//! one had no way to tell which was current (#3524).
//!
//! A doc comment is a claim like any other (CLAUDE.md § "A commit message is a
//! claim, not a fact"), and the failure mode here is silence: prose drifts back
//! without anything going red. So the ban is mechanical.
//!
//! # What this catches, and what it does not
//!
//! It matches over **whitespace-normalized** text rather than raw lines,
//! because two of the seven sites straddled a line break — `evidence.rs`'s
//! "and pure: the / host runs the oracle" and `error.rs`'s "a program the /
//! host runs" — and a line-oriented grep (the one the issue was filed with)
//! sees neither. A guard that a `rustfmt` reflow can defeat is not a guard.
//!
//! It is a phrase list, so it catches the claim in the wordings it has
//! actually been written in, not the claim in every wording. A differently
//! phrased regression still needs a reviewer; this closes the path where
//! nobody is looking.
//!
//! # What it deliberately does not assert
//!
//! Only two claims are retired. The host really does evaluate the
//! [`stella_plugin::VerdictRule`] (`stella_runtime::wrapper::judge`), really
//! does own the tamper finding a plugin cannot write
//! ([`stella_plugin::ObservedEvidence`], #3499), and `judge` really is
//! synchronous, I/O-free and total — so no model is ever asked to decide done.
//! A guard that banned "the host checks" outright would trade one false
//! statement for its mirror image, which is why this one bans exactly the
//! phrasings that attribute *running the oracle* to the host.

use std::fs;
use std::path::Path;

/// The phrasings that attribute running the oracle to the host: the issue's
/// own grep, widened to the two wrapped sites it could not see.
///
/// Lowercase, and matched against lowercased text — `manifest.rs` wrote the
/// retired claim as "the HOST runs this" for emphasis, and a guard that a
/// capital letter defeats is decoration.
const RETIRED: [&str; 5] = [
    "host-run",
    "host runs the oracle",
    "host runs the process",
    "program the host runs",
    "host runs this",
];

/// The two files where the retired claim legitimately survives, because in
/// both it is the *subject* rather than the assertion:
///
/// - `oracle.rs` narrates the #3511 decision on [`stella_plugin::Oracle`] —
///   "the manifest stops claiming the oracle is host-run" — which cannot be
///   said without saying it. The paragraph lived in `manifest.rs` until the
///   `[oracle]` block moved to its own module (#3730), and the exemption
///   followed it rather than being kept for a file that no longer carries it.
/// - `consent.rs` carries the same history paragraph plus the assertion string
///   in `an_oracle_is_disclosed_as_the_plugins_own_report`, the test that
///   proves the retired claim reaches no install prompt.
///
/// `manifest.rs`'s `[subloop]` line ("stages the host runs as bounded child
/// turns") is a different subject and is correct; it matches none of
/// [`RETIRED`] and needs no exemption of its own.
const ABOUT_THE_RETIRED_CLAIM: [&str; 2] = ["oracle.rs", "consent.rs"];

/// One file's text with every run of whitespace collapsed to a single space,
/// so a claim split across two lines reads as the sentence it is.
fn normalized(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The offending phrases in one file, each with the words around it so a
/// failure message points at the sentence rather than sending someone
/// grepping.
fn offences(name: &str, text: &str) -> Vec<String> {
    let text = normalized(text);
    let mut found = Vec::new();
    for phrase in RETIRED {
        let mut from = 0;
        while let Some(at) = text[from..].find(phrase) {
            let at = from + at;
            let start = text[..at].rfind(". ").map_or(0, |dot| dot + 2);
            let end = text[at..]
                .find(". ")
                .map_or(text.len(), |dot| (at + dot + 1).min(text.len()));
            found.push(format!("{name}: …{}…", &text[start..end]));
            from = at + phrase.len();
        }
    }
    found
}

/// Every file with `extension` under `dir`, as `(path relative to `dir`,
/// contents)`.
///
/// **Recursive**, which is the difference between a guard and a guard-shaped
/// hole: this crate's `src` is flat today apart from `src/bin`, so a
/// non-recursive walk would have looked green while covering everything, and
/// would have silently stopped covering the first submodule directory anyone
/// added. That is the shape of omission the whole file is against.
fn sources(dir: &Path, extension: &str) -> Vec<(String, String)> {
    fn walk(dir: &Path, extension: &str, prefix: &str, into: &mut Vec<(String, String)>) {
        for entry in fs::read_dir(dir).expect("the directory is readable") {
            let path = entry.expect("a readable directory entry").path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .expect("a UTF-8 file name")
                .to_string();
            let name = format!("{prefix}{name}");
            if path.is_dir() {
                walk(&path, extension, &format!("{name}/"), into);
            } else if path.extension().is_some_and(|ext| ext == extension) {
                let text = fs::read_to_string(&path).expect("a readable source file");
                into.push((name, text));
            }
        }
    }

    let mut files = Vec::new();
    walk(dir, extension, "", &mut files);
    // `read_dir` order is the filesystem's; a failure message that reorders
    // itself between runs is a failure message nobody trusts.
    files.sort();
    files
}

#[test]
fn no_doc_comment_attributes_running_the_oracle_to_the_host() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let files = sources(&src, "rs");
    let mut checked = 0usize;
    let mut found: Vec<String> = Vec::new();

    for (name, text) in &files {
        if ABOUT_THE_RETIRED_CLAIM.contains(&name.as_str()) {
            continue;
        }
        checked += 1;
        found.extend(offences(name, text));
    }

    assert!(
        checked >= 6,
        "the walk found only {checked} files, so a green result would prove nothing"
    );
    assert!(
        found.is_empty(),
        "the plugin runs its own oracle and reports the result; the host \
         evaluates the declared rule and merges its own tamper finding, and \
         runs nothing (#3511). These still say otherwise:\n{}",
        found.join("\n")
    );
}

/// The crate's own README, which is the page a contributor reads before any
/// doc comment and which described "the host-run `[oracle]` contract" in its
/// summary of what a loaded manifest has proved.
#[test]
fn the_crate_readme_does_not_describe_a_host_run_oracle() {
    let readme = Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
    let text = fs::read_to_string(&readme).expect("the crate has a README");
    let found = offences("README.md", &text);
    assert!(
        found.is_empty(),
        "the README is the first page a contributor reads:\n{}",
        found.join("\n")
    );
}

/// The same ban over the fixtures a manifest author reads as examples. A
/// fixture comment is the first documentation anyone writing a plugin sees,
/// and `arbiter.toml` carried the retired claim in its opening sentence.
#[test]
fn no_fixture_manifest_describes_a_host_run_oracle() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    let files = sources(&fixtures, "toml");
    let found: Vec<String> = files
        .iter()
        .flat_map(|(name, text)| offences(name, text))
        .collect();

    assert!(
        files.len() >= 2,
        "the walk found only {} fixtures, so a green result would prove nothing",
        files.len()
    );
    assert!(
        found.is_empty(),
        "a fixture is an example someone copies:\n{}",
        found.join("\n")
    );
}
