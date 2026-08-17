//! The root `stella --help` layout.
//!
//! clap renders one flat, alphabet-free list of every subcommand, which for a
//! 30-command binary is a wall: `run` and `chat` — what almost every session
//! actually starts with — sit in the same undifferentiated column as
//! `telemetry` and `cloud`, and on a normal terminal the list plus the global
//! flags overflow the window, so the first screen a new user sees is the
//! bottom of the flag list. clap 4 can group *arguments* under headings
//! (`Arg::help_heading`) but has no equivalent for subcommands, so the root
//! command's own `{subcommands}` section is replaced with the grouped index
//! this module renders.
//!
//! The index is **derived, not duplicated**: the summary beside each name is
//! that command's real `about`, read back off the built `clap::Command`. The
//! only thing declared here is which group a command belongs to, and
//! `every_command_is_grouped_exactly_once` fails the suite if a new command
//! is not assigned one. A command that somehow reaches a release unassigned
//! still lists — under `Other` — because a help page that silently omits a
//! command is worse than an ugly one.
//!
//! Reordering the index ahead of the flags fixed the *first* screen; it did
//! not fix the *last* one. Several session flags carry a multi-paragraph
//! `long_help` (the rationale `--tools` or `--upstream-pin` deserves), and
//! `--help` used to print every one of them in full — enough text that on a
//! real terminal the index, printed first, had scrolled off the top by the
//! time the command finished and the cursor came to rest, so reaching it
//! meant scrolling back up past the whole flag list. `-h` never had this
//! problem: clap only ever prints the short `help` line there.
//! `shorten_root_session_flags` gives `--help` the same one-line-per-flag
//! budget at the root. The paragraph is not deleted, only relocated to where
//! a reader who typed `stella <command> --help` is already looking for
//! depth.

use std::collections::BTreeSet;

use clap::CommandFactory;

use super::Cli;

/// Which commands appear under which heading, in the order the help prints
/// them. Ordered by what a session actually reaches for: the commands that do
/// work first, the ones that explain the workspace next, and the setup a user
/// touches once at the bottom.
///
/// Adding a command means adding it here. That is deliberate — the grouping
/// is an editorial judgement about what the command is *for*, which cannot be
/// inferred from its type.
const GROUPS: &[(&str, &[&str])] = &[
    (
        "Run the agent",
        &["run", "chat", "goal", "resume", "daemon", "init"],
    ),
    (
        "Run many at once",
        &["fleet", "monitor", "self-driving", "arena"],
    ),
    (
        "Ask about this workspace",
        &["search", "storage", "tools", "commands"],
    ),
    (
        "Steer what the agent knows",
        &["ingest", "context", "proposals", "memory"],
    ),
    (
        "What it cost, what happened",
        &[
            "stats",
            "scoreboard",
            "observe",
            "inspect",
            "calibration",
            "usage",
            "tune",
            "dataset",
        ],
    ),
    (
        "Set up",
        &[
            "auth",
            "models",
            "mcp",
            "plugin",
            "config",
            "migrate",
            "doctor",
            "completions",
            "cloud",
            "telemetry",
            "version",
        ],
    ),
];

/// clap adds this one itself during `build()`, and the footer already tells
/// the reader how to use it. Listing it as a command alongside `run` would be
/// noise.
const NOT_A_COMMAND: &str = "help";

/// The two-space indent every index row opens with, and the gutter between
/// the name column and the summary. `about_lines_fit_the_command_index`
/// derives the summary budget from these rather than hardcoding a number, so
/// changing the layout here cannot leave the test asserting the old shape.
const INDENT: usize = 2;
const GUTTER: usize = 2;

/// Section order and placement. Identical to clap's `DEFAULT_TEMPLATE` except
/// that `{all-args}` — which would emit clap's own flat `Commands:` list and
/// then the flags — is split, so the grouped index (`{after-help}`) can take
/// the command list's place and the flags follow it.
///
/// The flags' heading is a literal, styled with the command's own header
/// style, because `{options}` renders args *without* any heading: clap's
/// source calls that out ("we don't have a good way of handling help_heading
/// in the template"), so dropping `{all-args}` for `{options}` costs the
/// `Options:` line unless the template supplies one. `GlobalArgs` still
/// carries `next_help_heading` for the subcommand pages, which do use
/// `{all-args}` and do honour it.
///
/// Not propagated to subcommands: `Command::help_template` is per-command, so
/// `stella fleet --help` and every other leaf keeps clap's stock layout.
fn root_template(cmd: &clap::Command) -> String {
    let header = cmd.get_styles().get_header();
    format!(
        "\
{{before-help}}{{about-with-newline}}
{{usage-heading}} {{usage}}{{after-help}}

{on}Session flags:{off}
{{options}}

Run `stella help <command>` for a command's full description.
Session flags work on either side of it: `stella run … --spend-limit 5`.
Flags are one line here; `stella <command> --help` explains each in full.",
        on = header.render(),
        off = header.render_reset(),
    )
}

