// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella commands` — list custom slash commands, and convert markdown ones
//! to TOML.
//!
//! # Why conversion is opt-in, and what `init` may and may not do
//!
//! `stella init` **symlinks** `.claude/commands/` into `.stella/commands/`, so
//! a command edited in either place stays live in both. Converting forks that:
//! the TOML file is a copy, and the markdown original it came from keeps
//! drifting away from it. That is a real trade — TOML buys a typed
//! `allowed-tools` array and a delimiter-free `prompt`, and costs the live
//! link — and it is the user's to make, deliberately. So conversion is never
//! something a sync does to you.
//!
//! `init` does now *ask*, once per workspace, through
//! [`crate::commands_offer`] — which is the same rule, not an exception to
//! it: the offer states the cost, converts nothing without an explicit yes,
//! stays silent when nobody can answer, and never asks a second time. What
//! remains forbidden is the thing this paragraph originally guarded against,
//! a conversion the user did not choose.
//!
//! Conversion never deletes the source, and never overwrites an existing
//! `.toml` without `--force`. The worst outcome of a mistaken run is a file
//! the user can delete.

use std::path::{Path, PathBuf};

use crate::extensions::plan::{CommandDef, command_from_file};

/// `stella commands <cmd>`.
#[derive(Debug, clap::Subcommand)]
pub enum CommandsCmd {
    /// List the custom slash commands this workspace offers, with the file
    /// each came from.
    List,
    /// Convert markdown command definitions to TOML.
    ///
    /// Reads `<dir>` (default `.stella/commands/`), writes a `<slug>.toml`
    /// beside each `<slug>.md`, and leaves the markdown in place.
    Convert {
        /// Directory to convert (default: `.stella/commands/`).
        #[arg(value_name = "DIR")]
        dir: Option<PathBuf>,
        /// Show what would be written without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Overwrite a `<slug>.toml` that already exists.
        #[arg(long)]
        force: bool,
    },
}

