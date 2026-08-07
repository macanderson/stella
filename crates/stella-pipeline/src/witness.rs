//! Witness authoring: the front half of deterministic verification (L-E11).
//! When no `--test-command` is configured, the pipeline asks an independent
//! model (the verifier's resolution — witness ≠ worker, so the test that defines
//! "done" is authored by the same independent role that enforces it) to write
//! a **witness test**: a test that FAILS on the current code and will pass
//! once the goal is met. Its command becomes the flip oracle's tracked
//! command, so the repo's defining contract — "verified done, not claimed
//! done" — holds even when the user armed nothing.
//!
//! # Visible, not hidden — integrity by tamper exclusion
//!
//! The witness is deliberately **visible to the worker** once it exists:
//! authoring runs after the execute turn (demand-driven, gated on the
//! warrant), so the execute turn never sees it, but every revise turn
//! iterates with the failing test on disk — which is where convergence comes
//! from, and a test file is discoverable by any worker with a shell anyway.
//! Integrity comes instead
//! from *tamper exclusion* — the complete filesystem identity of the one test
//! artifact the witness turn created is snapshotted. A flip is only credited
//! when its bytes, type, mode, link count, and path remain unchanged at verify
//! time ([`witness_identity_matches`] over the [`ArtifactIdentity`] recorded in
//! [`Witness::files`]). A worker that edits, replaces, links, renames, or
//! deletes the witness hard-fails the candidate; a model verifier cannot override
//! that authority violation.
//!
//! # The pure/orchestration split
//!
//! Like `triage`/`verify`, everything here is a synchronous function over
//! owned data: prompt builders, the response parser, the watchlist delta, and
//! the tamper check. Running the witness engine turn, executing the authored
//! command, and the one bounded repair retry live in [`crate::pipeline`].
//!
//! # Fails now, and could fail meaningfully
//!
//! "The test must fail first" is necessary and not sufficient: a test with no
//! assertions, one asserting only over constants, or a bare `#[should_panic]`
//! all fail now and flip green without constraining the change.
//! [`density::screen_witness_source`] is the static half of the answer — a
//! lexical screen the `create_witness_test` boundary applies to the authored
//! bytes, so a vacuous artifact is refused inside the author's own turn rather
//! than after a baseline run and a repair turn have been paid for (#863).

pub mod airlock;
pub mod density;
pub mod warrant;

use std::collections::HashMap;

use crate::ports::{ArtifactIdentity, RecalledFrame, TestInvocation};

/// The marker line the witness author must end its reply with. Scanned
/// case-insensitively by [`parse_witness_command`]; the LAST occurrence wins
/// (the model may quote the marker while reasoning before its final answer).
pub const TEST_COMMAND_MARKER: &str = "TEST_COMMAND:";

/// Why a model- or user-authored test command was not accepted as a typed
/// test invocation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TestInvocationError {
    /// The command was empty or contained unbalanced quoting.
    #[error("the test command is empty or has invalid quoting")]
    InvalidSyntax,
    /// Shell control syntax is never valid at this boundary.
    #[error("shell operators, redirection, and expansion are not allowed in test commands")]
    ShellSyntax,
    /// Only explicit test-runner forms are accepted.
    #[error("unsupported test command `{0}`")]
    Unsupported(String),
}

/// A witness author crossed the one-new-test-file boundary.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WitnessArtifactError {
    /// Existing tracked content was changed or removed.
    #[error("witness author modified tracked file(s): {}", .0.join(", "))]
    TrackedMutation(Vec<String>),
    /// The author emitted a `TEST_COMMAND` but never created any file at all.
    /// Its own variant because the two failures deserve different fates: an
    /// absent artifact is a *cannot-author* condition the pipeline degrades
    /// past (the worker's change is real and already done), while a wrong or
    /// multiple-file delta is an integrity violation that stays fail-closed.
    #[error("witness author emitted a test command but created no file")]
    NothingCreated,
    /// The untracked delta was not exactly one newly created test artifact.
    #[error("witness author must create exactly one new test file; changed: {}", .0.join(", "))]
    InvalidArtifact(Vec<String>),
    /// The test file's language does not match the selected typed runner.
    #[error("witness artifact `{path}` does not match test runner `{program}`")]
    InvocationMismatch { path: String, program: String },
    /// The path was not a regular, single-link file matching the fingerprint
    /// captured in the repo-status delta.
    #[error("witness artifact `{0}` has an unsafe or unstable filesystem identity")]
    InvalidIdentity(String),
}

