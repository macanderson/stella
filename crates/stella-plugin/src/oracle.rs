//! The `[oracle]` block — what a verification plugin declares it runs, and the
//! two shapes of evidence it may report back.
//!
//! Split out of [`crate::manifest`] (#3730) on the rule every other block in
//! this crate already follows: `[runtime]`, `[wrapper]`, `[driver]`, the
//! package blocks and the evidence half each own a module, and `manifest.rs`
//! is left declaring [`PluginManifest`](crate::PluginManifest) and the rules
//! that are about the manifest *as a whole* — a grade against a block, a block
//! against another block. What is here is what is true of an `[oracle]`
//! whatever else the manifest says.
//!
//! The evidence half is next door in [`crate::evidence`], which owns
//! `[[oracle.checks]]` and the `measurements` rule, and holds the one piece of
//! validation that reads `[requirements]` alongside the oracle
//! ([`Oracle::validate_evidence`]).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::ManifestError;
use crate::evidence::OracleCheck;
use crate::manifest::Participation;
use crate::runtime::Runtime;
use crate::wire::WrapperPoint;

/// The `[oracle]` block — the witness protocol as a wire contract, and the
/// evidence protocol beside it.
///
/// # The plugin runs this and reports the result. Stella does not.
///
/// This doc comment used to read "the HOST runs this; the plugin never grades
/// its own work (the #2584 discipline, stated for plugins)", and
/// [`crate::consent_text`] repeated it to a user about to install. **No host
/// code has ever executed it** — `grep -rn OracleCommand crates/ --include=*.rs`
/// outside this crate returns only tests, while the flip and the measurements
/// `stella_runtime::wrapper::judge` decides on arrive verbatim from the
/// plugin's own `after_turn` response ([`crate::ObservedEvidence`]). A plugin
/// reporting a favourable number it never earned was believed, on the strength
/// of a sentence that was not true (#3511).
///
/// The maintainer settled that as Option 2 on 2026-08-17: the manifest stops
/// claiming the oracle is host-run, and the consent text says plainly that the
/// plugin reports its own evidence. The block stays because it is still the
/// author's declaration of *what will run* — it is what a user is shown at
/// install, and it is the field a host that later executes the oracle itself
/// would read — but it declares an intent, never a host-enforced fact.
///
/// # What is still structural, and what is not
///
/// Unchanged: `judge` is synchronous, I/O-free and total, so "a verification
/// plugin quietly calls a model to decide done" remains impossible by
/// construction; the *rule* is the manifest's and only the host evaluates it;
/// a check conjoins with the flip and can only narrow done (#3510); and the
/// tamper finding is host-owned and not a field a plugin can write
/// ([`crate::ObservedEvidence`], #3499).
///
/// Not true, and no longer claimed here: that the evidence was **earned**.
/// Whoever consents to a verification plugin is trusting its honesty about its
/// own work, which is exactly what the install prompt now says.
///
/// Arbiter-only: the oracle exists to decide requirements, and below `arbiter`
/// there are none to decide.
///
/// Two shapes of evidence, and a manifest may declare either or both: a
/// fail→pass flip ([`FlipPolicy`]), and numbers the oracle reports which
/// declared checks compare against a budget ([`crate::OracleCheck`]). The
/// second exists because the first can only express one definition of done —
/// see `evidence.rs` for the falsifier that established that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Oracle {
    /// The argv the plugin declares it runs as its oracle. Never a shell
    /// string — the #1400 rule, same as every hook.
    ///
    /// **Declared, not dispatched.** Nothing in Stella executes this today
    /// (see the type's own doc comment and #3511); it is shown at install and
    /// is what a host that took the oracle over would read.
    ///
    /// **Optional when `[runtime]` is declared**, and absent then means "the
    /// oracle is this plugin's own process" (#3501). It was mandatory until
    /// Track C built one plugin three times and every one of them wrote its
    /// `[runtime].argv` out a second time here, byte for byte: a grammar that
    /// forces a redundant declaration teaches every author a redundant
    /// concept, and it made three manifests differ in four lines where two
    /// would do. [`PluginManifest::oracle_process`] is the resolved answer, so
    /// a host never has to know which of the two shapes was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<OracleCommand>,
    /// Whether the host must observe a fail→pass flip before crediting.
    pub flip: FlipPolicy,
    /// How the host detects tampering with the witness artifacts.
    ///
    /// Defaulted rather than mandatory since #3499. Tamper snapshotting is
    /// host-side (`doc:pipeline-as-plugins` §4 A10) and there is exactly one
    /// policy, so a manifest restating it said nothing a host did not already
    /// know — and a `flip = "not-applicable"` oracle, which has no flip to
    /// protect, still had to write the line. Declaring it explicitly remains
    /// legal and is how a future second policy will be selected.
    #[serde(default)]
    pub tamper: TamperPolicy,
    /// The names of the numbers this oracle reports. Non-blank and unique; a
    /// check may only read a name declared here, which is the evidence half
    /// of "a rule reading something nothing publishes is a load error".
    ///
    /// A declared measurement no check reads is allowed: reporting a number
    /// for the trace is legitimate, and only *deciding* on an undeclared one
    /// is the silence this crate refuses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub measurements: Vec<String>,
    /// The `[[oracle.checks]]` entries — the verdict rule, as data.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<OracleCheck>,
}

