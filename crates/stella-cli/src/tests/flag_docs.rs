//! The flag-to-documentation lock (#3041).
//!
//! `scripts/check-command-docs.sh` relates a *subcommand* to its reference
//! page and `the_docs_index_summaries_are_the_clis_own` relates a command's
//! one-liner to its `about`. Nothing related a **flag** to anything, and the
//! predictable happened: a 2026-08-12 audit found the "Global flags" grid
//! listing 7 of 17 session flags, seven of which appeared nowhere on the site;
//! `--upstream-pin` was undocumented within two days of a docs-touching
//! commit. PR #3065 fixed the cards by hand and the mechanism was left alone,
//! so it recurred — `--allow-dir` shipped global in #3502 and had no card.
//!
//! # Why the clap tree rather than the source
//!
//! The list is read off `Cli::command()`, the same `Command` tree `--help`
//! renders, so it cannot disagree with what ships. A source parse would have
//! to re-derive clap's own naming — the kebab-casing, `short`, `alias`, and
//! whether a multi-line `#[arg(…)]` block carried `global = true` — and every
//! one of those is a way for the guard to be wrong in the direction that
//! reads as green.
//!
//! # The two directions read different things, on purpose
//!
//! **Forward** — does this flag reach a reader — is answered over the whole
//! page. A command page documents a flag in an `<OptionCard>`, in its
//! synopsis fence, or in the prose of the section that explains it, and all
//! three land in front of somebody looking for it. Requiring the card
//! specifically is a different and stricter rule than the flag surface is
//! written to today: it would report 81 flags across 37 pages, and 81
//! exemptions is the expedient this repository refuses.
//!
//! The exception is `index.mdx`'s session-wide grid, which is a grid: a
//! global flag missing from it is undocumented whatever the page says about
//! it in passing, and the grid is small enough to hold to that.
//!
//! **Reverse** — does this page describe a flag that no longer exists — is
//! answered over `<OptionCard>` alone. A page names flags it does not own all
//! the time: cross-references to sibling commands, shell examples, prose
//! about a settings key spelled like one. Over that text the reverse
//! direction would be noise. A card is the structural claim "this page
//! documents this flag", and that is the claim worth holding to a real
//! command.

use std::collections::{BTreeMap, BTreeSet};

use clap::CommandFactory;

use super::{COMMANDS_DOCS_DIR, Cli, repo_root};

/// Flags deliberately undocumented, one `command:flag` per line with the
/// reason as a `#` comment. `command` is the top-level command's slug, or
/// `_global` for a session-wide flag.
const EXEMPT_FILE: &str = "scripts/flag-docs-exempt.txt";

/// The pseudo-command a session-wide flag is filed under, since its card
/// lives on the section's index page rather than on any one command's.
const GLOBAL: &str = "_global";