/// Parse a deliberately small test-command vocabulary into an enumerable
/// program plus argv. This is quote-aware only to preserve arguments with
/// spaces; it is not a shell parser and rejects every shell control surface.
pub fn parse_test_invocation(command: &str) -> Result<TestInvocation, TestInvocationError> {
    let words = split_test_words(command)?;
    let (program, args) = words
        .split_first()
        .ok_or(TestInvocationError::InvalidSyntax)?;
    let allowed = match program.as_str() {
        "cargo" => {
            matches!(args.first().map(String::as_str), Some("test"))
                || matches!(
                    (
                        args.first().map(String::as_str),
                        args.get(1).map(String::as_str)
                    ),
                    (Some("nextest"), Some("run"))
                )
        }
        "pnpm" => {
            matches!(args.first().map(String::as_str), Some("test"))
                || matches!(
                    (
                        args.first().map(String::as_str),
                        args.get(1).map(String::as_str),
                        args.get(2).map(String::as_str)
                    ),
                    (Some("exec"), Some("vitest"), Some("run"))
                )
        }
        "npm" | "yarn" | "bun" => matches!(args.first().map(String::as_str), Some("test")),
        "vitest" => matches!(args.first().map(String::as_str), Some("run")),
        "npx" | "bunx" => matches!(
            (
                args.first().map(String::as_str),
                args.get(1).map(String::as_str)
            ),
            (Some("vitest"), Some("run"))
        ),
        "pytest" => true,
        "python" | "python3" => matches!(
            (
                args.first().map(String::as_str),
                args.get(1).map(String::as_str)
            ),
            (Some("-m"), Some("pytest"))
        ),
        "go" | "dotnet" => matches!(args.first().map(String::as_str), Some("test")),
        // #2064: the runner-less-workspace witness — exactly one positional
        // `.sh` script, the authored artifact. `split_test_words` has
        // already rejected pipelines, redirects, substitution, and
        // backgrounding anywhere in the line, and the script path gets the
        // same escape checks as every other runner's arguments. `sh -c
        // '<inline>'` is deliberately NOT accepted, although the issue
        // sketched it: the inline program rides as one opaque argv word, so
        // the per-argument confinement checks cannot see a path inside it —
        // and an invocation that names no artifact could never be
        // tamper-excluded anyway. A bare `sh` is an interactive shell, not
        // a test.
        "sh" => match args {
            [script] => script.ends_with(".sh") && !script.starts_with('-'),
            _ => false,
        },
        _ => false,
    };
    if !allowed {
        return Err(TestInvocationError::Unsupported(command.to_string()));
    }
    validate_local_args(program, args)?;
    Ok(TestInvocation {
        program: program.clone(),
        args: args.to_vec(),
    })
}

