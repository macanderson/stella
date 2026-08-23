# Handoff: autofix PR watcher (in progress)

Branch: `feat/issues-send-to-prompt-confirm` (also carries shipped PR #4377's commit).
State: uncommitted changes in the working tree. Task board: #4 in progress, #5/#6 pending.

## Done

1. **`autofix_prs` setting — fully wired through all completeness gates** (default: ON):
   - `crates/stella-cli/src/settings.rs` — `pub autofix_prs: Option<Toggle>` field + `pub fn autofix_prs()` accessor (`is_none_or(Toggle::is_on)`)
   - `crates/stella-cli/src/settings/merge.rs` — last-wins merge arm
   - `crates/stella-cli/src/settings/completeness.rs` — destructure + `keyed("autofix_prs", Posture::Merged, …)`
   - `crates/stella-cli/src/settings/unknown.rs` — `ROOT_FIELDS` + `RUN_FIELDS` entries
   - `crates/stella-cli/src/settings/toml_config.rs` — `[run] autofix_prs` field + lowering into `Settings`

2. **TUI types:**
   - `crates/stella-tui/src/envelope.rs` — new `AutofixStatus { pr_number, pr_url, checks_done, checks_total, phase }`; `SessionInfo.autofix: Option<AutofixStatus>`
   - Constructors updated: `command_deck/sessions_view.rs`, `deck_ui/sessions.rs`, `v2/sessions.rs`

## Remaining (in order)

1. Add `autofix: None` to the two `SessionInfo` constructors in `crates/stella-tui/src/deck_ui/tests/sessions.rs` (lines ~8 and ~149) — then `cargo check --workspace`.
2. **Watcher**: new `crates/stella-cli/src/command_deck/autofix.rs` — tokio task spawned beside `spawn_pr_monitor` (command_deck.rs ~line 858), gated on `settings.autofix_prs()`. On CI failure/conflict → `subsession::spawn` a fix worker (lane `autofix:<pr>`); on green+mergeable → `gh pr merge`. Extend `observe_pr` in `pr_observe.rs` to return checks done/total. Must not block the driver — same spawn pattern as the PR monitor.
3. **Sessions overlay** (`crates/stella-tui/src/v2/sessions.rs`): render autofix rows (PR #, `done/total` checks, phase), `o` open-in-browser (copy `open_in_browser` from `deck_ui/issues_keys.rs`), footer update. Left-arrow-in-empty-prompt already opens this overlay (deck_ui.rs ~1921) — no key work needed.
4. **Tests** (each a fail→pass witness, per AGENTS.md): setting default-on + merge; watcher decision fold (pure, no `gh`); overlay autofix row rendering; `o` key.
5. Commit + PR. Note: push may need `--no-verify` (repo hooks run full tests and time out).

## Key landmarks

- `spawn_pr_monitor` — crates/stella-cli/src/command_deck.rs:2959 (pattern to copy)
- `subsession::spawn` — crates/stella-cli/src/subsession.rs:583; `SubSessions::started` :128
- `observe_pr` / `aggregate_ci` — crates/stella-cli/src/command_deck/pr_observe.rs
- Sessions overlay keys — crates/stella-tui/src/deck_ui/sessions.rs; render — crates/stella-tui/src/v2/sessions.rs:38
- Settings gates: every new Settings field must appear in merge.rs, completeness.rs, unknown.rs, toml_config.rs or the crate fails to compile / gates fail.
