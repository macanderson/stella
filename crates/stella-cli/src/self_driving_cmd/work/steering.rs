//! Whether this workspace has steering the loop would lose by running
//! untrusted.
//!
//! Split out of `work.rs` so the counting rule has room to be precise. The
//! question is narrow: does `<root>/.stella/rules` hold a record that would
//! reach a turn if the project were trusted? A `.toml` file sitting there is
//! not the answer, because two kinds of file live beside the records and
//! neither ever steers anything.

use std::path::Path;

use crate::rules::RESERVED_RULE_FILENAMES;

/// Refuse to work an issue with the workspace's steering switched off.
///
/// **A loop-driven turn must get exactly the steering a person-driven turn
/// gets.** The whole design says the loop's behaviour comes from context
/// records — how this repository wants code written, what to prefer, what to
/// harden — and none of that reaches a turn when project steering is untrusted.
///
/// The trap is that it fails *silently and successfully*: the turn runs, writes
/// plausible code, commits, and the pull request looks like every other one. A
/// loop working unsteered is not a degraded loop, it is a loop doing work under
/// nobody's standards, and it is worse than one that did not run — so this
/// refuses rather than warns.
///
/// It refuses only when there is something to lose: a workspace with nothing
/// that would steer has no steering to miss, and demanding a trust flag from it
/// would be ceremony.
pub(in crate::self_driving_cmd) fn refuse_if_unsteered(root: &Path) -> Result<(), String> {
    refuse_unless_trusted(root, crate::settings::project_code_execution_trusted())
}

/// The rule of [`refuse_if_unsteered`], with the process it reads taken out.
///
/// Separated because `project_code_execution_trusted` answers from the process
/// environment, which this test suite shares with every other test running
/// beside it. A pure function takes the answer as an argument, so both
/// directions of the rule can be pinned without a race.
fn refuse_unless_trusted(root: &Path, trusted: bool) -> Result<(), String> {
    let records = root.join(".stella").join("rules");
    if trusted || !holds_a_steering_record(&records) {
        return Ok(());
    }

    Err(format!(
        "refusing to work an issue with this workspace's steering switched off.\n\
         \n\
         {} declares context records and none of them would reach the turn, so it \
         would write code under nobody's standards — and it would look exactly like \
         a turn that did.\n\
         \n\
         Set STELLA_TRUST_PROJECT=1 to let this repository steer the loop it is \
         driving.",
        records.display()
    ))
}

/// Whether `dir` holds at least one record that trust would let steer a turn.
///
/// Counting `*.toml` files answered a different question, and stopped the loop
/// over two kinds of file that steer nothing either way (#5712):
///
/// - `governance.toml` and anything else in [`RESERVED_RULE_FILENAMES`], which
///   the record loader never parses as a record at all.
/// - A record whose `status` is `archived` or `retracted`. The registry's
///   `blocking_reason` refuses to let those steer, so a workspace that has
///   retired its records has nothing left to lose.
///
/// Reads and parses the files rather than taking the session's registry:
/// `load_registry` runs truth probes and writes a sweep cache, which would put
/// filesystem probes in front of every work unit. Nothing here creates a
/// directory.
fn holds_a_steering_record(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        let path = entry.path();
        let is_toml = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"));
        let reserved = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| RESERVED_RULE_FILENAMES.contains(&name));
        if !is_toml || reserved {
            return false;
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => file_holds_a_steering_record(&text),
            // Unreadable is not "absent": the file is there and something is
            // wrong with reading it, so take the refusing answer.
            Err(_) => true,
        }
    })
}

/// The `status` of each `[[record]]` in one file, and nothing else. Reading the
/// full record type here would make a cheap pre-flight check a second parser to
/// keep in step with `stella_records`.
#[derive(serde::Deserialize)]
struct RecordFileStatuses {
    #[serde(default)]
    record: Vec<RecordStatusOnly>,
}

#[derive(serde::Deserialize)]
struct RecordStatusOnly {
    status: Option<String>,
}

