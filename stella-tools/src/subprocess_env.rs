//! Environment policy for subprocesses that can execute model- or
//! repository-controlled code.
//!
//! Stella needs provider credentials in its own process, but tools do not.
//! Passing the full inherited environment to a shell, project script, hook,
//! or long-running server lets ordinary repository code print or exfiltrate
//! the credential that pays for the agent. Apply
//! [`crate::subprocess_env::scrub_spawn_env`] as the final environment
//! mutation before every such spawn;
//! [`crate::subprocess_env::scrub_sensitive_env`] is the credential-only
//! subset, for fixed helper probes that never run repository code.
//!
//! Two families are removed, and they are removed for different reasons:
//!
//! * **Credentials** ([`crate::subprocess_env::is_sensitive_env_name`]) — the variable's *value* is
//!   or names a secret. Matched by exact name, by a credential suffix, and by
//!   the names trusted settings register at startup.
//! * **Ambient authority** ([`crate::subprocess_env::is_ambient_authority_env_name`]) — the value is
//!   not a secret, but possessing the variable hands the child authority it
//!   should not inherit, or redirects what a program it runs will execute.
//!   `SSH_AUTH_SOCK` (a live agent socket signs for the user's keys), the
//!   git config/command-injection family (`GIT_CONFIG_COUNT` +
//!   `GIT_CONFIG_KEY_0` + `GIT_CONFIG_VALUE_0` sets *any* git config key for
//!   every `git` the subprocess runs; `GIT_SSH_COMMAND`, `GIT_EXTERNAL_DIFF`
//!   and `GIT_PROXY_COMMAND` each name a program git execs), and
//!   `RIPGREP_CONFIG_PATH` (a file of default `rg` arguments that silently
//!   redacts what the `grep` tool returns) live here. This is the same family
//!   `stella-cli`'s `.env`-file loader refuses to import, applied at the other
//!   end of the pipe. It lives here rather than in `stella-fleet`'s
//!   `SystemGitCli` so `stella-tools`' and `stella-cli`'s own git invocations
//!   get identical treatment from one list.
//!
//! # Re-admitting one variable: `STELLA_SUBPROCESS_ENV_ALLOW`
//!
//! Widening a scrub breaks real workflows — a Django dev server started via
//! `start_process` needs `DJANGO_SECRET_KEY`, and a deploy-key setup drives
//! git through `GIT_SSH_COMMAND`. The operator re-admits a variable by naming
//! it in the `STELLA_SUBPROCESS_ENV_ALLOW` environment variable, a
//! comma-separated list of **exact** names:
//!
//! ```sh
//! STELLA_SUBPROCESS_ENV_ALLOW=DJANGO_SECRET_KEY,SSH_AUTH_SOCK stella
//! ```
//!
//! Matching is exact (ASCII case-insensitive) by design: there are no globs
//! and no suffix forms, so the hatch can re-admit a named variable but can
//! never be used to switch the scrub off wholesale. Two further limits:
//!
//! * It is read from **Stella's own process environment** only, never from a
//!   command's `[env]` overrides — a repository tool manifest cannot widen
//!   the policy that is about to be applied to it.
//! * A name that trusted settings registered as a model credential
//!   ([`crate::subprocess_env::register_sensitive_env_names`]) is never re-admitted. The registry is
//!   monotonic for the process lifetime, so the credential paying for the
//!   agent cannot be handed back by editing one environment variable.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::sync::{OnceLock, RwLock};

/// Environment variables that re-target git at a specific repository. Tool
/// subprocesses always run against their explicit working dir; when Stella
/// itself was spawned from inside a git hook (which exports `GIT_DIR` et
/// al.), letting them leak through would silently aim every git invocation
/// at the OUTER repo instead — `git init` in a scratch dir re-initing the
/// host repo, `verify_done` diffing against the wrong HEAD. Scrub these from
/// every subprocess that shells out with an explicit dir.
pub const GIT_REPO_ENV_VARS: [&str; 8] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
];

