# stella-tui

The `ratatui` terminal UI: the single-session event-log REPL and the multi-tab
**Command Deck** it grew into — the default interactive surface when `stella`
runs on a TTY. 47 source files, ~36k lines.

The hard boundary is **no engine access**. This crate never calls a model, a
tool, or the store. `AgentEvent`s (single session) or `Inbound` envelopes
(deck) arrive over a channel; `UserInput` / `WorkspaceInput` flow back out.
Everything drawn is derived from that inbound stream, with a small set of
*labeled* exceptions listed in [`src/deck.rs`](src/deck.rs)'s module header.
It also does not depend on `stella-graph`: the caller queries its own
`CodeGraph` and hands the deck a plain [`GraphSnapshot`](src/graph.rs).

## Where it sits

Depends on `stella-protocol` (the `AgentEvent` / `Attachment` types it folds)
and `stella-tools` for exactly one thing — `subprocess_env::scrub_sensitive_env`
when running a `!` command ([`src/deck_shell.rs:208`](src/deck_shell.rs)).
Third-party: `ratatui` + `crossterm`, `sysinfo` (CPU/MEM), `arboard` + `png`
(clipboard images). `stella-cli` is the only workspace crate that depends on it — it owns
the driver side (`stella-cli/src/command_deck.rs`, `src/tui.rs`). This crate
builds no binary; the runnable surface is `examples/deck_demo.rs`. The driver
and the TUI communicate over two channels and never call each other directly.

## Layout