/// The process an oracle runs as, with the host-enforced bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OracleCommand {
    /// Program and arguments. `${plugin_dir}` interpolation is the host's
    /// concern; this crate only requires the list to be non-empty.
    pub argv: Vec<String>,
    /// Seconds the oracle is allowed before it is killed. Must be at least
    /// 1 — a zero timeout kills the oracle before it runs. Enforced by
    /// whoever runs the program, which today is the plugin (#3511).
    pub timeout_secs: u64,
}

/// The program declared as this manifest's oracle, with the declaration it
/// came from.
///
/// [`Oracle::command`] and [`Runtime`] are two ways to name one program, and a
/// reader must not have to know which one an author chose —
/// [`PluginManifest::oracle_process`] resolves it once. Its one shipped caller
/// is [`crate::consent_text`], which names the program at install; nothing runs
/// it (#3511). Borrowed rather than owned so resolving costs nothing;
/// `${plugin_dir}` interpolation stays the host's job, exactly as it is for
/// either declaration on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OracleProcess<'a> {
    /// Program and arguments.
    pub argv: &'a [String],
    /// Seconds it is allowed before it is killed.
    pub timeout_secs: u64,
    /// Which block named it — the one thing a caller may legitimately want to
    /// distinguish, because "runs a program of its own" and "runs itself
    /// again" are different sentences at an install prompt.
    pub source: OracleProcessSource,
}

/// Which declaration an [`OracleProcess`] was resolved from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleProcessSource {
    /// `[oracle] command` named its own program.
    OracleCommand,
    /// `[oracle]` named none, so the oracle is the plugin's own `[runtime]`
    /// process.
    Runtime,
}

/// Whether a fail→pass flip is required before the oracle's requirement is
/// credited. Closed, so an unknown value is a load error rather than a
/// silently weaker contract; a further relaxation adds a variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FlipPolicy {
    /// The host credits the oracle only on a fail-before / pass-after flip.
    ///
    /// The flip is the one the **plugin reports** having seen
    /// ([`ObservedEvidence::flip`](crate::ObservedEvidence)); the host does not
    /// watch it happen (#3511). What the host does with the report is still
    /// its own: this policy conjoins with every declared check, so a check can
    /// only narrow done and never stand in for the flip (#3510).
    Required,
    /// This oracle's evidence is not a flip: its measurements are what decide
    /// its requirements. A performance budget is the reference case — the
    /// benchmark passes before and after, and what changed is a number
    /// (`doc:pipeline-as-plugins` §6.1).
    ///
    /// **Not a weaker contract.** With no flip to decide anything, every
    /// requirement must be decided by a declared check or the manifest is
    /// refused ([`ManifestError::UndecidableRequirement`]), so this trades
    /// one host-evaluated rule for another rather than dropping one.
    NotApplicable,
}