fn split_test_words(command: &str) -> Result<Vec<String>, TestInvocationError> {
    if command.contains("$(")
        || command.contains('`')
        || command.chars().any(|ch| {
            matches!(
                ch,
                '&' | '|'
                    | ';'
                    | '<'
                    | '>'
                    | '\n'
                    | '\r'
                    | '\u{ff06}'
                    | '\u{ff5c}'
                    | '\u{ff1b}'
                    | '\u{ff1c}'
                    | '\u{ff1e}'
            ) || (ch.is_whitespace() && !matches!(ch, ' ' | '\t'))
                || ch.is_control()
        })
    {
        return Err(TestInvocationError::ShellSyntax);
    }
    let mut words = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut single = false;
    let mut double = false;
    let mut chars = command.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\'' if !double => {
                single = !single;
                started = true;
            }
            '"' if !single => {
                double = !double;
                started = true;
            }
            '\\' if !single => {
                let Some(escaped) = chars.next() else {
                    return Err(TestInvocationError::InvalidSyntax);
                };
                current.push(escaped);
                started = true;
            }
            '&' | '|' | ';' | '<' | '>' if !single && !double => {
                return Err(TestInvocationError::ShellSyntax);
            }
            c if c.is_whitespace() && !single && !double => {
                if started {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if single || double {
        return Err(TestInvocationError::InvalidSyntax);
    }
    if started {
        words.push(current);
    }
    Ok(words)
}

fn validate_local_args(program: &str, args: &[String]) -> Result<(), TestInvocationError> {
    let forbidden_flags: &[&str] = match program {
        "cargo" => &["--manifest-path", "--config", "-C", "--target-dir"],
        "pnpm" | "npm" | "yarn" | "bun" | "vitest" | "npx" | "bunx" => &[
            "--prefix",
            "--dir",
            "--cwd",
            "-C",
            "--userconfig",
            "--globalconfig",
            "--script-shell",
        ],
        "pytest" | "python" | "python3" => &["--rootdir", "--confcutdir", "-c", "--basetemp"],
        "go" => &["-C", "-exec", "-toolexec", "-overlay", "-modfile"],
        "dotnet" => &["--test-adapter-path", "--settings"],
        _ => &[],
    };
    for arg in args {
        let normalized = arg.replace('\\', "/");
        let windows_absolute = normalized.as_bytes().get(1) == Some(&b':')
            && normalized.as_bytes().get(2) == Some(&b'/');
        if std::path::Path::new(arg).is_absolute()
            || windows_absolute
            || normalized.split('/').any(|component| component == "..")
        {
            return Err(TestInvocationError::ShellSyntax);
        }
        let flag = arg.split_once('=').map_or(arg.as_str(), |(flag, _)| flag);
        if forbidden_flags.contains(&flag) {
            return Err(TestInvocationError::ShellSyntax);
        }
    }
    Ok(())
}

/// Validate the witness author's complete working-tree delta and return the
/// content-hash baseline for the one accepted test artifact.
pub fn validate_witness_artifact(
    tracked_before: &HashMap<String, String>,
    tracked_after: &HashMap<String, String>,
    untracked_before: &HashMap<String, String>,
    untracked_after: &HashMap<String, String>,
) -> Result<HashMap<String, String>, WitnessArtifactError> {
    let tracked = changed_paths(tracked_before, tracked_after);
    if !tracked.is_empty() {
        return Err(WitnessArtifactError::TrackedMutation(tracked));
    }
    let changed = changed_paths(untracked_before, untracked_after);
    if changed.is_empty() {
        return Err(WitnessArtifactError::NothingCreated);
    }
    let accepted = match changed.as_slice() {
        [path]
            if !untracked_before.contains_key(path)
                && untracked_after.contains_key(path)
                && is_witness_test_path(path) =>
        {
            path
        }
        _ => return Err(WitnessArtifactError::InvalidArtifact(changed)),
    };
    Ok(HashMap::from([(
        accepted.clone(),
        untracked_after[accepted].clone(),
    )]))
}

/// Require the accepted test file's language to match the typed runner that
/// will execute it. This prevents an unrelated executable artifact from
/// riding beside a harmless test command.
pub fn validate_witness_invocation(
    path: &str,
    invocation: &TestInvocation,
) -> Result<(), WitnessArtifactError> {
    let normalized_path = path.replace('\\', "/");
    let extension = path
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    let matches = match invocation.program.as_str() {
        "cargo" => {
            extension == "rs" && cargo_invocation_is_exact(&normalized_path, &invocation.args)
        }
        "pytest" | "python" | "python3" => {
            extension == "py" && pytest_invocation_targets(&normalized_path, invocation)
        }
        "pnpm" | "vitest" | "npx" | "bunx" => {
            matches!(
                extension.as_str(),
                "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs"
            ) && vitest_invocation_targets(&normalized_path, invocation)
        }
        "go" => extension == "go" && go_invocation_is_exact(&normalized_path, &invocation.args),
        "dotnet" => {
            extension == "cs" && dotnet_invocation_is_exact(&normalized_path, &invocation.args)
        }
        // Only the script form: `sh -c '<inline>'` names no artifact, and an
        // invocation that does not pin the authored file cannot be
        // tamper-excluded (#2064).
        "sh" => extension == "sh" && sh_invocation_targets(&normalized_path, &invocation.args),
        _ => false,
    };
    if matches {
        Ok(())
    } else {
        Err(WitnessArtifactError::InvocationMismatch {
            path: path.to_string(),
            program: invocation.program.clone(),
        })
    }
}

fn cargo_invocation_is_exact(path: &str, args: &[String]) -> bool {
    if args.first().map(String::as_str) != Some("test")
        || args.iter().filter(|arg| arg.as_str() == "--test").count() != 1
        || args.iter().any(|arg| {
            matches!(
                arg.as_str(),
                "--no-run" | "--workspace" | "--all" | "--all-targets" | "--tests" | "--exclude"
            )
        })
    {
        return false;
    }
    let Some(stem) = path
        .rsplit('/')
        .next()
        .and_then(|name| name.strip_suffix(".rs"))
    else {
        return false;
    };
    let target_flag_index = match args.get(1).map(String::as_str) {
        Some("--test") => 1,
        Some("-p" | "--package")
            if args
                .get(2)
                .is_some_and(|package| !package.is_empty() && !package.starts_with('-')) =>
        {
            3
        }
        _ => return false,
    };
    if args.get(target_flag_index).map(String::as_str) != Some("--test")
        || args.get(target_flag_index + 1).map(String::as_str) != Some(stem)
    {
        return false;
    }
    let selector_index = target_flag_index + 2;
    let separator = selector_index + 1;
    if args.get(separator).map(String::as_str) != Some("--") {
        return false;
    }
    args.get(selector_index)
        .is_some_and(|selector| !selector.is_empty() && !selector.starts_with('-'))
        && args.get(separator + 1).map(String::as_str) == Some("--exact")
        && args[separator + 2..].iter().all(|arg| arg == "--nocapture")
}

fn pytest_invocation_targets(path: &str, invocation: &TestInvocation) -> bool {
    let args = match invocation.program.as_str() {
        "pytest" => invocation.args.as_slice(),
        "python" | "python3"
            if invocation.args.first().map(String::as_str) == Some("-m")
                && invocation.args.get(1).map(String::as_str) == Some("pytest") =>
        {
            &invocation.args[2..]
        }
        _ => return false,
    };
    let Some(target) = args.first().map(|arg| arg.replace('\\', "/")) else {
        return false;
    };
    (target == path
        || target
            .strip_prefix(path)
            .is_some_and(|rest| rest.starts_with("::")))
        && args[1..].iter().all(|arg| {
            matches!(
                arg.as_str(),
                "-q" | "--quiet" | "-v" | "--verbose" | "-x" | "-s"
            ) || arg.starts_with("--maxfail=")
        })
}

fn vitest_invocation_targets(path: &str, invocation: &TestInvocation) -> bool {
    let args = match invocation.program.as_str() {
        "vitest" if invocation.args.first().map(String::as_str) == Some("run") => {
            &invocation.args[1..]
        }
        "pnpm"
            if invocation.args.first().map(String::as_str) == Some("exec")
                && invocation.args.get(1).map(String::as_str) == Some("vitest")
                && invocation.args.get(2).map(String::as_str) == Some("run") =>
        {
            &invocation.args[3..]
        }
        "npx" | "bunx"
            if invocation.args.first().map(String::as_str) == Some("vitest")
                && invocation.args.get(1).map(String::as_str) == Some("run") =>
        {
            &invocation.args[2..]
        }
        _ => return false,
    };
    args.len() == 1 && args[0].replace('\\', "/") == path
}

fn go_invocation_is_exact(path: &str, args: &[String]) -> bool {
    if args.first().map(String::as_str) != Some("test") {
        return false;
    }
    let parent = path.rsplit_once('/').map_or(".", |(parent, _)| parent);
    let package = if parent == "." {
        ".".to_string()
    } else {
        format!("./{parent}")
    };
    if args.get(1) != Some(&package) {
        return false;
    }
    if args.get(2).map(String::as_str) != Some("-run") {
        return false;
    }
    let Some(selector) = args.get(3) else {
        return false;
    };
    let Some(exact) = selector
        .strip_prefix('^')
        .and_then(|selector| selector.strip_suffix('$'))
    else {
        return false;
    };
    let trailing_is_safe = args[4..]
        .chunks(2)
        .all(|chunk| matches!(chunk, [flag, value] if flag == "-count" && value != "0"));
    exact.starts_with("Test")
        && exact.len() > "Test".len()
        && exact
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        && trailing_is_safe
}

/// The one accepted shell form: a single positional argument naming exactly
/// the authored script (a leading `./` tolerated). No flags, no second
/// argument — a script that needs parameters to fail is not a witness.
fn sh_invocation_targets(path: &str, args: &[String]) -> bool {
    match args {
        [script] => {
            let script = script.replace('\\', "/");
            script.strip_prefix("./").unwrap_or(&script) == path
        }
        _ => false,
    }
}

fn dotnet_invocation_is_exact(path: &str, args: &[String]) -> bool {
    if args.first().map(String::as_str) != Some("test") {
        return false;
    }
    let Some(project) = args.get(1).map(|arg| arg.replace('\\', "/")) else {
        return false;
    };
    if !project.ends_with(".csproj") {
        return false;
    }
    let project_dir = project.rsplit_once('/').map_or("", |(parent, _)| parent);
    if !project_dir.is_empty()
        && path != project_dir
        && !path.starts_with(&format!("{project_dir}/"))
    {
        return false;
    }
    let filter = match args {
        [_, _, flag, value] if flag == "--filter" => Some(value.as_str()),
        [_, _, flag] => flag.strip_prefix("--filter="),
        _ => None,
    };
    filter.is_some_and(|filter| {
        let Some(identity) = filter.strip_prefix("FullyQualifiedName=") else {
            return false;
        };
        !identity.is_empty()
            && identity
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '+'))
    })
}

