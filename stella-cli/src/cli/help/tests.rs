//! What a maintainer can break here is the help page, which no other test
//! looks at. Each test below pins one property the grouped layout depends on.

use super::*;

/// The terminal this layout is designed to survive. Nothing here asserts a
/// *nice* result on narrower screens — clap wraps and the columns give — only
/// that the standard width is intact.
const NARROW: usize = 80;

fn short_help() -> String {
    command().term_width(NARROW).render_help().to_string()
}

fn long_help() -> String {
    command().term_width(NARROW).render_long_help().to_string()
}

/// Every command a user can type is reachable from the index, and every name
/// in the index is a command. The grouping is hand-maintained on purpose (a
/// command's *purpose* cannot be inferred from its type), so this is the test
/// that makes adding a command force the editorial decision instead of
/// silently dropping it into `Other`.
#[test]
fn every_command_is_grouped_exactly_once() {
    let cmd = command();
    let real: Vec<&str> = cmd
        .get_subcommands()
        .filter(|c| !c.is_hide_set() && c.get_name() != NOT_A_COMMAND)
        .map(|c| c.get_name())
        .collect();
    assert!(!real.is_empty(), "expected subcommands to introspect");

    for name in &real {
        let groups: Vec<&str> = GROUPS
            .iter()
            .filter(|(_, names)| names.contains(name))
            .map(|(heading, _)| *heading)
            .collect();
        assert_eq!(
            groups.len(),
            1,
            "`stella {name}` belongs to {} groups in cli/help.rs GROUPS, want exactly 1 \
             (found {groups:?}). A new command needs a heading picked for it.",
            groups.len()
        );
    }

    for (heading, names) in GROUPS {
        for name in *names {
            assert!(
                real.contains(name),
                "GROUPS lists `{name}` under {heading:?}, but no such subcommand exists — \
                 it was renamed or removed without updating the index."
            );
        }
    }
}

/// The index is two columns, and clap wraps `after_help` at the terminal
/// width with no hanging indent — so a summary past the budget does not
/// gracefully indent, it drops to column zero and the table stops being a
/// table. Cap the summaries instead.
#[test]
fn about_lines_fit_the_command_index() {
    let cmd = command();
    let widest = cmd
        .get_subcommands()
        .filter(|c| !c.is_hide_set() && c.get_name() != NOT_A_COMMAND)
        .map(|c| c.get_name().len())
        .max()
        .unwrap_or(0);

    for sub in cmd.get_subcommands() {
        let name = sub.get_name();
        if sub.is_hide_set() || name == NOT_A_COMMAND {
            continue;
        }
        let about = sub
            .get_about()
            .map(|a| a.to_string())
            .unwrap_or_else(|| panic!("`stella {name}` has no about; it would list blank"));

        assert!(
            !about.contains('\n'),
            "`stella {name}`'s about spans lines. clap takes everything before the first \
             blank `///` line as the summary — add the blank line."
        );
        // What is left of the line once the row's fixed furniture is paid for.
        let budget = NARROW - INDENT - widest - GUTTER;
        let width = about.chars().count();
        assert!(
            width <= budget,
            "`stella {name}`'s about is {width} chars, over the {budget} the index column \
             allows ({NARROW} columns, less the {INDENT}-space indent, the {widest}-char \
             name column, and the {GUTTER}-space gutter). Shorten the first line of its \
             doc comment; the detail belongs after the blank `///` line, where \
             `stella help {name}` shows it."
        );
    }
}

/// Same rule, one level up: the flags rendered by `-h` are one line each.
#[test]
fn session_flag_summaries_stay_one_line() {
    for arg in command().get_arguments() {
        let Some(help) = arg.get_help() else { continue };
        let help = help.to_string();
        assert!(
            !help.contains('\n'),
            "`--{}`'s short help spans lines; move the detail after a blank `///` line \
             so it lands in `long_help` instead.",
            arg.get_id()
        );
    }
}

/// `stella --help` used to open with fifteen lines of clap implementation
/// notes — `GlobalArgs`' doc comment, which clap derive lifts into the
/// *parent's* `long_about` when the parent sets none. The notes are still in
/// the source, as `//` comments; this asserts they are not in the help.
#[test]
fn the_long_help_opens_with_the_product_not_the_source() {
    let help = long_help();
    for leak in [
        "main_tests.rs",
        "global = true",
        "every_root_flag_is_global",
        "no_subcommand_flag_reuses_a_global_name",
    ] {
        assert!(
            !help.contains(leak),
            "`stella --help` contains {leak:?} — a maintainer note is being rendered to \
             users. Check that `Cli` still sets `long_about` explicitly."
        );
    }
    assert!(
        help.starts_with("A fast, BYOK, model-agnostic terminal coding agent"),
        "long help should open with what stella is; got:\n{}",
        help.lines().take(3).collect::<Vec<_>>().join("\n")
    );
}