/// Whether one record file's contents hold a record that could steer.
///
/// A file that will not parse counts as steering. It is a record the loader
/// will report on, and refusing is the safe direction for an unknown.
fn file_holds_a_steering_record(text: &str) -> bool {
    let Ok(parsed) = toml::from_str::<RecordFileStatuses>(text) else {
        return true;
    };
    parsed
        .record
        .iter()
        // An unset status is active: `RecordStatus` is optional on the wire and
        // the registry reads `None` as active.
        .any(|record| matches!(record.status.as_deref(), None | Some("active")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One record file holding one record, the shape `.stella/rules/*.toml`
    /// actually carries.
    fn record_with_status(status: &str) -> String {
        format!(
            "schema = \"context-record/v0.1\"\n\
             set_id = \"example.one\"\n\
             \n\
             [[record]]\n\
             lineage_id = \"ctx.example.one.thing\"\n\
             statement = \"Do the thing.\"\n\
             status = \"{status}\"\n"
        )
    }

    fn active_record() -> String {
        record_with_status("active")
    }

    fn rules_dir(root: &Path) -> std::path::PathBuf {
        let rules = root.join(".stella").join("rules");
        std::fs::create_dir_all(&rules).expect("rules directory");
        rules
    }

    /// **Witness.** The loop refuses to work an issue when the workspace's
    /// records would not reach the turn, and runs when they would.
    ///
    /// The failure this guards is silent. An untrusted checkout loads none of
    /// its records, so the turn writes plausible code under nobody's standards
    /// and the pull request looks like every other one. Both directions are
    /// asserted, because a check that only ever refused would be satisfied by
    /// a function that always refuses.
    ///
    /// A workspace with no records is the third cell: there is no steering to
    /// miss, so asking it for a trust flag would be ceremony.
    #[test]
    fn the_loop_will_not_work_an_issue_that_its_records_cannot_steer() {
        let bare = tempfile::tempdir().expect("workspace");
        assert!(
            refuse_unless_trusted(bare.path(), false).is_ok(),
            "a workspace with no records has no steering to miss"
        );

        let steered = tempfile::tempdir().expect("workspace");
        let rules = rules_dir(steered.path());
        std::fs::write(rules.join("ctx.example.one.toml"), active_record()).expect("a record file");

        let refusal = refuse_unless_trusted(steered.path(), false)
            .expect_err("records that cannot steer must stop the work");
        assert!(
            refusal.contains("STELLA_TRUST_PROJECT"),
            "the refusal must name the remedy: {refusal}"
        );
        assert!(
            refuse_unless_trusted(steered.path(), true).is_ok(),
            "a trusted workspace steers the turn, so the work proceeds"
        );
    }

    /// **Witness.** Two kinds of `.toml` in the rules directory steer nothing,
    /// and neither may stop the loop (#5712).
    ///
    /// Both stopped it before the count read the files. `governance.toml` is a
    /// reserved name the record loader never parses, and an archived or
    /// retracted record is one the registry already refuses to let steer. The
    /// operator was told to set a trust flag so records could steer the loop,
    /// in a workspace where no record would steer it either way.
    #[test]
    fn a_file_that_would_never_steer_does_not_stop_the_loop() {
        let governance_only = tempfile::tempdir().expect("workspace");
        let rules = rules_dir(governance_only.path());
        std::fs::write(rules.join("governance.toml"), "mode = \"regulated\"\n")
            .expect("a governance file");
        assert!(
            refuse_unless_trusted(governance_only.path(), false).is_ok(),
            "governance is not a record, so there is no steering to lose"
        );

        for status in ["archived", "retracted"] {
            let retired = tempfile::tempdir().expect("workspace");
            let rules = rules_dir(retired.path());
            std::fs::write(
                rules.join("ctx.example.one.toml"),
                record_with_status(status),
            )
            .expect("a record file");
            assert!(
                refuse_unless_trusted(retired.path(), false).is_ok(),
                "a {status} record does not steer, so it cannot be lost"
            );
        }

        // The same directory with one live record beside the retired one still
        // stops an untrusted loop, so the two cases above are about what was
        // written and not about the fixture.
        let mixed = tempfile::tempdir().expect("workspace");
        let rules = rules_dir(mixed.path());
        std::fs::write(rules.join("governance.toml"), "mode = \"regulated\"\n")
            .expect("a governance file");
        std::fs::write(
            rules.join("ctx.example.gone.toml"),
            record_with_status("archived"),
        )
        .expect("a retired record");
        std::fs::write(rules.join("ctx.example.live.toml"), active_record())
            .expect("a live record");
        assert!(
            refuse_unless_trusted(mixed.path(), false).is_err(),
            "one live record is still steering the loop would lose"
        );
    }

    /// A record with no `status` key is active. A file the parser cannot read
    /// takes the refusing answer, and a file declaring no record at all
    /// declares no steering.
    #[test]
    fn an_unset_status_steers_and_an_unparseable_file_refuses() {
        let unset = tempfile::tempdir().expect("workspace");
        let rules = rules_dir(unset.path());
        std::fs::write(
            rules.join("ctx.example.one.toml"),
            "schema = \"context-record/v0.1\"\n\n[[record]]\nlineage_id = \"ctx.a.b.c\"\n",
        )
        .expect("a record file");
        assert!(refuse_unless_trusted(unset.path(), false).is_err());

        let broken = tempfile::tempdir().expect("workspace");
        let rules = rules_dir(broken.path());
        std::fs::write(rules.join("ctx.example.one.toml"), "[[record\nnot toml")
            .expect("a broken file");
        assert!(
            refuse_unless_trusted(broken.path(), false).is_err(),
            "an unknown takes the safe direction"
        );

        let empty = tempfile::tempdir().expect("workspace");
        let rules = rules_dir(empty.path());
        std::fs::write(rules.join("ctx.example.one.toml"), "").expect("an empty file");
        assert!(
            refuse_unless_trusted(empty.path(), false).is_ok(),
            "a file declaring no record declares no steering"
        );
    }
}