/// Pin the accepted delta entry to a no-follow filesystem identity. The
/// identity must have been observed at the accepted path itself — an
/// observation the adapter attests was resolved elsewhere (or could not
/// locate at all) is not the accepted artifact.
///
/// Returns the identity it accepted rather than `()`, so the caller that needs
/// the pinned value gets it *from the proof* instead of re-unwrapping the same
/// `Option` afterwards (#1789). The unwrap was locally sound and one line from
/// what proved it, which is exactly how it survived review; making the
/// signature carry the proof means no future edit between the two lines can
/// make it unsound again.
pub fn validate_witness_identity(
    path: &str,
    expected_fingerprint: &str,
    identity: Option<ArtifactIdentity>,
) -> Result<ArtifactIdentity, WitnessArtifactError> {
    match identity {
        Some(identity)
            if identity.is_regular_single_link()
                && identity.path == path
                && identity.fingerprint == expected_fingerprint =>
        {
            Ok(identity)
        }
        _ => Err(WitnessArtifactError::InvalidIdentity(path.to_string())),
    }
}

fn changed_paths(before: &HashMap<String, String>, after: &HashMap<String, String>) -> Vec<String> {
    let mut paths: Vec<String> = before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .cloned()
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

/// Whether a candidate-relative path has a supported witness-test shape.
///
/// Rust is the one language here with no filename-only form: cargo can only
/// run an integration test from a `tests/` directory, so the `"rs"` arm
/// requires `recognized_dir` outright. A bare `_test.` fallback accepted
/// `src/backdoor_test.rs` as a witness artifact — after which
/// [`validate_witness_invocation`]'s required `cargo test --test <stem>` can
/// never pass, the flip never happens, the run burns its full revision
/// budget, and a verifier scoring the diff may still PASS, adopting the stray
/// production file into the user's real tree. The other arms keep their
/// filename forms because their runners genuinely collect by filename
/// wherever the file sits (`pytest test_*.py`, `go test ./... _test.go`).
pub fn is_witness_test_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    let name = normalized.rsplit('/').next().unwrap_or(&normalized);
    let recognized_dir = normalized
        .split('/')
        .any(|part| matches!(part, "test" | "tests" | "__tests__" | "spec" | "specs"));
    let extension = name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    match extension {
        "rs" => recognized_dir,
        "py" => recognized_dir || name.starts_with("test_") || name.contains("_test."),
        "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" => {
            recognized_dir || name.contains(".test.") || name.contains(".spec.")
        }
        "go" => recognized_dir || name.ends_with("_test.go"),
        "cs" => recognized_dir || name.ends_with("tests.cs"),
        // Shell witnesses (#2064) have no runner-imposed collection rule, so
        // the name itself must declare intent: `witness_check.sh`,
        // `test_tls.sh`. An arbitrary `deploy.sh` is not a witness artifact.
        "sh" => {
            recognized_dir
                || name.starts_with("test_")
                || name.contains("_test.")
                || name.contains("witness")
        }
        _ => false,
    }
}

