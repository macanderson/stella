//! What the four plugin conformance harnesses share: the golden comparison,
//! and the `BLESS=1` path that regenerates one.
//!
//! Not compiled as a test binary of its own — `tests/` subdirectories are
//! modules, and each harness pulls this in with `mod common;`.

use std::path::Path;

use stella_plugin::WrapperResponse;

/// Compare `actual` against the committed golden at `golden_path`, or rewrite
/// that golden from `actual` when `BLESS` is set (#3548).
///
/// # Why this exists
///
/// `plugins/stella-{research,plan,goal,witness}/testdata/*.expected.json` are goldens
/// produced by running the shipped plugin against the fixture workspace these
/// harnesses build in Rust. They were generated once by a scratch script that
/// reproduced that tree in Python and is not in this repository, so the next
/// person to change a contribution's wording had two options and both were
/// bad: hand-edit the JSON, or rewrite the scratch script and hope their
/// fixture matched the harness's. Either way the fixture is defined twice.
///
/// The bless path is the same `fixture()` the assertions run against, which is
/// the whole point — one definition, and a regeneration diff that is content
/// and nothing else.
///
/// # The rule that makes a blessed golden safe
///
/// AGENTS.md states it for the command deck's frames and it holds identically
/// here: **read the diff**. A golden blessed without looking is a changelog,
/// not a test.
///
/// # Formatting
///
/// `serde_json::to_string_pretty` plus a trailing newline — byte-for-byte what
/// the committed vectors carry, so a regeneration that changed nothing writes
/// nothing.
///
/// # `normalize`, and why it runs on one side rather than two
///
/// `plugins/stella-witness` reports `test-duration-ms`, which is wall clock: a
/// different number on every run and on every machine. Its harness used to
/// answer that by *dropping* the key from both sides and then comparing a
/// hand-picked pair of fields, which cost it this bless path — a golden written
/// from the unnormalised response could never match the next run — and cost it
/// the rest of the response as well (#4437).
///
/// `normalize` runs on `actual` and nowhere else, which is what makes the two
/// paths agree: the same pinned response is what gets written at bless time and
/// what gets compared, so a regenerated golden round-trips. The golden itself is
/// read verbatim and compared whole, so a measurement the plugin stops emitting,
/// or a `protocol_version` that moves, is a failure where dropping made it
/// invisible. Pin rather than drop, for that reason: the key stays in the
/// golden and only its value is exempt.
///
/// A harness with nothing to pin passes `|_| {}`.
///
/// # Panics
///
/// When a vector carries a `.refusal.txt` sibling: a refusal vector is graded
/// by the plugin's stderr and exit status, never by a golden response, and
/// blessing one would create the second grading file
/// `every_vector_is_graded_by_exactly_one_sibling` exists to forbid. Also when
/// the golden is unreadable, is not a response, or does not match.
pub fn bless_or_assert(
    name: &str,
    golden_path: &Path,
    actual: &WrapperResponse,
    normalize: impl Fn(&mut WrapperResponse),
) {
    let mut actual = actual.clone();
    normalize(&mut actual);
    let actual = &actual;

    let refusal = golden_path.with_file_name(
        golden_path
            .file_name()
            .expect("a golden has a file name")
            .to_string_lossy()
            .replace(".expected.json", ".refusal.txt"),
    );
    assert!(
        !refusal.exists(),
        "{name} carries a .refusal.txt sibling: it is graded by the plugin's refusal, \
         so it must not also carry a golden response"
    );

    if std::env::var_os("BLESS").is_some() {
        let mut json = serde_json::to_string_pretty(actual).expect("a response serializes");
        json.push('\n');
        std::fs::write(golden_path, json).expect("the golden is writable");
        return;
    }

    let golden: WrapperResponse =
        serde_json::from_str(&std::fs::read_to_string(golden_path).expect("a readable golden"))
            .unwrap_or_else(|e| panic!("{name}'s golden is not a response: {e}"));
    assert_eq!(
        actual, &golden,
        "{name} did not answer with its golden contribution. If the change is intended, \
         regenerate with BLESS=1 and read the diff."
    );
}