/// Environment variables that force CLIs to colorize even when piped.
/// Everything spawned here writes to a captured pipe that is parsed
/// (`gh … --json` into serde) or fed to the model — never a terminal — so
/// an inherited `CLICOLOR_FORCE=1` from the user's shell wraps `gh`'s JSON
/// in ANSI escapes and every parse dies with "expected value at line 1
/// column 1". Scrubbing only the *force* overrides restores standard
/// pipe detection; tools stay colorless on pipes, as they'd be anywhere.
pub const FORCED_COLOR_ENV_VARS: [&str; 3] = ["CLICOLOR_FORCE", "FORCE_COLOR", "GH_FORCE_TTY"];

/// Credential-bearing AWS variables whose names do not all use one of the
/// generic secret suffixes below. Region variables are intentionally absent:
/// `AWS_REGION` is task configuration, not a credential.
const AWS_CREDENTIAL_ENV_VARS: &[&str] = &[
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_SECURITY_TOKEN",
    "AWS_WEB_IDENTITY_TOKEN_FILE",
    "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
    "AWS_CONTAINER_CREDENTIALS_FULL_URI",
    "AWS_CONTAINER_AUTHORIZATION_TOKEN",
    "AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE",
    "AWS_SHARED_CREDENTIALS_FILE",
    "AWS_PROFILE",
    "AWS_DEFAULT_PROFILE",
];

/// Other credential-source variables that provide a child access to tokens
/// without putting the token directly in the variable value.
const CREDENTIAL_SOURCE_ENV_VARS: &[&str] = &[
    "GOOGLE_APPLICATION_CREDENTIALS",
    "AZURE_FEDERATED_TOKEN_FILE",
];

/// Credential suffixes. Deliberately *specific*: bare `_KEY` and `_AUTH` are
/// absent because they overmatch ordinary configuration — `PUBLIC_KEY`,
/// `LICENSE_KEY`, `SSH_KEY_PATH`, `AUTH_URL` — and silently stripping those
/// from every model-launched subprocess breaks builds with no diagnostic.
/// Each entry here names something that is a secret in essentially every
/// spelling it appears in.
const CREDENTIAL_ENV_SUFFIXES: &[&str] = &[
    "_API_KEY",
    "_APIKEY",
    "_TOKEN",
    "_PASSWORD",
    "_SECRET",
    "_SECRET_KEY",
    "_PRIVATE_KEY",
    "_ACCESS_KEY",
    "_CREDENTIALS",
    "_CREDENTIAL",
    "_PAT",
];

/// Ambient-authority variables: not secret *values*, but channels that let a
/// child act as the user or redirect what a program it runs will execute.
///
/// `SSH_AUTH_SOCK` is a live agent socket — a child holding it can sign with
/// the user's keys without ever seeing them. The `GIT_*` entries are git's
/// documented command- and config-injection surface: `GIT_CONFIG_GLOBAL` /
/// `GIT_CONFIG_SYSTEM` repoint git at an attacker-written config file, and
/// `GIT_SSH_COMMAND` / `GIT_EXTERNAL_DIFF` / `GIT_PROXY_COMMAND` name a
/// program git will exec.
///
/// `RIPGREP_CONFIG_PATH` is the same shape as `GIT_CONFIG_GLOBAL`, one layer
/// out: it repoints `rg` at a file of default arguments. The `grep` tool
/// shells out to `rg`, so a planted config (`--max-count=1`, a `--glob`
/// hiding a directory, `--fixed-strings` defeating a regex) silently changes
/// what a search returns — and the agent reads the difference as fact rather
/// than as a redacted result. Scrubbed for the same reason git's is.
const AMBIENT_AUTHORITY_ENV_VARS: &[&str] = &[
    "SSH_AUTH_SOCK",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_SSH_COMMAND",
    "GIT_EXTERNAL_DIFF",
    "GIT_PROXY_COMMAND",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "RIPGREP_CONFIG_PATH",
];