/// A validated witness: the flip-oracle command plus the filesystem identity
/// of the one new test artifact the witness turn created (the tamper baseline
/// [`witness_identity_matches`] is re-checked against at verify time).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Witness {
    /// The user-facing command the flip oracle names in evidence.
    pub command: String,
    /// Parsed process invocation used for every baseline/final test run.
    pub invocation: TestInvocation,
    /// `path -> identity` for the one accepted, newly created test file.
    /// Tracked edits, non-test files, and edits to pre-existing untracked
    /// files are rejected before candidate execution.
    pub files: HashMap<String, ArtifactIdentity>,
    /// The accepted failing baseline run's combined output tail. Carried so
    /// the transplanted observation arms the oracle through
    /// [`crate::verify::FlipOracle::observe_run`] — which records the
    /// baseline's failing test names and thereby arms the same-failure rule
    /// (#867) — instead of the name-blind `observe`, which left that guard
    /// inert for every authored witness.
    pub baseline_output: String,
}

/// Whether the current no-follow filesystem observation is exactly the
/// regular, single-link artifact accepted at witness authoring time —
/// including the location the observation was made at, so a witness that was
/// renamed to a different path never matches even when an aliased lookup
/// still resolves the pinned path to its unchanged bytes.
pub fn witness_identity_matches(
    expected: &ArtifactIdentity,
    current: Option<&ArtifactIdentity>,
) -> bool {
    expected.is_regular_single_link() && current == Some(expected)
}