| File | What it holds |
|---|---|
| [`src/lib.rs`](src/lib.rs) | The authoritative design statement (pure core + thin shell) and the whole public re-export surface. Read it first. |
| [`src/model.rs`](src/model.rs) | `SessionModel` — one agent's fold. `apply` (line 319) is the *only* mutator; `replay` (line 645) rebuilds it from a log. |
| [`src/ui.rs`](src/ui.rs) | `UiState` (scroll, composer, focus — everything *not* derived from events) and the pure `handle_key` / `ingest`. |
| [`src/render.rs`](src/render.rs) | `render(model, ui, frame)` for the single-session REPL, plus `guarded_panel` — its entry to the panic boundary. |
| [`src/panel_guard.rs`](src/panel_guard.rs) | The panel panic boundary itself (L-T7), shared by the REPL, the deck and the fleet dashboard — and the written argument for what a caught panic can leave in `DeckUi`. |
| [`src/shell.rs`](src/shell.rs) | `run(...)`: terminal setup, crossterm loop, two channels, `RunOptions`, `DebugLog`. No decision logic. |
| [`src/term.rs`](src/term.rs) | `TerminalGuard` + `PanicHookGuard`. Open this before touching anything about raw mode or panics. |
| [`src/deck.rs`](src/deck.rs) | `WorkspaceModel` — N `SessionModel`s plus cross-agent read-models (file ledger, route log, prompt queue, unified trace). |
| [`src/deck_ui.rs`](src/deck_ui.rs) | `DeckUi` and `handle_deck_key` — every deck keybinding, and the documented Esc precedence list (line 1222). The largest file here. |
| [`src/deck_render.rs`](src/deck_render.rs) | `render_deck`: tab bar · active view · trace strip · progress bar · composer · footer · statline. |
| [`src/deck_shell.rs`](src/deck_shell.rs) | `run_deck(...)`: the deck's `select!` loop, plus a third arm — a 33 ms tick that advances the clock and samples resources. |
| [`src/envelope.rs`](src/envelope.rs) | The multi-agent wire types: `Inbound` in, `WorkspaceInput` out, and every out-of-band snapshot the driver pushes. |
| [`src/composer.rs`](src/composer.rs) | The input model: textarea semantics, `classify_enter`, paste chips, and the slash popup shared by both surfaces. |
| [`src/views/*.rs`](src/views) | One module per deck tab (session · agents · installed · traces · graph · files · skills · mcp · issues · settings), each exposing `render(model, ui, area, buf)`. `engine` is the exception: the config editor SETTINGS hosts, with its own `render_panel` and key handler. |
| [`src/diff.rs`](src/diff.rs), [`src/syntax.rs`](src/syntax.rs), [`src/markdown.rs`](src/markdown.rs), [`src/textline.rs`](src/textline.rs) | Shared text presentation — one implementation each of "how a diff looks", source coloring, markdown, and the event→wording table (also consumed by `stella-cli`'s plain renderer). |
| [`src/theme.rs`](src/theme.rs), [`src/palette.rs`](src/palette.rs) | Every color and glyph. `palette.rs` is **generated** from `docs/brand/tokens.json`; `theme.rs` is the only module allowed to reference it. |
| [`src/progress.rs`](src/progress.rs), [`src/cache_panel.rs`](src/cache_panel.rs), [`src/splash.rs`](src/splash.rs) | Chrome widgets: the run progress bar, the statline's cache cells, and the launch mark held over session init. |
| [`src/scroll.rs`](src/scroll.rs), [`src/input.rs`](src/input.rs), [`src/graph.rs`](src/graph.rs), [`src/resource.rs`](src/resource.rs), [`src/attach.rs`](src/attach.rs), [`src/clipboard.rs`](src/clipboard.rs) | Small leaf modules: line-exact viewport math, the outbound message enum, the graph snapshot types, CPU/MEM sampling, pasted-path detection, `⌃V` clipboard capture. |
| [`src/fleet_dashboard.rs`](src/fleet_dashboard.rs) | A separate full-screen surface for `stella fleet` — its own fold (`FleetMsg`), its own `run`, monotonic `Instant` clocks only. |
| [`src/scenario.rs`](src/scenario.rs) | A deterministic scripted multi-agent scenario, driving both `examples/deck_demo.rs` and the snapshot test. |

## Key concepts

**"Pure fold" is a code-level split, not a slogan.** Verify it in three places:
`SessionModel` has exactly one mutator, `apply` ([`src/model.rs:319`](src/model.rs)),
and `replay` ([`src/model.rs:645`](src/model.rs)) is just `apply` in a loop.
`render` ([`src/render.rs:46`](src/render.rs)) takes `&SessionModel` — the model
is immutable during a draw; the `&mut UiState` exists solely so panels can
record their viewport heights for the next keypress's scroll clamp. And
`handle_key` ([`src/ui.rs:245`](src/ui.rs)) returns a `ShellAction` rather than
performing anything. So every question a contributor has — "what does this key
do", "what does the screen show after these events" — is answerable by calling
a function with no terminal, no engine, and no tokio runtime.

**Why that matters here.** AGENTS.md's [witness-test contract](../AGENTS.md)
names TUI rendering as the case where a witness is "genuinely impractical" —
you cannot assert on a real terminal. The fold/render split is how this crate
gets one anyway: fold synthetic events into a model, render into a
`ratatui::backend::TestBackend`, and assert on the flattened **cell buffer**,
never on ANSI bytes. [`tests/progress_brand_fill.rs`](tests/progress_brand_fill.rs) is the
shape to copy — its header states the goal, why it failed before the change,
and what it asserts after. The determinism this rests on is itself
property-tested: `replaying_a_log_renders_identical_buffers`
([`src/render/tests.rs:1160`](src/render/tests.rs)) folds one event vector into
two fresh models and asserts byte-identical backing buffers.

**Two shells, one design.** [`shell::run`](src/shell.rs) drives one agent;
[`deck_shell::run_deck`](src/deck_shell.rs) drives N. Both are near-logic-free
`select!` loops over (inbound events, key events); the deck adds a third arm, a
33 ms tick, because gauges and elapsed timers must repaint on a clock rather
than only when the model streams. Anything you are tempted to write inside
either loop belongs in `ui.rs` / `deck_ui.rs` instead.

**Keys are a precedence chain, not a match arm.** `handle_deck_key`
([`src/deck_ui.rs:1251`](src/deck_ui.rs)) runs modal contexts first (splash,
help, installed-agent editors, issues forms, queue editor, graph picker, engine
panel, overlays), then tab navigation, then focused-agent gates, then composer
editing, then per-tab keys, then Esc. Two rules hold throughout: bare-letter
hotkeys (`s`/`p`/`r` on AGENTS, `?` for help) only fire from an **empty
composer**, so they never eat a keystroke meant for a prompt; and the composer
never claims Esc, so a typed draft survives an interrupt.

**Slash commands are an input, not a hard-coded set.** The vocabulary arrives
as `RunOptions::slash_commands` / `DeckOptions::slash_commands`, so the CLI owns
it; `SlashKind` only distinguishes built-in from user-authored rows by glyph.
`handle_slash_key`
([`src/deck_ui.rs:1634`](src/deck_ui.rs)) intercepts only the deck-local ones —
`/files`, `/diff`, `/graph`, `/agents`, `/skills`, `/mcp`, `/mcp-search`,
`/settings`, `/sessions`, `/context`, `/inspect`, `/inbox` — because they change
view state the driver has no say over. Everything else, `/help` included, is
enqueued as a prompt so the answer lands in the transcript.

**Terminal restoration survives panics in raw mode** — read
[`src/term.rs`](src/term.rs) before changing any of it. `TerminalGuard` is
constructed *before* the first state is acquired and flags each one (raw, alt
screen, bracketed paste, mouse, kitty) as it lands, so a failure partway through
`enter` still rolls back exactly what was entered. Release builds set
`panic = "abort"`, where destructors never run, so `PanicHookGuard::install`
([`src/term.rs:194`](src/term.rs)) additionally restores from inside the panic
hook — but **only** under `cfg!(panic = "abort")`, then delegating to the
previous hook so the panic message prints on the real screen, not the alternate
one. In unwind builds the hook deliberately does *not* restore: it fires for
every panic on every thread, including panel panics the session survives.

## Gotchas

- **The panic boundary covers every band, but only in unwind builds.**
  [`src/panel_guard.rs`](src/panel_guard.rs) wraps the single-session panels,
  every deck band (tab bar, active tab, trace strip, progress bar, composer,
  footer, statline, splash, each overlay) and the fleet dashboard, so a
  panicking view becomes an error card and the session continues. Release
  builds set `panic = "abort"`, where `catch_unwind` never runs its handler —
  so the shipped binary still dies on a view panic. A new draw surface is
  covered only if you route it through `guarded_band`/`guarded_overlay`, and a
  new `&mut DeckUi` write in a view has to be classified in that module's
  "what a caught panic can leave behind" list or the argument there stops
  being true.
- **Digits never switch tabs, deliberately.** They quick-pick `ask_user`
  answers and must stay typeable as a prompt's first character, so `Tab` /
  `Shift-Tab` are the only tab navigation
  ([`src/deck_ui.rs:1418`](src/deck_ui.rs)). Adding a digit hotkey would eat a
  keystroke meant for the composer.
- Mouse capture is off by default in both `RunOptions` and `DeckOptions` so
  native terminal selection and copy keep working. The deck's event loop
  discards mouse events entirely today — turning the flag on currently costs
  selection and buys nothing.
- Bracketed paste is always enabled. Without it a multi-line paste arrives as
  raw key events and every newline acts as Enter, turning one paste into N
  submissions ([`src/term.rs:101`](src/term.rs)).
- `⌃G`, not `⌃I`, opens the INSPECT overlay: without the kitty keyboard
  protocol (pushed only best-effort) `⌃I` is byte-identical to Tab, which is
  bound to tab switching. The collision would be silent and terminal-dependent.
- Prefer stillness. A terminal has no `prefers-reduced-motion`, so anything
  that animates without carrying information is a bug: the launch cinematic,
  the tab-switch sweep, the caret blink, and the pulsing STAGE dot were all
  deleted for that reason. What motion remains (the progress shimmer, the
  in-flight spinners) reports live work and is gated on `no_anim`.
- What does animate must be *scrubbed*, not stateful: `splash` renders as a
  pure function of elapsed time, so two frames at the same `t` are byte-identical
  and a dropped frame lands where continuous playback would have.
- Do not add a color literal to a view. `theme.rs` is the only module that may
  read `palette.rs`, and `palette.rs` is generated — edit `docs/brand/tokens.json`.
- Anything you put on `WorkspaceModel` that is not folded from `Inbound` erodes
  the purity boundary. The current exceptions are enumerated by name in
  [`src/deck.rs`](src/deck.rs)'s header; extend that list deliberately or not
  at all.

## Testing

```bash
cargo test -p stella-tui                       # no make target exists for this crate
cargo test -p stella-tui -- --ignored          # the TTY smoke test, on a real terminal
cargo run -p stella-tui --example deck_demo    # interactive scripted deck
```

Unit tests live in `#[cfg(test)]` modules inside the source files (37 of the 47
have one); four modules grew large enough to split theirs into a sibling
`tests.rs` submodule (`src/render/`, `src/deck/`, `src/deck_render/`,
`src/fleet_dashboard/`).
Two integration tests sit in [`tests/`](tests): `deck_snapshot.rs` renders every
tab through the real `render_deck` and writes a human-readable text "screenshot"
to `CARGO_TARGET_TMPDIR` (deliberately outside the source tree, so a test run
never dirties the working tree), and `progress_brand_fill.rs` is a single-assertion
witness. `proptest` covers the two determinism-critical modules,
`src/scroll.rs` and `src/render/`. No fixtures, env vars, or feature flags are
needed. The one uncovered path is `shell::run` itself: its smoke test is
`#[ignore]`d because it needs a real TTY.

## Extending it

Adding a deck tab:

1. Add the variant to `DeckTab` **and** to `DeckTab::ALL` in
   [`src/deck.rs`](src/deck.rs). `ALL` is a fixed-length array driving `index`
   / `next` / `prev`, so forgetting it silently drops the tab from Tab-cycling;
   the exhaustive `title()` match is what the compiler catches. Labels are
   UPPERCASE by convention.
2. Write `src/views/<tab>.rs` exposing
   `render(model: &WorkspaceModel, ui: &mut DeckUi, area: Rect, buf: &mut Buffer)`,
   pulling every color from `theme` and recording viewport metrics onto
   `ui.metrics`. Register it in [`src/views/mod.rs`](src/views/mod.rs).
3. Add the match arm in `render_deck` ([`src/deck_render.rs`](src/deck_render.rs)).
4. Add a `handle_<tab>_key` in [`src/deck_ui.rs`](src/deck_ui.rs) and wire it
   into the per-tab dispatch, taking `composer_empty` and honoring the
   empty-composer rule for bare-letter hotkeys.
5. `deck_renders_every_tab_with_real_content` in
   [`tests/deck_snapshot.rs`](tests/deck_snapshot.rs) iterates the tabs — extend
   it with the content your tab must show.

Adding an event the deck reacts to: add the variant to `Inbound`
([`src/envelope.rs`](src/envelope.rs)), fold it in
`WorkspaceModel::apply_inbound` (or, if it is a driver-pushed snapshot rather
than a fold, handle it in `ingest_inbound` and add it to the `deck.rs`
exceptions list), then cover it in `src/deck/tests.rs`.

## See also

- [`../AGENTS.md`](../AGENTS.md) — "Workspace layout" for the routing rule, and
  "The definition of done: witness tests" for why this crate is named as the
  hard case.
- [`../stella-protocol`](../stella-protocol) — `AgentEvent`, the type every fold
  in here consumes.
- [`../stella-cli`](../stella-cli) — the driver: `src/command_deck.rs` and
  `src/tui.rs` own the slash vocabulary, the on-disk snapshots, and dispatch.
- [`../website/content/docs/agent-modes.mdx`](../website/content/docs/agent-modes.mdx)
  — the Command Deck from a user's point of view.
- [`../website/content/docs/agent-tools/commands.mdx`](../website/content/docs/agent-tools/commands.mdx)
  — how a user extends the slash menu this crate renders.
