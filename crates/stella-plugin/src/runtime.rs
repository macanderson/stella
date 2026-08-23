//! The `[runtime]` block — the process a plugin *is*, and the exact slice of
//! the operator's environment it may see.
//!
//! # Why there is no `language` field
//!
//! `argv` already says everything a host needs:
//! `["python3", "${plugin_dir}/main.py"]` and
//! `["node", "${plugin_dir}/dist/main.js"]` are two plugins, not two
//! Stella features. A `language` key would make Stella learn what a language
//! is — a per-language launcher table, a per-language packaging story, a
//! per-language bug — to express something the argv already expresses
//! (`doc:pipeline-as-plugins` §A5). It is also the property that makes the
//! three-language proof a *proof*: three plugins that differ in
//! `[runtime].argv` and in nothing else demonstrate that the socket is
//! language-neutral. If one of them needed a second field, it would not.
//!
//! # The environment is an allowlist, and that is a security boundary
//!
//! [`Runtime::env`] is default-deny: the child sees the variables the manifest
//! names and **nothing else**. A plugin is third-party code a user installed;
//! handing it the operator's whole environment hands it `ANTHROPIC_API_KEY`,
//! `AWS_SESSION_TOKEN`, `SSH_AUTH_SOCK` and every credential the shell was
//! carrying, silently, forever. Invariant 3 says every model call is made by
//! the host — a plugin that can read the key makes that a policy rather than a
//! property.
//!
//! Default-deny is also why this is a manifest field at all rather than a
//! host default: what the child needs is knowable only by its author, and what
//! it is *given* must be visible to the human who installs it. The names land
//! in the consent text beside the argv for exactly that reason.
//!
//! [`Runtime::child_env`] is the pure half — the host supplies a lookup and
//! gets back the exact pairs to set after clearing the child's environment.
//! Refusing a *credential* the manifest asked for is a further narrowing this
//! crate cannot make: it has no credential vocabulary and must not grow one.
//! That narrowing is not optional and it is not any one host's discretion:
//! `stella_runtime::wrapper::SubprocessWrapper::declare` applies it at the
//! socket, the boundary every driver crosses, and reports the names it
//! withheld (#3512). So a manifest may still *name* `ANTHROPIC_API_KEY` and
//! load cleanly; what it cannot do is receive it.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::ManifestError;

/// The placeholder a manifest writes where the package's own installed
/// directory belongs.
///
/// Declared here because this crate is where the convention is *specified* —
/// see [`Runtime::argv`] — even though only a host can carry it out: this crate
/// resolves no paths. It was previously a string literal in three places
/// (hook argv, wrapper argv, and a contributed tool's `command`/`[env]`), which
/// is the shape where one copy gains an escape rule and the others silently do
/// not (#4301).
pub const PLUGIN_DIR_PLACEHOLDER: &str = "${plugin_dir}";

/// Replace every [`PLUGIN_DIR_PLACEHOLDER`] in `text` with `dir`.
///
/// Only ever called for something read out of an installed package directory.
/// A user's own `.stella/tools/` manifest is left verbatim: no package is in
/// scope there, and expanding the placeholder to nothing would silently turn
/// `${plugin_dir}/x.sh` into a different path rather than leaving a visible
/// spawn failure naming what the manifest actually asked for.
///
/// A path that is not valid UTF-8 substitutes lossily, which is what every
/// caller did before this was shared: the alternative is refusing to start a
/// plugin because the *installer's* home directory has an odd byte in it.
#[must_use]
pub fn expand_plugin_dir(text: &str, dir: &Path) -> String {
    text.replace(PLUGIN_DIR_PLACEHOLDER, &dir.to_string_lossy())
}

