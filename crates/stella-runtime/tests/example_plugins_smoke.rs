//! The three `verify-{rs,py,ts}` example plugins, graded **here** (#3523).
//!
//! `doc:pipeline-as-plugins` §9 rule 4 asks that the three reference plugins in
//! `macanderson/stella-examples` fail the PR that breaks them. They did not:
//! before this file, `rg 'verify-rs' --glob '!docs/**'` returned no hit in this
//! repository at all. The examples are the public proof that a plugin can be
//! written in any language against this socket, and the socket's grammar lives
//! *here* — so a change to `stella_plugin`'s manifest rules or wire types could
//! invalidate all three with every check in this repository green.
//!
//! # What it grades
//!
//! Three tiers, cheapest first, and each is a different way for the examples to
//! break:
//!
//! 1. **The manifests still load** against *this* repository's
//!    `PluginManifest::from_toml_str`. This is the decisive tier: it is the
//!    assertion that turns red when a grammar change here orphans the examples,
//!    which is the exact failure §9 rule 4 exists to catch.
//! 2. **Rule 1 still holds** — the three manifests are byte-identical except
//!    the single `argv` line naming each implementation's program. That is the
//!    examples' own invariant (`plugins/ci/check-manifests-identical.py`), and
//!    it is worth re-checking here because a grammar change that forces an
//!    edit to one of them is exactly the moment the three drift apart.
//! 3. **The plugins still answer** — each committed `after_turn` vector goes
//!    through the host's own [`SubprocessWrapper`] and comes back decoding as
//!    this repository's `stella_plugin::wire` types, equal to its golden. The
//!    same mechanism `goal_plugin_conformance.rs` and its three siblings
//!    already use, pointed at another repository's plugins.
//!
//! # The pin, and why the checkout is not this file's business
//!
//! The examples are resolved from a checkout the caller provides
//! (`STELLA_EXAMPLES_DIR`), at the commit `scripts/stella-examples-pin.txt`
//! names. A test that fetched over the network would be a test that fails when
//! GitHub does; `.github/workflows/example-plugins.yml` does the checkout and
//! this file grades what it finds. If the checkout carries a `.git`, its HEAD
//! is asserted against the pin, so a workflow that drifted to `main` is caught
//! rather than silently grading the wrong tree.
//!
//! # A skip is reported, and in CI a skip is a failure
//!
//! Rust and Python run unconditionally in CI; TypeScript needs `node` and a
//! build step, so it may legitimately skip. Every skip prints a line naming
//! itself, and when `STELLA_EXAMPLES_REQUIRE` names a language, skipping it
//! **fails**. A silently skipped language check is how "we support three
//! languages" becomes false without any PR being red, and that is the failure
//! mode this arrangement exists to make impossible.
//!
//! `cfg(unix)` for `wrapper_socket.rs`'s reason (#3497).

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use stella_plugin::{PluginManifest, WrapperRequest, WrapperResponse};
use stella_runtime::wrapper::{SubprocessWrapper, TurnWrapper};

/// The three implementations, and the toolchain each needs to actually run.
const LANGUAGES: [&str; 3] = ["rs", "py", "ts"];

/// Where `scripts/stella-examples-pin.txt` lives, from this crate.
fn pin_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/stella-examples-pin.txt")
}

/// `(repo, sha)` as the pin file declares them.
fn pin() -> (String, String) {
    let text = fs::read_to_string(pin_path()).expect("scripts/stella-examples-pin.txt is tracked");
    let mut repo = None;
    let mut sha = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match line.split_once('=') {
            Some((key, value)) => match key.trim() {
                "repo" => repo = Some(value.trim().to_string()),
                "sha" => sha = Some(value.trim().to_string()),
                other => panic!("unknown key in the pin file: {other}"),
            },
            None => panic!("the pin file is `key = value` lines: {line}"),
        }
    }
    (
        repo.expect("the pin names a repo"),
        sha.expect("the pin names a sha"),
    )
}

/// Say a language was not graded, in a form a reader will notice — and fail
/// outright when the caller declared this language mandatory.
fn skip(language: &str, why: &str) {
    assert!(
        !required(language),
        "STELLA_EXAMPLES_REQUIRE names `{language}`, so skipping it is a failure: {why}"
    );
    println!("SKIPPED verify-{language}: {why}");
}

/// Whether the caller declared this language must run.
///
/// `STELLA_EXAMPLES_REQUIRE=rs,py` is what the workflow sets. Absent, every
/// language is optional — which is right for a developer's machine and wrong
/// for CI, so CI says so explicitly rather than this file guessing from `CI`.
fn required(language: &str) -> bool {
    std::env::var("STELLA_EXAMPLES_REQUIRE")
        .map(|names| names.split(',').any(|name| name.trim() == language))
        .unwrap_or(false)
}