/// The numbered halves of git's `GIT_CONFIG_COUNT` protocol
/// (`GIT_CONFIG_KEY_0`, `GIT_CONFIG_VALUE_12`, …). The index is unbounded, so
/// these are matched by prefix rather than enumerated; no legitimate variable
/// starts with either string.
const GIT_CONFIG_NUMBERED_PREFIXES: &[&str] = &["GIT_CONFIG_KEY_", "GIT_CONFIG_VALUE_"];

/// Environment variable naming exact names the operator re-admits to every
/// scrubbed subprocess. See this module's docs for the contract.
pub const SUBPROCESS_ENV_ALLOW_VAR: &str = "STELLA_SUBPROCESS_ENV_ALLOW";

/// Exact environment authentication channels documented by GitHub CLI.
/// Trusted `gh` call sites may preserve these while still removing every
/// model-provider, cloud, database, and unrelated repository secret.
pub const GITHUB_CLI_AUTH_ENV_VARS: &[&str] = &[
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
];

static REGISTERED_CREDENTIAL_ENV_VARS: OnceLock<RwLock<HashSet<String>>> = OnceLock::new();

/// Register exact credential environment names learned from trusted provider
/// configuration.
///
/// Custom providers may use a name such as `CORP_AUTH` that no suffix rule can
/// infer. Registration is monotonic for the process lifetime: once a name has
/// carried a model credential, arbitrary descendants must never inherit it.
pub fn register_sensitive_env_names<I, S>(names: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let registry = REGISTERED_CREDENTIAL_ENV_VARS.get_or_init(|| RwLock::new(HashSet::new()));
    let mut registry = registry
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    registry.extend(names.into_iter().filter_map(|name| {
        let upper = name.as_ref().to_string_lossy().to_ascii_uppercase();
        (!upper.is_empty()).then_some(upper)
    }));
}

/// Whether an environment variable name can carry a credential.
///
/// Matching is ASCII case-insensitive for parity with Windows environment
/// semantics. The suffix policy covers built-in and custom Stella providers
/// (`OPENROUTER_API_KEY`, `ANTHROPIC_API_KEY`, `VERTEX_ACCESS_TOKEN`, ...)
/// plus common repository credentials such as `GITHUB_TOKEN`. Exact AWS and
/// credential-source names cover the standard chains that do not follow a
/// generic suffix.
pub fn is_sensitive_env_name(name: &OsStr) -> bool {
    let upper = name.to_string_lossy().to_ascii_uppercase();
    matches!(upper.as_str(), "API_KEY" | "TOKEN" | "PASSWORD" | "SECRET")
        || CREDENTIAL_ENV_SUFFIXES
            .iter()
            .any(|suffix| upper.ends_with(suffix))
        || AWS_CREDENTIAL_ENV_VARS.contains(&upper.as_str())
        || CREDENTIAL_SOURCE_ENV_VARS.contains(&upper.as_str())
        || REGISTERED_CREDENTIAL_ENV_VARS
            .get()
            .is_some_and(|registry| {
                registry
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .contains(&upper)
            })
}

/// Whether an environment variable name carries ambient authority rather than
/// a secret value — an agent socket, or one of git's config/command-injection
/// channels. Separate from [`is_sensitive_env_name`] on purpose: callers that
/// ask "did this credential get registered for scrubbing?" must keep getting
/// an answer about credentials.
pub fn is_ambient_authority_env_name(name: &OsStr) -> bool {
    let upper = name.to_string_lossy().to_ascii_uppercase();
    AMBIENT_AUTHORITY_ENV_VARS.contains(&upper.as_str())
        || GIT_CONFIG_NUMBERED_PREFIXES
            .iter()
            .any(|prefix| upper.starts_with(prefix))
}

/// The full scrub predicate: everything [`scrub_sensitive_env`] removes.
pub fn is_scrubbed_env_name(name: &OsStr) -> bool {
    is_sensitive_env_name(name) || is_ambient_authority_env_name(name)
}