/// The `[runtime]` block — how the host starts this plugin's process.
///
/// Modelled directly on [`crate::OracleCommand`], deliberately: the oracle
/// block already settled that a plugin names a program as an argv list and
/// never as a shell string (the #1400 rule), and that `${plugin_dir}`
/// interpolation is the host's job. This block is the same decision applied to
/// the plugin's own process, plus the environment allowlist
/// [`crate::OracleCommand`] does not carry — and does not need, because the
/// oracle is the plugin's own process to run and report on (#3511, and #3501
/// lets it be literally the same argv). The environment an oracle sees is the
/// one declared here, which is the one a human consented to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    /// Program and arguments. Never a shell string.
    ///
    /// `${plugin_dir}` interpolation is the host's concern — this crate is
    /// pure and has no idea where the plugin was installed — so a manifest
    /// writes `["python3", "${plugin_dir}/main.py"]` and the loader
    /// substitutes the resolved directory before spawning. This crate only
    /// requires a non-empty list whose first element names something.
    pub argv: Vec<String>,
    /// Seconds the host allows one invocation before killing the process.
    /// At least 1, for [`crate::OracleCommand::timeout_secs`]'s reason: a
    /// zero timeout kills the child before it runs.
    pub timeout_secs: u64,
    /// The environment variable names — **exactly** these — the child
    /// inherits. Absent or empty means the child inherits nothing.
    ///
    /// Not a denylist and not a prefix grammar: an exact list of names, so
    /// the set a user consents to at install is the set the child gets. A
    /// glob here would let one accepted line widen later without the consent
    /// text changing, which is the failure mode the whole block exists to
    /// prevent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
}

impl Runtime {
    /// The environment pairs to set on the child, in declaration order.
    ///
    /// **Default-deny.** The result contains only names this manifest
    /// declared, and only those `lookup` answered for — an allowlisted name
    /// that is unset in the parent is absent from the child rather than
    /// present and empty, because "unset" and "empty" are different answers
    /// to a program that checks.
    ///
    /// The caller must **clear** the child's environment before applying
    /// this; nothing here can un-inherit a variable the caller left in place.
    /// `stella_runtime::wrapper::SubprocessWrapper` is the reference caller:
    /// it clears, applies these pairs, and withholds the credentials among
    /// them.
    #[must_use]
    pub fn child_env<F>(&self, mut lookup: F) -> Vec<(String, String)>
    where
        F: FnMut(&str) -> Option<String>,
    {
        self.env
            .iter()
            .filter_map(|name| lookup(name).map(|value| (name.clone(), value)))
            .collect()
    }

