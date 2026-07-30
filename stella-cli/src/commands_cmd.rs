// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella commands` — list custom slash commands, and convert markdown ones
//! to TOML.
//!
//! # Why conversion is opt-in and never part of `init`
//!
//! `stella init` **symlinks** `.claude/commands/` into `.stella/commands/`, so
//! a command edited in either place stays live in both. Converting forks that:
//! the TOML file is a copy, and the markdown original it came from keeps
//! drifting away from it. That is a real trade — TOML buys a typed
//! `allowed-tools` array and a delimiter-free `prompt`, and costs the live
//! link — and it is the user's to make, per command, deliberately. So this is
//! a command you run, never something a sync does to you.
//!
//! Conversion never deletes the source, and never overwrites an existing
//! `.toml` without `--force`. The worst outcome of a mistaken run is a file
//! the user can delete.

use std::path::{Path, PathBuf};

use stella_core::extensions::{CommandDef, command_from_file};

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
/// `prompt` always uses a multi-line basic string. A body containing `"""` is
/// the one shape that would break out of it, so those are escaped; everything
/// else is passed through byte-for-byte, because a converted prompt that is
/// not the original prompt is a silently changed command.
pub fn to_toml(cmd: &CommandDef) -> String {
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
    out.push_str("\nprompt = \"\"\"\n");
    out.push_str(&cmd.body.replace("\"\"\"", "\\\"\\\"\\\""));
    out.push_str("\n\"\"\"\n");
    out
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
        if !dry_run && let Err(e) = std::fs::write(&target, to_toml(&cmd)) {
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
    use stella_core::extensions::command_from_toml;

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
             allowed-tools: read_file, grep\n\
             model: anthropic/claude-fable-5\n\
             disable-model-invocation: true\n\
             ---\n\
             Review PR $1.\n\nBe thorough about \"quoted\" things.";
        let from_md = command_from_file("/ws/.stella/commands/review-pr.md", md).unwrap();
        let round_tripped =
            command_from_toml("/ws/.stella/commands/review-pr.toml", &to_toml(&from_md)).unwrap();

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
        let toml_src = to_toml(&cmd);
        assert!(!toml_src.contains("argument-hint"), "{toml_src}");
        assert!(!toml_src.contains("allowed-tools"), "{toml_src}");
        assert!(!toml_src.contains("model"), "{toml_src}");
        assert!(!toml_src.contains("disable-model-invocation"), "{toml_src}");
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
}