/// The closed runner vocabulary [`parse_test_invocation`] accepts, by leading
/// program — and therefore the natural probe list for runner availability
/// (#1539). Kept beside the parser so extending one without the other is a
/// single-file review question.
///
/// `sh` is the floor (#2064): a POSIX shell is present in every container the
/// agent already executes in, and the witness contract — a command that fails
/// on the old code and passes on the new — is exactly a script's exit code.
/// Without it, a workspace with no language toolchain (an nginx config task,
/// git surgery, a system service) has an empty availability set and the
/// witness stage degrades before authoring anything, which measured as
/// `witness_authored` in 1 of 51 trials across the whole arenabench corpus.
pub const RUNNER_VOCABULARY: &[&str] = &[
    "cargo", "pnpm", "npm", "yarn", "bun", "vitest", "npx", "bunx", "pytest", "python", "python3",
    "go", "dotnet", "sh",
];

/// The cheapest invocation that answers "is this runner actually usable
/// here?" for a vocabulary program — version-style, discovers and runs no
/// test (#1539). `None` for a program outside the vocabulary.
///
/// `python`/`python3` probe `-m pytest --version`, not the bare interpreter:
/// [`parse_test_invocation`] only accepts them as pytest hosts, and an
/// interpreter without the module is exactly the missing toolchain the probe
/// exists to catch. Probing is program-level by design — `cargo --version`
/// cannot vouch for a `cargo nextest` subcommand — so an available program
/// with a missing subcommand still degrades at the baseline observation,
/// exactly as before.
pub fn runner_probe(program: &str) -> Option<TestInvocation> {
    let args: &[&str] = match program {
        "go" => &["version"],
        "python" | "python3" => &["-m", "pytest", "--version"],
        // `sh --version` is non-portable — dash and busybox reject it — so
        // the probe is the cheapest no-op the contract allows: it spawns,
        // discovers nothing, runs no test, and answers "a shell exists".
        "sh" => &["-c", "exit 0"],
        "cargo" | "pnpm" | "npm" | "yarn" | "bun" | "vitest" | "npx" | "bunx" | "pytest"
        | "dotnet" => &["--version"],
        _ => return None,
    };
    Some(TestInvocation {
        program: program.to_string(),
        args: args.iter().map(|a| (*a).to_string()).collect(),
    })
}