    /// The cross-field rules, kept beside the type they govern exactly as
    /// [`crate::consent`]'s and [`crate::wrapper`]'s are.
    pub(crate) fn validate(&self) -> Result<(), ManifestError> {
        if self.argv.is_empty() {
            return Err(ManifestError::EmptyRuntimeArgv);
        }
        if self.argv.iter().any(|arg| arg.trim().is_empty()) {
            return Err(ManifestError::BlankRuntimeArg);
        }
        if self.timeout_secs == 0 {
            return Err(ManifestError::ZeroRuntimeTimeout);
        }
        let mut seen = std::collections::HashSet::with_capacity(self.env.len());
        for name in &self.env {
            if name.is_empty() {
                return Err(ManifestError::EmptyRuntimeEnvName);
            }
            // A name that is not a name can never match a variable, so the
            // allowlist entry is dead — and a dead entry in a security
            // allowlist reads to its author as a granted one. `=` and NUL are
            // the two bytes an environment name cannot contain on any
            // platform; surrounding whitespace is the same defect wearing a
            // subtler spelling (`" PATH"` looks granted and matches nothing).
            if name.contains('=') || name.contains('\0') || name.trim() != name {
                return Err(ManifestError::InvalidRuntimeEnvName { name: name.clone() });
            }
            if !seen.insert(name) {
                return Err(ManifestError::DuplicateRuntimeEnv { name: name.clone() });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Path, expand_plugin_dir};
    use crate::error::ManifestError;
    use crate::manifest::PluginManifest;

    fn parse(text: &str) -> Result<PluginManifest, ManifestError> {
        PluginManifest::from_toml_str(text)
    }

    const OBSERVER: &str = "name = \"p\"\n[loop]\nparticipation = \"observer\"\n\n[runtime]\n";

    /// **Witness (a) for A5.** A manifest carrying `[runtime]` loads, and an
    /// unknown key inside the block is still a load error — the block joins
    /// the `deny_unknown_fields` discipline rather than opening a hole in it.
    #[test]
    fn a_runtime_block_loads_and_an_unknown_key_inside_it_still_fails() {
        let manifest = parse(&format!(
            "{OBSERVER}argv = [\"python3\", \"${{plugin_dir}}/main.py\"]\ntimeout_secs = 30\nenv = [\"PATH\"]"
        ))
        .expect("a well-formed [runtime] block must load");
        let runtime = manifest.runtime.expect("the block must be carried");
        assert_eq!(runtime.argv, ["python3", "${plugin_dir}/main.py"]);
        assert_eq!(runtime.timeout_secs, 30);
        assert_eq!(runtime.env, ["PATH"]);

        let unknown = parse(&format!(
            "{OBSERVER}argv = [\"python3\"]\ntimeout_secs = 30\nlanguage = \"python\""
        ))
        .unwrap_err();
        assert!(
            matches!(unknown, ManifestError::Parse(_)),
            "a `language` key is exactly the field this block refuses to grow; \
             got {unknown:?}"
        );
    }

    /// **Witness (b) for A5, the pure half.** A variable the manifest did not
    /// name is not in the child's environment — the allowlist is default-deny,
    /// not a filter over an inherited set. The spawning half of this witness
    /// lives in `stella-cli`, where there is a real child to inspect.
    #[test]
    fn an_undeclared_variable_does_not_reach_the_child() {
        let manifest = parse(&format!(
            "{OBSERVER}argv = [\"python3\"]\ntimeout_secs = 5\nenv = [\"PLUGIN_MODE\", \"PATH\"]"
        ))
        .expect("fixture must parse");
        let runtime = manifest.runtime.expect("the block must be carried");

        let parent = |name: &str| match name {
            "PLUGIN_MODE" => Some("wrapper".to_string()),
            "PATH" => Some("/usr/bin".to_string()),
            "ANTHROPIC_API_KEY" => Some("sk-do-not-leak".to_string()),
            "SSH_AUTH_SOCK" => Some("/tmp/agent.sock".to_string()),
            _ => None,
        };
        let child = runtime.child_env(parent);
        assert_eq!(
            child,
            vec![
                ("PLUGIN_MODE".to_string(), "wrapper".to_string()),
                ("PATH".to_string(), "/usr/bin".to_string()),
            ],
            "declaration order is preserved and nothing else is admitted"
        );
        assert!(
            !child.iter().any(|(name, _)| name == "ANTHROPIC_API_KEY"),
            "the credential paying for the agent is set in the parent and must \
             not be reachable by a plugin that did not ask for it — and would \
             still be refused by the host if it had"
        );
    }

    /// An allowlisted name the parent does not carry is absent, never present
    /// and empty: `os.environ.get("X") is None` and `== ""` are different
    /// answers, and a plugin branching on the first must not see the second.
    #[test]
    fn an_unset_allowlisted_name_is_absent_rather_than_empty() {
        let manifest = parse(&format!(
            "{OBSERVER}argv = [\"node\"]\ntimeout_secs = 5\nenv = [\"UNSET_ONE\"]"
        ))
        .expect("fixture must parse");
        let runtime = manifest.runtime.expect("the block must be carried");
        assert!(runtime.child_env(|_| None).is_empty());
    }

    #[test]
    fn an_empty_allowlist_admits_nothing() {
        let manifest = parse(&format!("{OBSERVER}argv = [\"node\"]\ntimeout_secs = 5"))
            .expect("fixture must parse");
        let runtime = manifest.runtime.expect("the block must be carried");
        assert!(runtime.env.is_empty());
        assert!(
            runtime
                .child_env(|_| Some("anything".to_string()))
                .is_empty()
        );
    }

    #[test]
    fn argv_and_timeout_bounds_match_the_oracle_command_they_are_modelled_on() {
        let empty = parse(&format!("{OBSERVER}argv = []\ntimeout_secs = 5")).unwrap_err();
        assert!(matches!(empty, ManifestError::EmptyRuntimeArgv));

        let blank = parse(&format!(
            "{OBSERVER}argv = [\"python3\", \" \"]\ntimeout_secs = 5"
        ))
        .unwrap_err();
        assert!(matches!(blank, ManifestError::BlankRuntimeArg));

        let zero = parse(&format!("{OBSERVER}argv = [\"python3\"]\ntimeout_secs = 0")).unwrap_err();
        assert!(matches!(zero, ManifestError::ZeroRuntimeTimeout));
    }

    /// A dead allowlist entry reads to its author as a granted one, so each
    /// shape that could never match a variable is refused by name.
    #[test]
    fn an_allowlist_entry_that_could_never_match_is_refused() {
        let cases = [
            ("\"\"", "empty"),
            ("\" PATH\"", "leading space"),
            ("\"PATH \"", "trailing space"),
            ("\"PA=TH\"", "an equals sign"),
        ];
        for (entry, why) in cases {
            let err = parse(&format!(
                "{OBSERVER}argv = [\"node\"]\ntimeout_secs = 5\nenv = [{entry}]"
            ))
            .unwrap_err();
            assert!(
                matches!(
                    err,
                    ManifestError::EmptyRuntimeEnvName
                        | ManifestError::InvalidRuntimeEnvName { .. }
                ),
                "{why} must be refused, got {err:?}"
            );
        }

        let dupe = parse(&format!(
            "{OBSERVER}argv = [\"node\"]\ntimeout_secs = 5\nenv = [\"PATH\", \"PATH\"]"
        ))
        .unwrap_err();
        assert!(matches!(dupe, ManifestError::DuplicateRuntimeEnv { .. }));
    }

    /// A `none`-grade content bundle is never invoked at any point, so a
    /// process declaration there is a promise nothing can keep. Refused
    /// rather than ignored, for the reason every other grade rule in this
    /// crate is: an unreachable declaration reads as a granted one.
    #[test]
    fn a_runtime_below_observer_is_rejected() {
        let err =
            parse("name = \"p\"\n\n[runtime]\nargv = [\"node\"]\ntimeout_secs = 5").unwrap_err();
        assert!(matches!(err, ManifestError::RuntimeRequiresObserver { .. }));
    }

    /// The three-language proof, as a test rather than a promise: three
    /// plugins that differ in `[runtime].argv` and in nothing else.
    #[test]
    fn three_languages_differ_in_argv_and_nothing_else() {
        let body = "\ntimeout_secs = 30\nenv = [\"PATH\"]";
        let argvs = [
            "[\"python3\", \"${plugin_dir}/main.py\"]",
            "[\"node\", \"${plugin_dir}/dist/main.js\"]",
            "[\"${plugin_dir}/target/release/wrapper\"]",
        ];
        let manifests: Vec<PluginManifest> = argvs
            .iter()
            .map(|argv| parse(&format!("{OBSERVER}argv = {argv}{body}")).expect("must parse"))
            .collect();
        for pair in manifests.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            let (ra, rb) = (
                a.runtime.as_ref().expect("runtime"),
                b.runtime.as_ref().expect("runtime"),
            );
            assert_ne!(ra.argv, rb.argv, "the argv is what differs");
            assert_eq!(ra.timeout_secs, rb.timeout_secs);
            assert_eq!(ra.env, rb.env);
            assert_eq!(a.loop_grant, b.loop_grant, "and nothing else does");
        }
    }

    /// The one substitution, pinned once. Three hosts call it — hook argv,
    /// wrapper argv, and a contributed tool's `command`/`[env]` — and each used
    /// to spell the literal itself (#4301).
    #[test]
    fn plugin_dir_expands_everywhere_it_appears_and_nowhere_else() {
        let dir = Path::new("/home/a/.stella/plugins/vera");
        assert_eq!(
            expand_plugin_dir("${plugin_dir}/main.py", dir),
            "/home/a/.stella/plugins/vera/main.py"
        );
        // Several in one string, and one embedded mid-value — an `[env]` entry
        // like `PATH = "${plugin_dir}/bin:${plugin_dir}/vendor/bin"`.
        assert_eq!(
            expand_plugin_dir("${plugin_dir}/bin:${plugin_dir}/vendor/bin", dir),
            "/home/a/.stella/plugins/vera/bin:/home/a/.stella/plugins/vera/vendor/bin"
        );
        // Nothing else is a placeholder: no shell expansion, no other name.
        for verbatim in ["python3", "$plugin_dir", "${PLUGIN_DIR}", "${HOME}/x"] {
            assert_eq!(expand_plugin_dir(verbatim, dir), verbatim);
        }
    }
}
