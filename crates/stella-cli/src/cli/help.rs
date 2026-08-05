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
        &["run", "chat", "goal", "resume", "init", "daemon"],
    ),
    ("Run many at once", &["fleet", "monitor", "arena"]),
    (
        "Ask about this workspace",
        &["graph", "storage", "scripts", "tools", "commands"],
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
            "connect",
            "mcp",
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
Session flags work on either side of it: `stella run … --budget 5`.",
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
    let index = render_index(&cmd);
    let template = root_template(&cmd);
    cmd.help_template(template)
        .after_help(index.clone())
        .after_long_help(index)
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