/// Exact names the operator re-admitted through [`SUBPROCESS_ENV_ALLOW_VAR`],
/// uppercased. Read from Stella's own environment on every scrub so a test —
/// or a `stella` invoked with a different value — is never answered from a
/// cached first reading.
fn operator_allowlist() -> Vec<String> {
    let Some(raw) = std::env::var_os(SUBPROCESS_ENV_ALLOW_VAR) else {
        return Vec::new();
    };
    raw.to_string_lossy()
        .split(',')
        .map(|entry| entry.trim().to_ascii_uppercase())
        .filter(|entry| !entry.is_empty())
        .collect()
}

fn allowlisted_sensitive_env_is_safe_to_preserve(
    name: &OsStr,
    preserved_names: &[&str],
    registered_as_model_credential: bool,
) -> bool {
    let candidate = name.to_string_lossy();
    preserved_names
        .iter()
        .any(|allowed| candidate.eq_ignore_ascii_case(allowed))
        && !registered_as_model_credential
}

fn is_registered_model_credential_name(name: &OsStr) -> bool {
    let upper = name.to_string_lossy().to_ascii_uppercase();
    REGISTERED_CREDENTIAL_ENV_VARS
        .get()
        .is_some_and(|registry| {
            registry
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .contains(&upper)
        })
}

/// Remove credentials from a Tokio command while preserving ordinary task
/// environment such as `PATH`, locale, build flags, and provider-neutral
/// configuration.
///
/// Both inherited variables and explicit command overrides are inspected.
/// The latter matters for custom tool manifests: a repository must not be
/// able to re-introduce `OPENROUTER_API_KEY` through its `[env]` table after
/// the inherited copy was removed.
pub fn scrub_sensitive_env(command: &mut tokio::process::Command) {
    scrub_sensitive_std_env(command.as_std_mut());
}

/// Remove credentials except an exact, integration-owned allowlist.
///
/// This is only for a trusted executable whose documented authentication
/// channel is itself an environment variable (for example `gh`). Arbitrary
/// shells, hooks, test commands, repository tools, and model-selected
/// executables must use [`crate::subprocess_env::scrub_sensitive_env`] with no exceptions.
pub fn scrub_sensitive_env_except(command: &mut tokio::process::Command, preserved_names: &[&str]) {
    scrub_sensitive_std_env_except(command.as_std_mut(), preserved_names);
}

/// Synchronous-command counterpart used by the few repository helpers that
/// cannot use Tokio.
pub fn scrub_sensitive_std_env(command: &mut std::process::Command) {
    scrub_sensitive_std_env_except(command, &[]);
}

/// The COMPLETE environment policy for a spawned tool subprocess: the
/// credential/ambient-authority scrub plus the two hygiene families every
/// spawn path must also remove — [`GIT_REPO_ENV_VARS`] (a surrounding git
/// hook must not re-aim the child's git at the outer repo) and
/// [`FORCED_COLOR_ENV_VARS`] (output goes to a captured pipe, never a
/// terminal). One helper so a new spawn path cannot pick up the credential
/// scrub and silently miss the other two — `bash`, `start_process`, and the
/// hook runner did exactly that.
pub fn scrub_spawn_env(command: &mut tokio::process::Command) {
    scrub_spawn_env_except(command, &[]);
}

/// [`scrub_spawn_env`] preserving an exact, integration-owned credential
/// allowlist — same contract (and same restraint) as
/// [`scrub_sensitive_env_except`].
pub fn scrub_spawn_env_except(command: &mut tokio::process::Command, preserved_names: &[&str]) {
    for var in GIT_REPO_ENV_VARS {
        command.env_remove(var);
    }
    for var in FORCED_COLOR_ENV_VARS {
        command.env_remove(var);
    }
    scrub_sensitive_env_except(command, preserved_names);
}