/// How the host detects witness-artifact tampering. One variant today, for
/// the same reason as [`FlipPolicy`].
///
/// **This names what the *host* does, not what the plugin does.** Snapshotting
/// artifact identity is host-side by design (`doc:pipeline-as-plugins` §4 A10),
/// which is why the finding it produces — [`TamperFinding`](crate::TamperFinding)
/// — is not part of what a plugin may report: an
/// [`ObservedEvidence`](crate::ObservedEvidence) has no field for it, and the
/// host merges its own answer in before `judge` runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TamperPolicy {
    /// The host snapshots artifact identity at authoring time and refuses
    /// the flip if it changed by verify time — the `witness.rs` discipline.
    ///
    /// The default, because it is the only thing a host does: a manifest
    /// declaring nothing here is asking for the check every host performs, not
    /// opting out of one.
    #[default]
    #[serde(rename = "artifact-identity")]
    ArtifactIdentity,
}

impl Oracle {
    /// The `[oracle]` block's own load rules — everything true of an oracle
    /// regardless of what else the manifest declares.
    ///
    /// Takes the facts from outside the block that its rules read, rather than
    /// the whole manifest: the grade it must sit at, whether a `[runtime]`
    /// process exists for an oracle that named no command of its own, the
    /// points the plugin answers, and the requirements its checks must decide.
    /// A block validator handed the whole manifest is a second
    /// [`PluginManifest::validate`](crate::PluginManifest) waiting to grow.
    pub(crate) fn validate(
        &self,
        participation: Participation,
        runtime: Option<&Runtime>,
        points: &[WrapperPoint],
        requirements: Option<&BTreeMap<String, String>>,
    ) -> Result<(), ManifestError> {
        if participation != Participation::Arbiter {
            return Err(ManifestError::OracleRequiresArbiter { participation });
        }
        match &self.command {
            Some(command) => {
                if command.argv.is_empty() {
                    return Err(ManifestError::EmptyOracleArgv);
                }
                if command.timeout_secs == 0 {
                    return Err(ManifestError::ZeroOracleTimeout);
                }
            }
            // No command of its own means the oracle is the plugin's own
            // process, so there must be one. `[runtime]`'s own argv and
            // timeout bounds are checked where they are declared.
            None if runtime.is_none() => return Err(ManifestError::OracleCommandRequired),
            None => {}
        }
        if !points.contains(&WrapperPoint::AfterTurn) {
            return Err(ManifestError::OracleRequiresAfterTurn);
        }
        self.validate_evidence(requirements)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PluginManifest;

    fn parse(text: &str) -> Result<PluginManifest, ManifestError> {
        PluginManifest::from_toml_str(text)
    }

    #[test]
    fn an_oracle_below_arbiter_is_rejected() {
        let err = parse(
            "name = \"x\"\n[loop]\nparticipation = \"steering\"\n\n[oracle]\ncommand = { argv = [\"oracle\"], timeout_secs = 10 }\nflip = \"required\"\ntamper = \"artifact-identity\"",
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::OracleRequiresArbiter { .. }));
    }

    #[test]
    fn oracle_argv_and_timeout_bounds_are_enforced() {
        let head = "name = \"x\"\n[loop]\nparticipation = \"arbiter\"\nhooks = [\"Stop\"]\n\n[requirements]\nr = \"a requirement\"\n\n[oracle]\n";
        let tail = "flip = \"required\"\ntamper = \"artifact-identity\"";

        let empty_argv = parse(&format!(
            "{head}command = {{ argv = [], timeout_secs = 10 }}\n{tail}"
        ))
        .unwrap_err();
        assert!(matches!(empty_argv, ManifestError::EmptyOracleArgv));

        let zero_timeout = parse(&format!(
            "{head}command = {{ argv = [\"o\"], timeout_secs = 0 }}\n{tail}"
        ))
        .unwrap_err();
        assert!(matches!(zero_timeout, ManifestError::ZeroOracleTimeout));
    }