/// The witness author's fixed system prompt (#1786): the role and every hard
/// requirement, byte-identical for the life of the process — the same
/// contract as `VERIFIER_INSTRUCTIONS` (#1434). This block used to open the
/// *user* message, ahead of the volatile sections, which re-billed the whole
/// instruction set uncached on every author call, every repair call, and
/// every tool round-trip inside them — the one verification role that runs a
/// multi-step tool loop was the one paying full freight. As the system
/// message it rides the cache-markable stable prefix (#1474); everything
/// per-call stays in [`witness_prompt`].
pub const WITNESS_SYSTEM_PROMPT: &str = "You are the WITNESS AUTHOR for a coding agent: a precise test author who writes a \
     minimal test that FAILS on the current code and will PASS once the goal you are \
     given is correctly accomplished. The fail→pass flip of your test is what verifies \
     the work. You never modify production code and never fix the problem yourself.\n\n\
     Hard requirements:\n\
     - Create ONE NEW test file. Never modify existing files, and never touch \
     production code — the implementation is someone else's job.\n\
     - CHOOSE A RUNNER THIS REPOSITORY ALREADY USES. You cannot execute anything in \
     this role, so you cannot discover a missing toolchain — and a command whose \
     runner is not installed does not fail the test, it produces NO observation at \
     all, which discards your witness and leaves the work unverified. Pick the \
     ecosystem the repository listing evidences (a manifest such as \
     `Cargo.toml`, `package.json`, `pyproject.toml`/`setup.py`, `go.mod`, or a \
     `*.csproj`, plus existing tests written for it) and match the conventions of \
     the tests already there. When the workspace has no language toolchain and \
     `sh` is the only available runner, the witness is a POSIX shell script (see \
     the available-runners section); if not even a shell is available, say so in \
     prose and emit no TEST_COMMAND line rather than guessing.\n\
     - Put it where that runner collects it. Rust integration tests MUST \
     live in `tests/` (cargo cannot run a test file under `src/`); Python, Vitest, Go \
     and .NET may use their filename conventions.\n\
     - The test must fail NOW for the RIGHT reason (it exercises the missing/broken \
     behavior), not because of a typo, a missing import, or a harness error.\n\
     - ASSERT on a value the goal decides. A test with no assertions, one comparing \
     constants (`assert_eq!(2, 2)`), one comparing a value to itself, or a bare \
     `#[should_panic]` / `raises(Exception)` is REFUSED at creation — each of those \
     flips green without constraining the change. Name the expected panic if a panic \
     is what you mean to prove.\n\
     - Explore with `read_file` and `glob` (names only) to find the test directories \
     and conventions; create with `create_witness_test`. No general write, edit, \
     process, network, or external action is available in this role.\n\
     - The command must directly name this artifact and an exact test: for Rust use \
     `cargo test --test <file-stem> <selector> -- --exact`; for Python/Vitest name the \
     file path; for Go/.NET include an exact test filter. Never run a whole suite.\n\
     - End your reply with exactly one line:\n\
     TEST_COMMAND: <the direct, artifact-specific test command>";