/// Synchronous counterpart of [`scrub_sensitive_env_except`].
pub fn scrub_sensitive_std_env_except(
    command: &mut std::process::Command,
    preserved_names: &[&str],
) {
    // A provider credential name learned from trusted settings always wins
    // over an integration allowlist collision. For example, if a custom
    // provider is configured to use `GH_TOKEN`, `gh` authentication must fail
    // closed rather than inheriting the model-spend credential. The same rule
    // governs the operator's `STELLA_SUBPROCESS_ENV_ALLOW` hatch.
    let operator_allowed = operator_allowlist();
    let operator_allowed: Vec<&str> = operator_allowed.iter().map(String::as_str).collect();
    let is_preserved = |name: &OsStr| {
        let registered = is_registered_model_credential_name(name);
        allowlisted_sensitive_env_is_safe_to_preserve(name, preserved_names, registered)
            || allowlisted_sensitive_env_is_safe_to_preserve(name, &operator_allowed, registered)
    };
    let mut names: Vec<OsString> = std::env::vars_os()
        .filter_map(|(name, _)| {
            (is_scrubbed_env_name(&name) && !is_preserved(&name)).then_some(name)
        })
        .collect();

    // `Command::env` entries are not necessarily present in Stella's own
    // environment. Snapshot them before calling `env_remove`, which mutates
    // the iterator returned by `get_envs`.
    names.extend(
        command
            .get_envs()
            .filter(|(name, value)| {
                value.is_some() && is_scrubbed_env_name(name) && !is_preserved(name)
            })
            .map(|(name, _)| name.to_os_string())
            .collect::<Vec<_>>(),
    );
    // Belt and braces for the exactly-known names: remove them whether or not
    // this process or this command carries them, so the child's environment
    // states their absence rather than inheriting one that appeared between
    // the snapshot above and the spawn.
    names.extend(
        AWS_CREDENTIAL_ENV_VARS
            .iter()
            .chain(CREDENTIAL_SOURCE_ENV_VARS)
            .chain(AMBIENT_AUTHORITY_ENV_VARS)
            .filter(|name| !is_preserved(OsStr::new(*name)))
            .map(OsString::from),
    );

    for name in names {
        command.env_remove(name);
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::ffi::OsString;
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Probe that reports only the word `leaked`, never a credential value.
    pub const PROBE_COMMAND: &str = "printf '%s|%s|%s|%s|%s' \
        \"${OPENROUTER_API_KEY:+leaked}\" \
        \"${GITHUB_TOKEN:+leaked}\" \
        \"${AWS_SECRET_ACCESS_KEY:+leaked}\" \
        \"${STELLA_TEST_BENIGN-unset}\" \
        \"${PATH:+present}\"";

    /// Serializes tests that temporarily modify the process environment and
    /// restores the caller's original values even if an assertion panics.
    pub struct InheritedCredentialFixture {
        previous: Vec<(&'static str, Option<OsString>)>,
        _lock: MutexGuard<'static, ()>,
    }

    impl InheritedCredentialFixture {
        pub fn install() -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
            let values = [
                ("OPENROUTER_API_KEY", "test-openrouter-secret"),
                ("GITHUB_TOKEN", "test-github-secret"),
                ("AWS_SECRET_ACCESS_KEY", "test-aws-secret"),
                ("STELLA_TEST_BENIGN", "visible"),
            ];
            let previous = values
                .iter()
                .map(|(name, _)| (*name, std::env::var_os(name)))
                .collect();
            for (name, value) in values {
                // SAFETY: all credential-inheritance tests use ENV_LOCK and
                // no production thread is running in this test process.
                unsafe { std::env::set_var(name, value) };
            }
            Self {
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for InheritedCredentialFixture {
        fn drop(&mut self) {
            for (name, value) in &self.previous {
                // SAFETY: the fixture still owns ENV_LOCK during restoration.
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
    }

    /// Probe for the two spawn-hygiene families [`super::scrub_spawn_env`]
    /// removes beyond credentials. `GIT_PREFIX` stands in for the git-repo
    /// family because git itself only ever *exports* it (never reads it), so
    /// setting it process-wide cannot break a concurrently running test that
    /// spawns git without a scrub.
    pub const SPAWN_HYGIENE_PROBE_COMMAND: &str =
        "printf '%s|%s' \"${GIT_PREFIX-unset}\" \"${CLICOLOR_FORCE-unset}\"";

    /// Sets one variable from each spawn-hygiene family under the same
    /// `ENV_LOCK` as [`InheritedCredentialFixture`], restoring on drop.
    pub struct SpawnHygieneFixture {
        previous: Vec<(&'static str, Option<OsString>)>,
        _lock: MutexGuard<'static, ()>,
    }

    impl SpawnHygieneFixture {
        pub fn install() -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
            let values = [("GIT_PREFIX", "sub/dir/"), ("CLICOLOR_FORCE", "1")];
            let previous = values
                .iter()
                .map(|(name, _)| (*name, std::env::var_os(name)))
                .collect();
            for (name, value) in values {
                // SAFETY: same ENV_LOCK contract as InheritedCredentialFixture.
                unsafe { std::env::set_var(name, value) };
            }
            Self {
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for SpawnHygieneFixture {
        fn drop(&mut self) {
            for (name, value) in &self.previous {
                // SAFETY: the fixture still owns ENV_LOCK during restoration.
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
    }

    pub fn assert_spawn_hygiene_scrubbed(output: &str) {
        assert_eq!(
            output, "unset|unset",
            "a git-repo or forced-color variable reached the child: {output}"
        );
    }

    /// Sets one process environment variable for the guard's lifetime,
    /// serialized against every other environment-mutating test through the
    /// same `ENV_LOCK` [`InheritedCredentialFixture`] takes.
    pub struct ScopedEnvVar {
        name: &'static str,
        previous: Option<OsString>,
        _lock: MutexGuard<'static, ()>,
    }

    impl ScopedEnvVar {
        pub fn set(name: &'static str, value: &str) -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
            let previous = std::env::var_os(name);
            // SAFETY: the guard owns ENV_LOCK for its whole lifetime and no
            // production thread runs in this test process.
            unsafe { std::env::set_var(name, value) };
            Self {
                name,
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for ScopedEnvVar {
        fn drop(&mut self) {
            // SAFETY: the guard still owns ENV_LOCK during restoration.
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var(self.name, value),
                    None => std::env::remove_var(self.name),
                }
            }
        }
    }

    pub fn assert_scrubbed(output: &str) {
        assert_eq!(
            output, "|||visible|present",
            "credential reached child or benign environment was removed: {output}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_model_credential_outranks_integration_allowlist() {
        let name = OsStr::new("GH_TOKEN");
        assert!(allowlisted_sensitive_env_is_safe_to_preserve(
            name,
            GITHUB_CLI_AUTH_ENV_VARS,
            false,
        ));
        assert!(!allowlisted_sensitive_env_is_safe_to_preserve(
            name,
            GITHUB_CLI_AUTH_ENV_VARS,
            true,
        ));
    }

    #[test]
    fn classifies_provider_repo_and_aws_credentials_without_overmatching() {
        for secret in [
            "OPENROUTER_API_KEY",
            "ANTHROPIC_API_KEY",
            "GITHUB_TOKEN",
            "DATABASE_PASSWORD",
            "STELLA_LINEAR_CLIENT_SECRET",
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_WEB_IDENTITY_TOKEN_FILE",
            "GOOGLE_APPLICATION_CREDENTIALS",
        ] {
            assert!(
                is_sensitive_env_name(OsStr::new(secret)),
                "{secret} must be classified as sensitive"
            );
        }
        for benign in [
            "PATH",
            "HOME",
            "STELLA_TEST_BENIGN",
            "AWS_REGION",
            "CARGO_TARGET_DIR",
            "TOKENIZERS_PARALLELISM",
        ] {
            assert!(
                !is_sensitive_env_name(OsStr::new(benign)),
                "{benign} must remain available to task subprocesses"
            );
        }
    }

    #[test]
    fn widened_credential_suffixes_catch_real_spellings_without_eating_configuration() {
        for secret in [
            "STRIPE_SECRET_KEY",
            "DJANGO_SECRET_KEY",
            "DEPLOY_PRIVATE_KEY",
            "MINIO_ACCESS_KEY",
            "SENTRY_APIKEY",
            "REGISTRY_CREDENTIALS",
            "VAULT_CREDENTIAL",
            "GITHUB_PAT",
        ] {
            assert!(
                is_sensitive_env_name(OsStr::new(secret)),
                "{secret} must be classified as sensitive"
            );
        }
        // `_KEY` and `_AUTH` are deliberately NOT suffixes: they overmatch
        // ordinary configuration, and stripping these would break builds
        // with no diagnostic anywhere.
        for benign in [
            "PUBLIC_KEY",
            "LICENSE_KEY",
            "SSH_KEY_PATH",
            "AUTH_URL",
            "PARTITION_KEY",
            "SORT_KEY",
        ] {
            assert!(
                !is_sensitive_env_name(OsStr::new(benign)),
                "{benign} must remain available to task subprocesses"
            );
        }
    }

    #[test]
    fn config_injection_family_and_agent_socket_are_ambient_authority_not_credentials() {
        for name in [
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_KEY_0",
            "GIT_CONFIG_VALUE_12",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_SYSTEM",
            "GIT_SSH_COMMAND",
            "GIT_EXTERNAL_DIFF",
            "GIT_PROXY_COMMAND",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "RIPGREP_CONFIG_PATH",
            "SSH_AUTH_SOCK",
        ] {
            assert!(
                is_ambient_authority_env_name(OsStr::new(name)),
                "{name} hands a child authority it must not inherit"
            );
            assert!(
                is_scrubbed_env_name(OsStr::new(name)),
                "{name} not scrubbed"
            );
            assert!(
                !is_sensitive_env_name(OsStr::new(name)),
                "{name} is ambient authority, not a credential string"
            );
        }
        // The numbered prefixes end in `_`, so a name that merely starts with
        // the same letters is untouched — and git's own ordinary variables
        // stay available to a subprocess.
        for benign in [
            "GIT_CONFIG_KEYRING",
            "GIT_AUTHOR_NAME",
            "GIT_TERMINAL_PROMPT",
            "GIT_PAGER",
        ] {
            assert!(
                !is_scrubbed_env_name(OsStr::new(benign)),
                "{benign} must remain available to task subprocesses"
            );
        }
    }

    /// The `grep` tool shells out to `rg`, which reads default arguments from
    /// the file `RIPGREP_CONFIG_PATH` names. A planted config (`--max-count=1`,
    /// a `--glob` hiding a directory) silently truncates what a search returns
    /// and the agent reads the shortfall as fact — the `GIT_CONFIG_GLOBAL`
    /// hazard, one layer out. An INHERITED value must not reach the child.
    #[tokio::test]
    async fn an_inherited_ripgrep_config_path_never_reaches_a_scrubbed_child() {
        let _planted = test_support::ScopedEnvVar::set("RIPGREP_CONFIG_PATH", "/tmp/planted-rgrc");
        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", "printf '%s' \"${RIPGREP_CONFIG_PATH-unset}\""]);
        scrub_sensitive_env(&mut command);

        let output = command.output().await.expect("spawn shell");
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), "unset");
    }

    #[tokio::test]
    async fn operator_allowlist_readmits_an_exact_name_and_only_an_exact_name() {
        let _allow = test_support::ScopedEnvVar::set(
            SUBPROCESS_ENV_ALLOW_VAR,
            " DJANGO_SECRET_KEY , GIT_SSH_COMMAND ",
        );
        let mut command = tokio::process::Command::new("sh");
        command
            .args([
                "-c",
                "printf '%s|%s|%s|%s|%s' \
                 \"${DJANGO_SECRET_KEY-unset}\" \
                 \"${GIT_SSH_COMMAND-unset}\" \
                 \"${MY_DJANGO_SECRET_KEY-unset}\" \
                 \"${GIT_CONFIG_KEY_0-unset}\" \
                 \"${OPENROUTER_API_KEY-unset}\"",
            ])
            .env("DJANGO_SECRET_KEY", "dev-server-key")
            .env("GIT_SSH_COMMAND", "ssh -i deploy_key")
            // A suffix-shaped near-miss of an allowlisted name, so an
            // allowlist that matched by suffix would re-admit it.
            .env("MY_DJANGO_SECRET_KEY", "must-not-leak")
            .env("GIT_CONFIG_KEY_0", "core.pager")
            .env("OPENROUTER_API_KEY", "must-not-leak");
        scrub_sensitive_env(&mut command);

        let output = command.output().await.expect("spawn shell");
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "dev-server-key|ssh -i deploy_key|unset|unset|unset"
        );
    }

    #[tokio::test]
    async fn operator_allowlist_cannot_re_admit_a_registered_model_credential() {
        register_sensitive_env_names(["STELLA_TEST_ALLOWLIST_MODEL_CRED"]);
        let _allow = test_support::ScopedEnvVar::set(
            SUBPROCESS_ENV_ALLOW_VAR,
            "STELLA_TEST_ALLOWLIST_MODEL_CRED",
        );
        let mut command = tokio::process::Command::new("sh");
        command
            .args([
                "-c",
                "printf '%s' \"${STELLA_TEST_ALLOWLIST_MODEL_CRED-unset}\"",
            ])
            .env("STELLA_TEST_ALLOWLIST_MODEL_CRED", "model-spend-secret");
        scrub_sensitive_env(&mut command);

        let output = command.output().await.expect("spawn shell");
        assert_eq!(String::from_utf8_lossy(&output.stdout), "unset");
    }

    #[tokio::test]
    async fn removes_explicit_secret_overrides_but_keeps_benign_overrides() {
        let mut command = tokio::process::Command::new("sh");
        command
            .args([
                "-c",
                "printf '%s|%s' \"${CUSTOM_API_KEY-unset}\" \"${STELLA_TEST_BENIGN-unset}\"",
            ])
            .env("CUSTOM_API_KEY", "must-not-leak")
            .env("STELLA_TEST_BENIGN", "kept");
        scrub_sensitive_env(&mut command);
        let output = command.output().await.expect("spawn shell");
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), "unset|kept");
    }

    #[tokio::test]
    async fn trusted_integration_can_preserve_only_its_exact_auth_names() {
        let mut command = tokio::process::Command::new("sh");
        command
            .args([
                "-c",
                "printf '%s|%s|%s|%s' \"${OPENROUTER_API_KEY-unset}\" \"${GITHUB_TOKEN-unset}\" \"${GH_TOKEN-unset}\" \"${AWS_SECRET_ACCESS_KEY-unset}\"",
            ])
            .env("OPENROUTER_API_KEY", "provider-secret")
            .env("GITHUB_TOKEN", "github-secret")
            .env("GH_TOKEN", "gh-secret")
            .env("AWS_SECRET_ACCESS_KEY", "cloud-secret");
        scrub_sensitive_env_except(&mut command, &["GH_TOKEN", "GITHUB_TOKEN"]);

        let output = command.output().await.unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "unset|github-secret|gh-secret|unset"
        );
    }

    #[test]
    fn registered_custom_provider_name_is_sensitive_without_a_secret_suffix() {
        assert!(!is_sensitive_env_name(OsStr::new("STELLA_TEST_CORP_AUTH")));
        register_sensitive_env_names(["STELLA_TEST_CORP_AUTH"]);
        assert!(is_sensitive_env_name(OsStr::new("STELLA_TEST_CORP_AUTH")));
    }
}