    #[test]
    fn an_unknown_flip_or_tamper_value_is_a_load_error() {
        let err = parse(
            "name = \"x\"\n[loop]\nparticipation = \"arbiter\"\nhooks = [\"Stop\"]\n\n[requirements]\nr = \"a requirement\"\n\n[oracle]\ncommand = { argv = [\"o\"], timeout_secs = 10 }\nflip = \"optional\"\ntamper = \"artifact-identity\"",
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::Parse(_)));
    }

    /// The flip vocabulary's wire strings, pinned on both sides. `kebab-case`
    /// replaced `lowercase` when `not-applicable` joined the enum, which is
    /// invisible for `Required` and would silently rename any future variant
    /// spelled in two words — every shipped manifest naming it would stop
    /// loading. Pinning both spellings makes that a red test instead.
    #[test]
    fn flip_policy_wire_strings_are_pinned() {
        for (policy, wire) in [
            (FlipPolicy::Required, "required"),
            (FlipPolicy::NotApplicable, "not-applicable"),
        ] {
            assert_eq!(serde_json::to_value(policy).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<FlipPolicy>(wire.into()).unwrap(),
                policy
            );
        }
    }

    /// **Witness for #3501 item 1.** The oracle may be the plugin's own
    /// process, so a manifest declaring `[runtime]` no longer writes the same
    /// argv twice — and the resolver answers the same program either way.
    #[test]
    fn an_oracle_without_a_command_is_the_plugins_own_process() {
        let head = "name = \"x\"\n[loop]\nparticipation = \"arbiter\"\nhooks = [\"Stop\"]\npoints = [\"after_turn\"]\n\n[requirements]\nr = \"a requirement\"\n\n[oracle]\nflip = \"required\"\n";
        let runtime =
            "\n[runtime]\nargv = [\"python3\", \"${plugin_dir}/main.py\"]\ntimeout_secs = 30\n";

        let same_process = parse(&format!("{head}{runtime}")).expect(
            "an [oracle] with no command must load when [runtime] declares the process it is",
        );
        let resolved = same_process
            .oracle_process()
            .expect("the oracle resolves to the runtime's process");
        assert_eq!(resolved.argv, ["python3", "${plugin_dir}/main.py"]);
        assert_eq!(resolved.timeout_secs, 30);
        assert_eq!(resolved.source, OracleProcessSource::Runtime);

        // A command of its own still wins, and still says so.
        let own = parse(&format!(
            "{head}command = {{ argv = [\"oracle\"], timeout_secs = 10 }}\n{runtime}"
        ))
        .expect("a declared command must still load");
        let resolved = own.oracle_process().expect("the declared command resolves");
        assert_eq!(resolved.argv, ["oracle"]);
        assert_eq!(resolved.timeout_secs, 10);
        assert_eq!(resolved.source, OracleProcessSource::OracleCommand);

        // Neither: there is no program to run, and a manifest that names none
        // is refused rather than loaded into a host that would find out later.
        let neither = parse(head).unwrap_err();
        assert!(matches!(neither, ManifestError::OracleCommandRequired));
    }

    /// An `[oracle]` whose evidence can never arrive is the undecidable
    /// contract #3499 named, one level up: the evidence rides on the
    /// `after_turn` response, and an undeclared point is never dispatched.
    #[test]
    fn an_oracle_must_declare_the_point_its_evidence_arrives_at() {
        let err = parse(
            "name = \"x\"\n[loop]\nparticipation = \"arbiter\"\nhooks = [\"Stop\"]\n\n[requirements]\nr = \"a requirement\"\n\n[oracle]\ncommand = { argv = [\"o\"], timeout_secs = 10 }\nflip = \"required\"",
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::OracleRequiresAfterTurn));
    }

    /// **The `[oracle] tamper` half of #3499.** The policy names what the
    /// *host* does, so a manifest no longer has to restate the only thing a
    /// host does — while a manifest that states it explicitly still loads and
    /// still means the same thing.
    #[test]
    fn the_tamper_policy_is_the_hosts_and_need_not_be_restated() {
        let head = "name = \"x\"\n[loop]\nparticipation = \"arbiter\"\nhooks = [\"Stop\"]\npoints = [\"after_turn\"]\n\n[requirements]\nr = \"a requirement\"\n\n[oracle]\ncommand = { argv = [\"o\"], timeout_secs = 10 }\nflip = \"required\"\n";

        let silent = parse(head).expect("an [oracle] with no tamper line must load");
        let oracle = silent.oracle.expect("the block must be carried");
        assert_eq!(oracle.tamper, TamperPolicy::ArtifactIdentity);

        let explicit = parse(&format!("{head}tamper = \"artifact-identity\""))
            .expect("declaring it explicitly must keep working");
        assert_eq!(
            explicit.oracle.expect("the block must be carried").tamper,
            TamperPolicy::ArtifactIdentity
        );
    }
}