/// `Cli::command()` with the grouped root help installed.
///
/// Every entry point that renders or introspects the CLI goes through here —
/// parsing, `stella completions`, and the tests — so no surface can drift
/// onto the ungrouped layout.
pub(crate) fn command() -> clap::Command {
    let mut cmd = Cli::command();
    // `build` materializes the flags clap synthesizes (`--help`, `--version`)
    // and the propagated globals, so the index below reads the same tree a
    // user's terminal will.
    cmd.build();
    // Must run after `build()`, not before: propagation of `global = true`
    // args into every subcommand (`Command::_propagate_global_args`) already
    // happened above, as a deep clone per subcommand. Shortening the root's
    // own copy now touches only what the root page renders — `stella run
    // --help` and friends keep their subcommand-local clone, long_help intact.
    cmd = shorten_root_session_flags(cmd);
    let index = render_index(&cmd);
    let template = root_template(&cmd);
    cmd.help_template(template)
        .after_help(index.clone())
        .after_long_help(index)
}

/// Drop every session flag's `long_help` from the root command's own copy, so
/// `--help` falls back to the same one-line `help` summary `-h` already used
/// (clap's long-help renderer tries `long_help` first, then `help` — see
/// `clap_builder::output::help_template`). Scoped by `is_global_set()` rather
/// than a hardcoded flag list: every session flag is `global = true` by
/// `every_root_flag_is_global` (src/tests.rs), so a new flag inherits this
/// automatically instead of silently reopening the wall of text.
///
/// `Command::mut_args` (plural), not `mut_arg`: `build()` above has already
/// populated clap's internal keymap, which caches each flag's *position* in
/// the arg vector for fast lookup during parsing. `mut_arg` (singular)
/// implements one flag by removing it and pushing it back at the *end* of
/// the vector, which shifts every later flag's position without refreshing
/// the keymap's cached indices — so after 17 of those, `--help` itself
/// silently resolved to the wrong `Arg`, and the process fell through to the
/// default chat REPL instead of printing help and exiting. `mut_args`
/// rewrites every element via `drain().map(f).extend()`, which preserves
/// vector order exactly, so the keymap's positions stay valid. No test in
/// this file's suite would have caught the corrupt-keymap version — they all
/// inspect the built `Command` tree, never run real parsing — so this was
/// only found by running `stella --help` end to end.
fn shorten_root_session_flags(cmd: clap::Command) -> clap::Command {
    cmd.mut_args(|a| {
        if a.is_global_set() {
            a.long_help(None::<&str>)
        } else {
            a
        }
    })
}

/// Render the grouped command list.
///
/// Styled with the command's *own* `Styles` rather than hardcoded escapes, so
/// the group headings match clap's `Commands:`/`Options:` headings exactly and
/// follow any future restyling. Raw SGR in help text is clap's documented
/// mechanism (see `StyledStr`); it reaches the terminal through
/// `anstream::AutoStream`, which strips it for `NO_COLOR`, `--color never`,
/// and a non-tty stdout.
fn render_index(cmd: &clap::Command) -> String {
    let styles = cmd.get_styles();
    let (header, literal) = (styles.get_header(), styles.get_literal());

    let listed: Vec<(&str, String)> = cmd
        .get_subcommands()
        .filter(|c| !c.is_hide_set() && c.get_name() != NOT_A_COMMAND)
        .map(|c| {
            (
                c.get_name(),
                c.get_about().map(|a| a.to_string()).unwrap_or_default(),
            )
        })
        .collect();

    // Pad to the longest name that will actually print, not the longest name
    // in GROUPS — an entry naming a command that no longer exists must not
    // widen the column for everyone else.
    let width = listed.iter().map(|(n, _)| n.len()).max().unwrap_or(0);

    let mut out = String::new();
    let mut placed: BTreeSet<&str> = BTreeSet::new();

    for (heading, names) in GROUPS {
        let rows: Vec<&(&str, String)> = names
            .iter()
            .filter_map(|name| listed.iter().find(|(n, _)| n == name))
            .collect();
        if rows.is_empty() {
            continue;
        }
        push_section(&mut out, heading, &rows, width, header, literal);
        placed.extend(rows.iter().map(|(n, _)| *n));
    }

    // A command nobody assigned a group still has to be discoverable.
    let orphans: Vec<&(&str, String)> =
        listed.iter().filter(|(n, _)| !placed.contains(n)).collect();
    if !orphans.is_empty() {
        push_section(&mut out, "Other", &orphans, width, header, literal);
    }

    out.trim_end().to_string()
}

fn push_section(
    out: &mut String,
    heading: &str,
    rows: &[&(&str, String)],
    width: usize,
    header: &clap::builder::styling::Style,
    literal: &clap::builder::styling::Style,
) {
    use std::fmt::Write as _;

    let _ = writeln!(
        out,
        "{}{heading}:{}",
        header.render(),
        header.render_reset()
    );
    for (name, about) in rows {
        // Padding sits outside the styled span: a trailing run of *styled*
        // spaces is invisible but real, and clap's wrapper measures it.
        let _ = writeln!(
            out,
            "{:INDENT$}{}{name}{}{:pad$}{:GUTTER$}{about}",
            "",
            literal.render(),
            literal.render_reset(),
            "",
            "",
            pad = width - name.len()
        );
    }
    out.push('\n');
}

#[cfg(test)]
mod tests;