/// The checkout the caller provided, verified against the pin.
///
/// `None` — with a printed reason — when no checkout was given, which is the
/// ordinary case on a developer's machine.
fn examples() -> Option<PathBuf> {
    let (repo, sha) = pin();
    let Ok(dir) = std::env::var("STELLA_EXAMPLES_DIR") else {
        assert!(
            std::env::var("STELLA_EXAMPLES_REQUIRE").is_err(),
            "STELLA_EXAMPLES_REQUIRE is set but STELLA_EXAMPLES_DIR is not — the caller \
             asked for languages to be graded and provided no checkout to grade"
        );
        println!(
            "SKIPPED every language: STELLA_EXAMPLES_DIR is unset. Check out {repo} at \
             {sha} and point it here; .github/workflows/example-plugins.yml does exactly that."
        );
        return None;
    };
    let dir = PathBuf::from(dir);
    assert!(
        dir.join("plugins").is_dir(),
        "{} does not look like a {repo} checkout: no plugins/ directory",
        dir.display()
    );

    // A checkout without `.git` is a tarball export, which is a legitimate way
    // to provide one — so this asserts the pin only when it *can*.
    if dir.join(".git").exists() {
        let head = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&dir)
            .output()
            .expect("git is on PATH wherever a git checkout is");
        let head = String::from_utf8_lossy(&head.stdout).trim().to_string();
        assert_eq!(
            head, sha,
            "the checkout is at {head} but scripts/stella-examples-pin.txt names {sha} — \
             bump the pin deliberately, in its own commit, rather than grading whatever \
             `main` happens to be"
        );
    }
    Some(dir)
}

fn manifest_text(examples: &Path, language: &str) -> String {
    let path = examples
        .join("plugins")
        .join(format!("verify-{language}"))
        .join("plugin.toml");
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} is unreadable: {error}", path.display()))
}

/// **The witness (§9 rule 4, tier 1).** All three example manifests still load
/// against this repository's own grammar.
///
/// This is the assertion the whole harness exists for. `stella_plugin`'s
/// manifest rules live here; the examples live in another repository and were
/// referenced from nothing but prose. A change to `PluginManifest`'s
/// validation could orphan all three with every check in this tree green —
/// which is precisely what #3523 found had already happened once, against
/// #3499 and #3501.
#[test]
fn the_three_example_manifests_load_against_this_repositorys_grammar() {
    let Some(examples) = examples() else {
        return;
    };
    for language in LANGUAGES {
        let text = manifest_text(&examples, language);
        let manifest = PluginManifest::from_toml_str(&text).unwrap_or_else(|error| {
            panic!(
                "verify-{language}'s manifest no longer loads against this repository's \
                 grammar: {error:?}\n\nEither this change needs to keep accepting it, or \
                 the examples need the same change and the pin needs bumping to the commit \
                 that made it."
            )
        });
        assert_eq!(manifest.name, "verify");
        assert!(
            manifest.runtime.is_some(),
            "verify-{language} declares [runtime]: it is a plugin a host spawns"
        );
    }
}

/// **The witness (§9 rule 4, tier 2).** The three manifests stay byte-identical
/// except the one `argv` line.
///
/// The examples' own rule 1, enforced by their
/// `plugins/ci/check-manifests-identical.py`. It is re-checked here rather than
/// trusted because the moment it is most likely to break is a grammar change in
/// *this* repository forcing an edit to one of them.
#[test]
fn the_three_example_manifests_differ_only_in_the_argv_line() {
    let Some(examples) = examples() else {
        return;
    };
    let texts: Vec<(&str, String)> = LANGUAGES
        .iter()
        .map(|language| (*language, manifest_text(&examples, language)))
        .collect();

    let (first_language, first) = &texts[0];
    for (language, text) in texts.iter().skip(1) {
        let differing: Vec<(usize, &str, &str)> = first
            .lines()
            .zip(text.lines())
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(index, (a, b))| (index + 1, a, b))
            .collect();
        assert_eq!(
            first.lines().count(),
            text.lines().count(),
            "verify-{first_language} and verify-{language} have different line counts, so \
             they differ by more than one argv line"
        );
        assert_eq!(
            differing.len(),
            1,
            "verify-{first_language} and verify-{language} must differ in exactly one line \
             (the argv naming each implementation's program); they differ in {}: {differing:#?}",
            differing.len()
        );
        let (_, a, b) = differing[0];
        assert!(
            a.trim_start().starts_with("argv") && b.trim_start().starts_with("argv"),
            "the one differing line must be the argv, not {a:?} vs {b:?}"
        );
    }
}