/// Run `stella commands <cmd>`. Offline: definition files only.
pub fn run_commands(cmd: &CommandsCmd) -> Result<(), String> {
    let root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    match cmd {
        CommandsCmd::List => {
            // Listing goes through the same authority-gated loader the chat
            // surfaces use, on the REAL resolved policy — a command hidden
            // from the composer must not appear here, and one the composer
            // offers must not be missing here. `AuthorityPolicy::default()`
            // would be neither: it denies project prompts unconditionally, so
            // this would under-report every workspace that allows them.
            let authority = crate::settings::Settings::load(&root)
                .map(|s| s.authority_policy)
                .unwrap_or_default();
            let loaded =
                crate::extensions::CustomExtensions::load_with_authority(&root, &authority);
            if loaded.commands.is_empty() {
                println!("no custom commands — add one under .stella/commands/");
                return Ok(());
            }
            for command in &loaded.commands {
                let hint = command.argument_hint.as_deref().unwrap_or("");
                println!(
                    "/{:<24} {:<40} {}",
                    command.invocation(),
                    truncate(&command.description, 40),
                    hint
                );
                println!("  {}", command.source_path);
            }
            Ok(())
        }
        CommandsCmd::Convert {
            dir,
            dry_run,
            force,
        } => {
            let dir = dir
                .clone()
                .unwrap_or_else(|| root.join(".stella").join("commands"));
            let done = convert_dir(&dir, *dry_run, *force)?;
            if done.is_empty() {
                println!("no markdown commands under {}", dir.display());
                return Ok(());
            }
            let mut written = 0usize;
            for entry in &done {
                match &entry.skipped {
                    None => {
                        written += 1;
                        let verb = if *dry_run { "would write" } else { "wrote" };
                        println!("{verb} {}", entry.target.display());
                    }
                    Some(why) => println!("skipped {}: {why}", entry.source.display()),
                }
            }
            println!(
                "{written}/{} converted — the markdown originals are untouched",
                done.len()
            );
            Ok(())
        }
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

/// Render one [`CommandDef`] as a TOML document.
///
/// Only fields the definition actually carries are emitted — a converted file
/// full of `argument-hint = ""` would read as "this command declares an empty
/// hint" rather than "this command has no hint", and the two mean different
/// things to both a reader and the parser.
///
/// `prompt` always uses a multi-line **literal** string (`'''…'''`). It
/// applies no escape processing, so the body lands byte-for-byte. A basic
/// (`"""…"""`) string is not safe here. It reads a trailing backslash as a
/// line continuation and eats the newline after it, so a multi-line shell
/// example joins onto one line. It also treats `\(` — the start of an
/// ordinary jq expression like `"#\(.number)"` — as an invalid escape, so
/// the file fails to parse. A literal string has neither problem. Its only
/// limit is the sequence `'''`, which it cannot carry. A body with that
/// sequence is rejected, the same way the loader already rejects a body
/// that will not round-trip.
pub fn to_toml(cmd: &CommandDef) -> Result<String, String> {
    if cmd.body.contains("'''") {
        return Err(format!(
            "{}: the prompt body contains `'''`, which a TOML literal string \
             cannot carry — rewrite the body to avoid that sequence",
            cmd.name
        ));
    }
    let mut out = String::new();
    out.push_str(&format!("name = {}\n", quote(&cmd.name)));
    out.push_str(&format!("description = {}\n", quote(&cmd.description)));
    if let Some(hint) = &cmd.argument_hint {
        out.push_str(&format!("argument-hint = {}\n", quote(hint)));
    }
    if let Some(tools) = &cmd.allowed_tools {
        let items: Vec<String> = tools.iter().map(|t| quote(t)).collect();
        out.push_str(&format!("allowed-tools = [{}]\n", items.join(", ")));
    }
    if let Some(model) = &cmd.model {
        out.push_str(&format!("model = {}\n", quote(model)));
    }
    if !cmd.model_invocable {
        out.push_str("disable-model-invocation = true\n");
    }
    out.push_str("\nprompt = '''\n");
    out.push_str(&cmd.body);
    out.push_str("\n'''\n");
    Ok(out)
}

/// A TOML basic string. Backslashes first, or the escapes added after would
/// themselves be escaped.
fn quote(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    format!("\"{escaped}\"")
}

/// One conversion the run would perform or did.
#[derive(Debug, PartialEq, Eq)]
pub struct Converted {
    pub source: PathBuf,
    pub target: PathBuf,
    /// Why nothing was written, when nothing was.
    pub skipped: Option<String>,
}

/// Plan (and, unless `dry_run`, perform) the conversion of every `<slug>.md`
/// under `dir`, one level deep so namespace directories convert too.
pub fn convert_dir(dir: &Path, dry_run: bool, force: bool) -> Result<Vec<Converted>, String> {
    let mut out = Vec::new();
    let mut sources = Vec::new();
    collect_markdown(dir, &mut sources)?;
    sources.sort();

    for source in sources {
        let target = source.with_extension("toml");
        let raw = match std::fs::read_to_string(&source) {
            Ok(raw) => raw,
            Err(e) => {
                out.push(Converted {
                    target,
                    skipped: Some(format!("unreadable: {e}")),
                    source,
                });
                continue;
            }
        };
        // Parsed, never transliterated: the converted file must mean what the
        // loader thinks the markdown means, so it goes through the same parser
        // the loader uses. A definition that will not load is not converted —
        // writing a TOML copy of a broken command just makes two broken ones.
        let cmd = match command_from_file(&source.display().to_string(), &raw) {
            Ok(cmd) => cmd,
            Err(diag) => {
                out.push(Converted {
                    target,
                    skipped: Some(format!("does not load: {:?}", diag.problem)),
                    source,
                });
                continue;
            }
        };
        if target.exists() && !force {
            out.push(Converted {
                target,
                skipped: Some("a .toml already exists (use --force)".to_string()),
                source,
            });
            continue;
        }
        // A body the renderer cannot carry (see `to_toml`) is reported the
        // same way a body the loader cannot parse is, above — never written
        // as a silently wrong file.
        let toml_src = match to_toml(&cmd) {
            Ok(toml_src) => toml_src,
            Err(e) => {
                out.push(Converted {
                    target,
                    skipped: Some(e),
                    source,
                });
                continue;
            }
        };
        if !dry_run && let Err(e) = std::fs::write(&target, toml_src) {
            out.push(Converted {
                target,
                skipped: Some(format!("could not write: {e}")),
                source,
            });
            continue;
        }
        out.push(Converted {
            source,
            target,
            skipped: None,
        });
    }
    Ok(out)
}

/// Markdown definitions in `dir` and one level below it (namespaces). The
/// nested `<slug>/COMMAND.md` layout is included; anything deeper is not,
/// matching what the loader reads.
fn collect_markdown(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let hidden = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_none_or(|n| n.starts_with('.'));
        if hidden {
            continue;
        }
        if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        } else if path.is_dir() {
            for child in std::fs::read_dir(&path).into_iter().flatten().flatten() {
                let child = child.path();
                if child.extension().is_some_and(|e| e == "md")
                    && child
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| !n.starts_with('.'))
                {
                    out.push(child);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::plan::command_from_toml;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// The witness for the converter: a round trip must not change what the
    /// command IS. Parsing the emitted TOML has to yield the same definition
    /// the markdown yielded — otherwise converting a library silently rewrites
    /// every command in it.
    #[test]
    fn a_converted_command_parses_back_to_the_same_definition() {
        let md = "---\n\
             name: review-pr\n\
             description: Review a pull request\n\
             argument-hint: <pr-number>\n\
             allowed-tools: task_list, get_environment\n\
             model: anthropic/claude-fable-5\n\
             disable-model-invocation: true\n\
             ---\n\
             Review PR $1.\n\nBe thorough about \"quoted\" things.";
        let from_md = command_from_file("/ws/.stella/commands/review-pr.md", md).unwrap();
        let toml_src = to_toml(&from_md).unwrap();
        let round_tripped =
            command_from_toml("/ws/.stella/commands/review-pr.toml", &toml_src).unwrap();

        assert_eq!(round_tripped.name, from_md.name);
        assert_eq!(round_tripped.description, from_md.description);
        assert_eq!(round_tripped.argument_hint, from_md.argument_hint);
        assert_eq!(round_tripped.allowed_tools, from_md.allowed_tools);
        assert_eq!(round_tripped.model, from_md.model);
        assert_eq!(round_tripped.model_invocable, from_md.model_invocable);
        assert_eq!(
            round_tripped.body, from_md.body,
            "the prompt must survive byte-for-byte"
        );
    }

    /// A command with no optional fields must not gain any. An emitted
    /// `disable-model-invocation = false` would be a capability statement the
    /// author never made.
    #[test]
    fn absent_fields_are_absent_in_the_output() {
        let cmd = command_from_file("/ws/.stella/commands/ship.md", "Ship it.").unwrap();
        let toml_src = to_toml(&cmd).unwrap();
        assert!(!toml_src.contains("argument-hint"), "{toml_src}");
        assert!(!toml_src.contains("allowed-tools"), "{toml_src}");
        assert!(!toml_src.contains("model"), "{toml_src}");
        assert!(!toml_src.contains("disable-model-invocation"), "{toml_src}");
    }

    /// A basic string reads a trailing backslash as a line continuation. It
    /// eats the newline and joins a multi-line shell example onto one line.
    /// A literal string applies no escape processing, so the line break
    /// survives.
    #[test]
    fn a_trailing_backslash_line_continuation_survives_conversion() {
        let body = "gh issue list \\\n  --json number,title\n\nline three";
        let cmd = command_from_file("/ws/.stella/commands/list-issues.md", body).unwrap();
        let toml_src = to_toml(&cmd).unwrap();
        let round_tripped =
            command_from_toml("/ws/.stella/commands/list-issues.toml", &toml_src).unwrap();
        assert_eq!(
            round_tripped.body, cmd.body,
            "a trailing backslash must not become a line continuation: {toml_src}"
        );
        assert!(
            toml_src.contains("gh issue list \\\n"),
            "the backslash itself must reach the file untouched: {toml_src}"
        );
    }

    /// `\(` is not a valid basic-string escape. A jq expression like
    /// `"#\(.number)"` makes a basic string fail to parse. A literal string
    /// passes it through untouched.
    #[test]
    fn a_jq_interpolation_escape_survives_conversion() {
        let body = r##"gh issue list --json number | jq -r '"#\(.number)"'"##;
        let cmd = command_from_file("/ws/.stella/commands/list-issues.md", body).unwrap();
        let toml_src = to_toml(&cmd).unwrap();
        let round_tripped =
            command_from_toml("/ws/.stella/commands/list-issues.toml", &toml_src).unwrap();
        assert_eq!(round_tripped.body, cmd.body, "{toml_src}");
    }

    /// A literal string is the one TOML shape that cannot carry `'''` inside
    /// it. A prompt containing that sequence must be reported, not silently
    /// truncated or written as a file that fails to parse.
    #[test]
    fn a_body_containing_triple_single_quotes_is_rejected() {
        let cmd = command_from_file("/ws/.stella/commands/quote.md", "before '''after").unwrap();
        let err = to_toml(&cmd).unwrap_err();
        assert!(err.contains("'''"), "{err}");
    }

    #[test]
    fn conversion_writes_beside_the_source_and_keeps_it() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("commands");
        write(&dir.join("ship.md"), "Ship it.");
        write(&dir.join("vercel/deploy.md"), "Deploy $1.");

        let done = convert_dir(&dir, false, false).unwrap();
        assert_eq!(done.len(), 2, "{done:?}");
        assert!(done.iter().all(|c| c.skipped.is_none()), "{done:?}");
        assert!(dir.join("ship.toml").exists());
        assert!(
            dir.join("vercel/deploy.toml").exists(),
            "namespaces convert"
        );
        assert!(dir.join("ship.md").exists(), "the source is never deleted");
    }

    #[test]
    fn an_existing_toml_is_never_clobbered_without_force() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("commands");
        write(&dir.join("ship.md"), "Ship it.");
        write(&dir.join("ship.toml"), "prompt = \"hand written\"\n");

        let done = convert_dir(&dir, false, false).unwrap();
        assert!(done[0].skipped.is_some(), "{done:?}");
        assert_eq!(
            std::fs::read_to_string(dir.join("ship.toml")).unwrap(),
            "prompt = \"hand written\"\n"
        );

        let forced = convert_dir(&dir, false, true).unwrap();
        assert!(forced[0].skipped.is_none(), "{forced:?}");
        assert!(
            std::fs::read_to_string(dir.join("ship.toml"))
                .unwrap()
                .contains("Ship it.")
        );
    }

    #[test]
    fn a_dry_run_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("commands");
        write(&dir.join("ship.md"), "Ship it.");

        let done = convert_dir(&dir, true, false).unwrap();
        assert_eq!(done.len(), 1);
        assert!(done[0].skipped.is_none());
        assert!(!dir.join("ship.toml").exists());
    }

    /// A definition the loader rejects is reported, not converted — two broken
    /// commands are worse than one.
    #[test]
    fn a_command_that_does_not_load_is_not_converted() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("commands");
        write(&dir.join("empty.md"), "---\nname: empty\n---\n");

        let done = convert_dir(&dir, false, false).unwrap();
        assert!(done[0].skipped.is_some(), "{done:?}");
        assert!(!dir.join("empty.toml").exists());
    }

    /// A body the literal-string renderer cannot carry (`'''`) is reported
    /// the same way, on a real `dry_run` and a real write, rather than
    /// writing a `.toml` that fails to parse.
    #[test]
    fn a_command_whose_body_cannot_render_is_not_converted() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("commands");
        write(&dir.join("quote.md"), "before '''after");

        let done = convert_dir(&dir, false, false).unwrap();
        assert!(done[0].skipped.is_some(), "{done:?}");
        assert!(!dir.join("quote.toml").exists());

        let dry = convert_dir(&dir, true, false).unwrap();
        assert!(dry[0].skipped.is_some(), "{dry:?}");
    }
}