/// The reported bug, stated as a property: with thirty-odd commands no help
/// page fits a 24-line terminal, so the thing that matters is not total
/// length but *what is above the fold*. It used to be the tail of the global
/// flag list. It must be the commands a session actually starts with.
#[test]
fn the_first_screen_shows_the_commands_people_type() {
    let help = short_help();
    let fold: String = help.lines().take(24).collect::<Vec<_>>().join("\n");

    for name in ["run", "chat", "goal", "resume", "init"] {
        assert!(
            fold.lines().any(|l| l.starts_with(&format!("  {name} "))),
            "`stella {name}` fell below the first 24 lines of `-h`:\n{fold}"
        );
    }
    assert!(
        !fold.contains("--budget") && !fold.contains("--model"),
        "session flags are back above the fold, which is the bug this layout fixed:\n{fold}"
    );
}

/// Grouping must not cost discoverability: every command still appears, and
/// under a heading.
#[test]
fn the_short_help_lists_every_command_under_a_heading() {
    let help = short_help();
    let cmd = command();
    for sub in cmd.get_subcommands() {
        let name = sub.get_name();
        if sub.is_hide_set() || name == NOT_A_COMMAND {
            continue;
        }
        assert!(
            help.lines().any(
                |l| l.starts_with(&format!("  {name} ")) || l.trim_end() == format!("  {name}")
            ),
            "`stella {name}` is missing from the `-h` index:\n{help}"
        );
    }
    for (heading, _) in GROUPS {
        assert!(
            help.contains(&format!("{heading}:")),
            "heading {heading:?} is missing from `-h`:\n{help}"
        );
    }
}

/// The whole point of the two columns is that they line up: at the design
/// width every command row keeps its summary on the same line as its name,
/// and nothing this module emits overflows.
///
/// Scoped to the index and the footer — the parts rendered here. clap's own
/// argument section is not held to the same bar because clap appends the
/// `[env: …]` / `[possible values: …]` suffixes *after* wrapping, so an
/// arg line can exceed the width no matter what this file does.
#[test]
fn the_index_holds_its_shape_at_eighty_columns() {
    let help = short_help();
    let ours = help
        .lines()
        .take_while(|l| !l.starts_with("Session flags:"))
        .chain(help.lines().filter(|l| l.starts_with("Run `stella help")));
    for line in ours {
        let width = line.chars().count();
        assert!(
            width <= NARROW,
            "line overflows {NARROW} columns at {width}: {line:?}"
        );
    }

    let cmd = command();
    for sub in cmd.get_subcommands() {
        let name = sub.get_name();
        if sub.is_hide_set() || name == NOT_A_COMMAND {
            continue;
        }
        let row = help
            .lines()
            .find(|l| l.starts_with(&format!("  {name} ")))
            .unwrap_or_else(|| panic!("no index row for `{name}`:\n{help}"));
        let about = sub.get_about().map(|a| a.to_string()).unwrap_or_default();
        assert!(
            row.ends_with(&about),
            "`{name}`'s summary wrapped off its row — the column budget is wrong.\n\
             row:   {row:?}\nabout: {about:?}"
        );
    }
}

/// Replacing clap's `{subcommands}` section changes what is *rendered*, never
/// what exists: completion scripts are generated from the same tree and must
/// still know every command. A future `hide = true` used to shorten the help
/// would silently break tab-completion, and this is where that shows up.
#[test]
fn grouping_the_help_did_not_shrink_the_completions() {
    let mut cmd = command();
    let names: Vec<String> = cmd
        .get_subcommands()
        .map(|c| c.get_name().to_string())
        .collect();

    let mut script = Vec::new();
    clap_complete::generate(clap_complete::Shell::Zsh, &mut cmd, "stella", &mut script);
    let script = String::from_utf8(script).expect("completion scripts are utf-8");

    for name in names {
        assert!(
            script.contains(&name),
            "`{name}` is missing from the generated zsh completions"
        );
    }
}

/// The custom template is installed on the root only. Every subcommand page
/// must keep clap's stock layout — including its own `Commands:` list, which
/// the root no longer has.
#[test]
fn subcommand_help_keeps_claps_own_layout() {
    let mut cmd = command();
    let fleet = cmd
        .find_subcommand_mut("mcp")
        .expect("`mcp` has subcommands of its own");
    let help = fleet.render_help().to_string();
    assert!(
        help.contains("Commands:"),
        "`stella mcp --help` lost clap's subcommand list — is `help_template` propagating?\n{help}"
    );
    assert!(
        !help.contains("Run `stella help <command>`"),
        "the root footer leaked into a subcommand page:\n{help}"
    );
}