/// `command:flag` pairs the guard must not report, parsed the way
/// `scripts/check-command-docs.sh` parses its own exemption file: `#` opens a
/// comment, blank lines are skipped.
fn exemptions() -> BTreeSet<String> {
    let path = repo_root().join(EXEMPT_FILE);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    raw.lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Every `--long` name a `<OptionCard name="…">` on `page` claims to
/// document.
///
/// One card may name several — `-v / --verbose` is one card for one flag, and
/// a card is free to name a flag's alias beside it — so every `--token` in
/// the `name` attribute counts as documented. The value placeholder
/// (`--model <provider/model_id>`) is not a flag and stops at the space.
fn carded_flags(page: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for card in page.split("<OptionCard").skip(1) {
        let Some(rest) = card.split_once("name=\"") else {
            continue;
        };
        let Some((name, _)) = rest.1.split_once('"') else {
            continue;
        };
        for token in name.split_whitespace() {
            if let Some(flag) = token.strip_prefix("--")
                && !flag.is_empty()
            {
                found.insert(flag.trim_end_matches(',').to_owned());
            }
        }
    }
    found
}

/// Whether `page` names `--flag` anywhere, as a whole flag.
///
/// The boundary check is what stops `--list` being satisfied by a page that
/// only ever mentions `--list-tools`: a bare `contains` would report the
/// narrower flag as documented by the wider one's name.
fn names_flag(page: &str, flag: &str) -> bool {
    let needle = format!("--{flag}");
    page.match_indices(&needle).any(|(at, _)| {
        page[at + needle.len()..]
            .chars()
            .next()
            .is_none_or(|next| !next.is_ascii_alphanumeric() && next != '-')
    })
}

/// The long names of `cmd`'s own arguments, split by whether clap propagates
/// them to every subcommand.
///
/// `help`/`version` are clap's own and hidden args are hidden from `--help`
/// too, so neither owes the docs a card — the same exclusion
/// `every_subcommand_has_a_reference_page` makes for hidden commands.
fn long_flags(cmd: &clap::Command, global: bool) -> BTreeSet<String> {
    cmd.get_arguments()
        .filter(|arg| arg.is_global_set() == global)
        .filter(|arg| !arg.is_hide_set())
        .filter_map(clap::Arg::get_long)
        .filter(|long| *long != "help" && *long != "version")
        .map(str::to_owned)
        .collect()
}

/// Every non-global long flag reachable under `cmd`, including its
/// subcommands' — nested commands have no page of their own, so their flags
/// are documented on the top-level command's page.
fn own_flags_recursive(cmd: &clap::Command) -> BTreeSet<String> {
    let mut flags = long_flags(cmd, false);
    for child in cmd.get_subcommands().filter(|c| !c.is_hide_set()) {
        if child.get_name() == "help" {
            continue;
        }
        flags.extend(own_flags_recursive(child));
    }
    flags
}

/// The top-level commands that have a page, by slug, with every long flag
/// reachable under each.
fn command_flags() -> BTreeMap<String, BTreeSet<String>> {
    Cli::command()
        .get_subcommands()
        .filter(|sub| !sub.is_hide_set())
        .filter(|sub| sub.get_name() != "help")
        .map(|sub| (sub.get_name().to_owned(), own_flags_recursive(sub)))
        .collect()
}

/// The long names of `cmd`'s global, non-hidden arguments, in the order clap
/// declared them — the same order `--help` renders them in.
fn ordered_global_flags(cmd: &clap::Command) -> Vec<String> {
    cmd.get_arguments()
        .filter(|arg| arg.is_global_set())
        .filter(|arg| !arg.is_hide_set())
        .filter_map(clap::Arg::get_long)
        .filter(|long| *long != "help" && *long != "version")
        .map(str::to_owned)
        .collect()
}

/// The primary (first-named) `--long` flag of every `<OptionCard>` on `page`
/// that documents a flag, in the order the cards appear on the page.
///
/// A card naming an alias beside its flag (`-v / --verbose`) counts by its
/// first `--token`, matching what a reader scans top to bottom.
fn ordered_carded_flags(page: &str) -> Vec<String> {
    let mut found = Vec::new();
    for card in page.split("<OptionCard").skip(1) {
        let Some(rest) = card.split_once("name=\"") else {
            continue;
        };
        let Some((name, _)) = rest.1.split_once('"') else {
            continue;
        };
        if let Some(flag) = name
            .split_whitespace()
            .find_map(|token| token.strip_prefix("--"))
        {
            found.push(flag.trim_end_matches(',').to_owned());
        }
    }
    found
}

fn page_source(slug: &str) -> Option<String> {
    std::fs::read_to_string(
        repo_root()
            .join(COMMANDS_DOCS_DIR)
            .join(format!("{slug}.mdx")),
    )
    .ok()
}

/// Every session-wide flag has a card on the commands index.
///
/// The forward direction, and the one that was red when this was written:
/// `--allow-dir` had been `global = true` since #3502 and appeared nowhere
/// under `website/content/docs/`.
#[test]
fn every_global_flag_has_a_card_on_the_index() {
    let exempt = exemptions();
    let index = page_source("index").expect("the commands index must exist");
    let carded = carded_flags(&index);

    let missing: Vec<String> = long_flags(&Cli::command(), true)
        .into_iter()
        .filter(|flag| !carded.contains(flag))
        .filter(|flag| !exempt.contains(&format!("{GLOBAL}:{flag}")))
        .collect();

    assert!(
        missing.is_empty(),
        "these session-wide flags have no `<OptionCard>` in \
         {COMMANDS_DOCS_DIR}/index.mdx: {missing:?}\n\
         add a card under `## Global flags`, or exempt it in {EXEMPT_FILE} as \
         `{GLOBAL}:<flag>  # reason`"
    );
}

/// The global-flag cards on the commands index appear in the same order
/// `stella --help` lists them in.
///
/// The index page claims this order explicitly ("The cards below are in
/// `stella --help`'s own order") — a claim #4508 found false: the grid had
/// drifted to `--no-anim, --plan-mode, --tools, -v, --log-level, --log-file`
/// against `GlobalArgs`' declared `--no-anim, -v, --log-level, --plan-mode,
/// --tools, --log-file`. Cheap to keep true once asserted, because
/// `Cli::command()` is the same tree `--help` renders — see the module
/// doc's "why the clap tree rather than the source".
#[test]
fn global_flag_cards_are_in_help_order() {
    let exempt = exemptions();
    let index = page_source("index").expect("the commands index must exist");

    let expected: Vec<String> = ordered_global_flags(&Cli::command())
        .into_iter()
        .filter(|flag| !exempt.contains(&format!("{GLOBAL}:{flag}")))
        .collect();
    let global_set: BTreeSet<String> = expected.iter().cloned().collect();
    let actual: Vec<String> = ordered_carded_flags(&index)
        .into_iter()
        .filter(|flag| global_set.contains(flag))
        .collect();

    assert_eq!(
        expected, actual,
        "the `## Global flags` cards in {COMMANDS_DOCS_DIR}/index.mdx are not \
         in `stella --help`'s order (GlobalArgs' declaration order in \
         crates/stella-cli/src/cli.rs) — reorder the cards to match"
    );
}

/// Every flag a command accepts is named on that command's page.
///
/// Nested subcommands do not get their own page, so a child's flags are
/// documented on the parent's — the same rule
/// `every_page_documents_all_of_its_commands_subcommands` applies to the
/// child commands themselves.
#[test]
fn every_command_flag_is_named_on_its_page() {
    let exempt = exemptions();
    let mut missing: Vec<String> = Vec::new();

    for (slug, flags) in command_flags() {
        // "this page is missing entirely" belongs to
        // `every_subcommand_has_a_reference_page`; reporting it here too
        // would make one omission look like two problems.
        let Some(page) = page_source(&slug) else {
            continue;
        };
        for flag in flags {
            if !names_flag(&page, &flag) && !exempt.contains(&format!("{slug}:{flag}")) {
                missing.push(format!("{slug}:--{flag}"));
            }
        }
    }
    missing.sort();

    assert!(
        missing.is_empty(),
        "these flags are named nowhere on their command's page: {missing:#?}\n\
         document each in {COMMANDS_DOCS_DIR}/<command>.mdx, or exempt it in \
         {EXEMPT_FILE} as `<command>:<flag>  # reason`"
    );
}

/// No page carries a card for a flag its command does not have.
///
/// The reverse direction, and the one that reads as current and is wrong: a
/// card outliving the flag it documents is worse than a missing card, because
/// a reader has no way to tell it from a live one. A page may card a global
/// flag it wants to say something command-specific about, so globals count as
/// present everywhere.
#[test]
fn no_page_cards_a_flag_its_command_does_not_have() {
    let globals = long_flags(&Cli::command(), true);
    let mut orphans: Vec<String> = Vec::new();

    for (slug, flags) in command_flags() {
        let Some(page) = page_source(&slug) else {
            continue;
        };
        for carded in carded_flags(&page) {
            if !flags.contains(&carded) && !globals.contains(&carded) {
                orphans.push(format!("{slug}:--{carded}"));
            }
        }
    }
    orphans.sort();

    assert!(
        orphans.is_empty(),
        "these pages card a flag their command does not accept: {orphans:#?}\n\
         remove the card, or fix its name — a card for a flag that no longer \
         exists reads exactly like one for a flag that does"
    );
}

/// An exemption for a flag that no longer exists is a blanket permission for
/// whatever name a future flag reuses, so the file cannot rot.
///
/// Same rule `scripts/check-command-docs.sh` applies to its own exemptions
/// (its check 4), for the same reason.
#[test]
fn no_exemption_names_a_flag_that_does_not_exist() {
    let by_command = command_flags();
    let globals = long_flags(&Cli::command(), true);

    let stale: Vec<String> = exemptions()
        .into_iter()
        .filter(|entry| {
            let Some((command, flag)) = entry.split_once(':') else {
                return true;
            };
            let Some(flag) = flag.strip_prefix("--").or(Some(flag)) else {
                return true;
            };
            if command == GLOBAL {
                return !globals.contains(flag);
            }
            by_command
                .get(command)
                .is_none_or(|flags| !flags.contains(flag))
        })
        .collect();

    assert!(
        stale.is_empty(),
        "these {EXEMPT_FILE} entries name no real flag: {stale:#?}\n\
         each line is `<command>:<flag>` where `<command>` is a top-level \
         command slug or `{GLOBAL}`; drop the line when the flag goes"
    );
}