/// The witness author's per-call user prompt: split context exactly like the
/// planner (goal + recall + repo structure, never the worker transcript —
/// L-E6). The fixed role and hard requirements ride separately as
/// [`WITNESS_SYSTEM_PROMPT`]; the parts the prompt promises —
/// new file only, must fail now, marker line — are what
/// [`parse_witness_command`] and the pipeline's fail-check enforce
/// mechanically, so the prose is guidance over an enforced contract.
///
/// `available_runners` is the probed availability set (#1539): the runners
/// the pipeline confirmed this workspace can actually spawn. The author's
/// role cannot execute anything, so this is the one fact about the toolchain
/// it can be GIVEN rather than left to infer — and the pipeline enforces the
/// choice afterwards, so the section is a constraint, not a hint.
pub fn witness_prompt(
    goal: &str,
    recall: &[RecalledFrame],
    repo_structure: &str,
    available_runners: &[String],
) -> String {
    let mut s = String::new();
    if !available_runners.is_empty() {
        // Probed fact, not inference (#1539): the pipeline spawned each
        // vocabulary runner's version probe in this very workspace. The
        // author's TEST_COMMAND is checked against this set, so naming a
        // runner outside it discards the witness. The wording stays honest
        // about the probe's granularity: `npx --version` answering says
        // nothing about `vitest` being installed, so the set is a ceiling
        // on runner *programs*, never a warranty for their subcommands.
        s.push_str(&format!(
            "\n## Test runners available in this workspace\n{}\nEach of these answered a \
             version probe here, and your TEST_COMMAND must use one of them — any other \
             runner discards your witness. The probe is program-level only: it cannot \
             vouch for a subcommand or a package the program would have to fetch (`cargo \
             nextest`, `npx vitest`), so prefer an invocation the repository listing \
             below evidences the project already using.\n",
            available_runners.join(", ")
        ));
        // #2064: when the shell is the ONLY runner, a model asked for "a
        // test" tends to write a pytest file anyway — into a workspace with
        // nothing to collect it. Say the shape explicitly, as a constraint
        // derived from the probe rather than a guess left to the author.
        if available_runners.iter().all(|runner| runner == "sh") {
            s.push_str(
                "\nThis workspace has NO language test framework — `sh` is the only \
                 runner. Author the witness as ONE POSIX shell script that asserts \
                 observable state the goal decides (a served endpoint, a file or \
                 certificate the change must create, a git fact, a program's output), \
                 exiting non-zero NOW and zero once the goal is met. Name it so its \
                 intent is legible (e.g. `witness_check.sh`) and end with \
                 `TEST_COMMAND: sh <path-to-script>`. Do NOT write a pytest, cargo, or \
                 other framework test here: nothing exists to collect it.\n",
            );
        }
    }
    if !repo_structure.trim().is_empty() {
        s.push_str("\n## Repository structure\n");
        s.push_str(repo_structure.trim());
        s.push('\n');
    }
    if !recall.is_empty() {
        s.push_str("\n## Recalled context\n");
        for f in recall {
            s.push_str("- [");
            s.push_str(&f.citation_label);
            s.push_str("] ");
            s.push_str(f.content.trim());
            s.push('\n');
        }
    }
    s.push_str("\n## Goal\n");
    s.push_str(goal.trim());
    // The first section opens with its own separating newline; with the
    // fixed block gone to the system message there is nothing above it.
    s.trim_start().to_string()
}

/// The one bounded repair retry (the L-V2 pattern): the authored test passed
/// on the *unmodified* code, so it witnesses nothing. Sent into the same
/// witness thread; a second failure to produce a failing test discards the
/// witness (the pipeline continues without a witness, never loops).
pub fn witness_repair_prompt(command: &str) -> String {
    format!(
        "Your witness test PASSED on the current, unmodified code — it proves nothing, \
         because only a fail→pass flip counts as verification. Rewrite the test so it fails \
         NOW for the right reason (it must exercise the behavior the goal will add or fix). \
         Call `create_witness_test` again with the corrected file — it REPLACES your \
         previous artifact, which is discarded. The command that just passed was:\n\
         {command}\n\n\
         End your reply with the corrected `TEST_COMMAND:` line."
    )
}

/// Extract the witness command from the author's reply: the LAST
/// `TEST_COMMAND:` line (case-insensitive), stripped of surrounding
/// whitespace and backticks. `None` when no non-empty command is found — the
/// caller treats that like a failed witness stage (continue without it, never guess).
pub fn parse_witness_command(text: &str) -> Option<String> {
    let mut found: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim().trim_start_matches('`');
        // `get(..n)`, never a bare `[..n]` slice: the marker's length is a BYTE
        // count, and this scans model-authored text. A reply line that opens
        // with multi-byte characters (any non-ASCII prose) puts that index
        // mid-character, and slicing there panics — taking the whole run down
        // over a witness author that happened to explain itself in Japanese.
        // A non-boundary is simply not the marker, so `None` skips the line.
        let Some(head) = trimmed.get(..TEST_COMMAND_MARKER.len()) else {
            continue;
        };
        if head.eq_ignore_ascii_case(TEST_COMMAND_MARKER) {
            let cmd = trimmed[TEST_COMMAND_MARKER.len()..]
                .trim()
                .trim_matches('`')
                .trim();
            if !cmd.is_empty() {
                found = Some(cmd.to_string());
            }
        }
    }
    found
}

#[cfg(test)]
mod tests;