/// Whether a language's program can actually be run from this checkout, and
/// the argv to run it with.
fn runnable(examples: &Path, language: &str) -> Result<(Vec<String>, PluginManifest), String> {
    let text = manifest_text(examples, language);
    let manifest =
        PluginManifest::from_toml_str(&text).map_err(|error| format!("manifest: {error:?}"))?;
    let dir = examples
        .join("plugins")
        .join(format!("verify-{language}"))
        .display()
        .to_string();
    let argv: Vec<String> = manifest
        .runtime
        .as_ref()
        .ok_or("no [runtime]")?
        .argv
        .iter()
        .map(|arg| stella_plugin::expand_plugin_dir(arg, Path::new(&dir)))
        .collect();
    let program = argv.first().ok_or("empty argv")?;

    // Two things can be missing, and an early draft of this checked only the
    // first — which let `verify-ts` through on a machine with `node` installed
    // and no `dist/`, so the skip became a hard failure inside the grading
    // loop instead of a reported skip.
    //
    // 1. The program itself: an interpreter is looked up on PATH, a compiled
    //    artifact must exist on disk.
    if program.contains('/') {
        if !Path::new(program).exists() {
            return Err(format!(
                "{program} is not built — the workflow builds it before grading"
            ));
        }
    } else if Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {program}"))
        .output()
        .is_ok_and(|out| !out.status.success())
    {
        return Err(format!("{program} is not on PATH"));
    }
    // 2. Anything the interpreter is pointed at. Every remaining argv entry
    //    that resolved to a path inside this plugin's directory is an artifact
    //    that has to be there — `node dist/main.js` needs the build as much as
    //    a compiled binary is one.
    for argument in argv.iter().skip(1) {
        if argument.starts_with(&dir) && !Path::new(argument).exists() {
            return Err(format!(
                "{argument} is not built — the workflow builds it before grading"
            ));
        }
    }
    Ok((argv, manifest))
}

fn vectors(examples: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(examples.join("plugins").join("testdata"))
        .expect("the examples ship their vectors at plugins/testdata")
        .map(|entry| entry.expect("a readable vector").path())
        .filter(|path| path.to_string_lossy().ends_with(".request.json"))
        .collect();
    found.sort();
    assert!(!found.is_empty(), "the vector directory must not be empty");
    found
}

fn sibling(request: &Path, suffix: &str) -> PathBuf {
    let name = request
        .file_name()
        .expect("a vector has a file name")
        .to_string_lossy()
        .replace(".request.json", suffix);
    request.with_file_name(name)
}

/// **The witness (§9 rule 4, tier 3).** Each example answers its committed
/// vectors, through the host's own transport, decoded by the host's own types.
///
/// Only the vectors that carry an `.expected.json` are driven here: a
/// `.refusal.txt` sibling grades a request a *typed* host cannot send, which is
/// the examples' own conformance runner's job rather than this one's — this
/// harness asks whether the plugin still speaks to **this** host.
///
/// `test-duration-ms` is normalised out of the comparison: it is a wall-clock
/// measurement, and asserting on it would make this check fail on a slow
/// runner rather than on a real break. Every other measurement is compared
/// exactly.
#[tokio::test]
async fn each_example_answers_its_committed_vectors_through_this_hosts_transport() {
    let Some(examples) = examples() else {
        return;
    };
    let mut graded_languages = 0;

    for language in LANGUAGES {
        let (argv, manifest) = match runnable(&examples, language) {
            Ok(pair) => pair,
            Err(why) => {
                skip(language, &why);
                continue;
            }
        };
        let runtime = manifest.runtime.as_ref().expect("checked in `runnable`");
        let wrapper = SubprocessWrapper::declare(
            argv,
            runtime.child_env(|name| std::env::var(name).ok()),
            Duration::from_secs(runtime.timeout_secs),
        )
        .expect("the manifest declares a program and a budget")
        .wrapper;

        let mut graded = 0;
        for vector in vectors(&examples) {
            let golden_path = sibling(&vector, ".expected.json");
            if !golden_path.exists() {
                continue;
            }
            let request: WrapperRequest =
                serde_json::from_str(&fs::read_to_string(&vector).expect("a readable vector"))
                    .expect("a vector decodes as this host's own request type");
            let WrapperRequest::AfterTurn(body) = request else {
                continue;
            };
            let golden: WrapperResponse =
                serde_json::from_str(&fs::read_to_string(&golden_path).expect("a readable golden"))
                    .expect("a golden decodes as this host's own response type");
            let WrapperResponse::AfterTurn(expected) = golden else {
                panic!("an after_turn vector's golden must be an after_turn response");
            };

            let answered = wrapper
                .after_turn(body)
                .await
                .unwrap_or_else(|error| panic!("verify-{language} on {vector:?}: {error}"));

            let mut observed = answered.evidence;
            let mut expected = expected.evidence;
            // Wall clock, not a claim about the work — see the doc comment.
            observed.measurements.remove("test-duration-ms");
            expected.measurements.remove("test-duration-ms");
            assert_eq!(
                observed.flip,
                expected.flip,
                "verify-{language} reported a different flip from its golden on {}",
                vector.display()
            );
            assert_eq!(
                observed.measurements,
                expected.measurements,
                "verify-{language} reported different measurements from its golden on {}",
                vector.display()
            );
            graded += 1;
        }
        assert!(
            graded > 0,
            "verify-{language} was runnable but no after_turn vector graded it — the vector \
             set moved, and this check would otherwise pass by doing nothing"
        );
        println!("graded verify-{language} against {graded} vector(s)");
        graded_languages += 1;
    }

    if std::env::var("STELLA_EXAMPLES_REQUIRE").is_ok() {
        assert!(
            graded_languages > 0,
            "no language was graded at all, which cannot be right when the caller named some"
        );
    }
}
