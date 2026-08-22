# stella-tui

The `ratatui` terminal UI: the single-session event-log REPL and the multi-tab
**Command Deck** it grew into — the default interactive surface when `stella`
runs on a TTY.

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
the driver side (`crates/stella-cli/src/command_deck.rs`, `src/tui.rs`). This crate
builds no binary; the runnable surface is `examples/deck_demo.rs`. The driver
and the TUI communicate over two channels and never call each other directly.

## Direction — render what the loop says, including its ending

Stella is becoming one turn loop with a plugin architecture around it, embeddable
in any application (`doc:turn-loop-wrappers`, `doc:engine-embedding`). This crate
is the reference *consumer* of that loop's event stream, and two changes in flight
land squarely on the fold:

- **One completion per turn, plus one per run.** Since #3379 the engine emits its
  own terminal completion for every turn and no wrapper filters it; a wrapper's
  run-level ending is a separately named signal. A fold that treats the first
  completion it sees as "the work is over" is wrong for any multi-round run —
  which is most of them.
- **Stages are a wrapper's vocabulary, not the engine's.** The staged panels
  render stage events emitted by whatever wraps the turn. That was
  `crates/stella-pipeline`, which left the workspace to become an installable
  plugin (#3246) and was deleted in #3865. The deck must stay legible
  for a run with **no** wrapper at all — the raw loop is the default
  (#3381) — so a stage-shaped view has to degrade to "the loop ran and here is
  what it changed" rather than look broken.

The boundary is unchanged and is what makes this safe: everything drawn is derived
from the inbound stream, so a new wrapper's events are new rows to fold, never a
new dependency.

## Boundary — does this change belong here?

This crate owns what a terminal shows and what a keypress means, and nothing
upstream of that.

**One exception is worth stating, because it points the other way:** a policy
that *every* transcript surface must answer identically — how much of a tool
result a collapsed fold shows, how a JSON body is lexed for colouring — lives
in [`stella-transcript`](../stella-transcript/README.md), which this crate
depends on. Both of those started here, and both meant the Command Deck and an
exported transcript rendered the same run differently (#3644). The paint is
still this crate's: `theme::SYNTAX_*` and the `Style` a token class resolves to
need ratatui and stay here. The *decision* does not. If your change is a number
or a rule the Observatory would also have to know, it belongs downstairs. The decision rule: if your change can be written as a pure
fold — a function from the inbound `AgentEvent` / `Inbound` stream (plus
`DeckUi` interaction state) to a `ratatui` buffer or an outbound
`WorkspaceInput` — it belongs here. Views, keybindings, cards, overlays,
themes, scroll math, the composer: all of these are answerable by "given these
events, what do the cells say?", which is exactly the property the buffer
tests rely on.

What must never land here is anything the fold cannot derive. Business
decisions — what to compact, when a budget trips, whether a loop is detected —
live in [`stella-core`](../stella-core); this crate renders the *events* those
decisions emit and never re-decides them. Tool logic belongs in
[`stella-tools`](../stella-tools) (the one existing exception, env scrubbing
for `!` commands, is noted above). Provider calls go through
[`stella-model`](../stella-model), persistence through
[`stella-store`](../stella-store). AGENTS.md invariants #1 and #2 are the
normative statement of this split; the "Workspace layout" table there routes
"REPL rendering / panels / keybindings" here and nothing else.

The subtler boundary is with the driver,
`crates/stella-cli/src/command_deck.rs`. It owns the slash vocabulary,
prompt dispatch, and on-disk snapshots; the deck asks by sending
`WorkspaceInput` and learns outcomes only from what folds back in. If your
change needs the deck to *know* something no `Inbound` carries, the fix is a
new envelope variant in [`src/envelope.rs`](src/envelope.rs) fed by the
driver — never a back-channel or a field on `WorkspaceModel` that the fold
did not produce (the short labeled exception list in
[`src/deck.rs`](src/deck.rs)'s header is the only sanctioned escape).

Do not reach for a new crate. A new tab, card, overlay, or widget is a module
under [`src/views/`](src/views), not a crate — this crate already absorbs its
heavy presentation dependencies (`ratatui`, `arboard`, `png`) and one more
view costs nothing structurally. A new crate is justified only when
functionality (a) sits behind a port/trait and would otherwise drag new heavy
dependencies into a crate that is deliberately light, (b) needs a dependency
direction the current graph forbids — the existing example is `stella-graph`,
which this crate must not link, so the caller hands over a plain
`GraphSnapshot` — or (c) is a genuinely separate deliverable with its own
binary and release cadence. Otherwise extend this crate: a new crate costs a
workspace-table row, an impacted-crates scope, CI time, and a README, and a
wrong split is harder to undo than a wrong merge. If you do add one, the same
PR must update AGENTS.md's workspace table and the root `Cargo.toml` members.

## God files — do not add lines

The gate's `file-size` guard (`scripts/check-file-size.sh`) enforces a
1500-line ratchet: a new file over the limit is a hard failure with no
baseline escape, and the two files below are grandfathered at a recorded
ceiling in `scripts/file-size-baseline.txt`. They are god files — already too
big, closed to growth — and this crate hosts the workspace's single worst:
the guard's own header cites [`src/deck_ui.rs`](src/deck_ui.rs), which had
reached 6,884 lines when the guard landed and has since been cut to its
recorded ceiling in `scripts/file-size-baseline.txt`. Plan changes so no new line lands in any of them. The
crate's own precedent is the [`src/views/`](src/views) directory — one render
module per tab, split out of the deck files — so a change that adds rendering
goes in a `views/` module, not `deck_ui.rs`; new modal
key handling goes in a `src/deck_ui/` submodule the way
[`src/deck_ui/cards.rs`](src/deck_ui/cards.rs) already does. Note that
[`src/views/engine.rs`](src/views/engine.rs) is itself grandfathered: a split
buys headroom, not immunity, so code you touch inside any of these files is a
candidate to extract.

- [`src/deck_ui.rs`](src/deck_ui.rs)
- [`src/views/engine.rs`](src/views/engine.rs)

Two files have left this list, both the same way. `src/views/session.rs` left in
#4127: its incremental transcript fold moved to
[`src/views/session/fold.rs`](src/views/session/fold.rs), which took it under the
limit. `src/deck_render.rs` left in #3591: the composer footer's affordance
budgeting moved to [`src/deck_render/footer.rs`](src/deck_render/footer.rs),
taking it from 1518 to 1383. That is the shape a split is meant to have — a
self-contained concern leaving, not lines redistributed.

A ceiling can move only via `make file-size-update`, which lands as a
reviewable baseline diff justified like any other change — treat it as an
escape hatch for an irreducible line (a module declaration in an oversized
`lib.rs`), never as a planning assumption.

## Layout

| File | What it holds |
|---|---|
| [`src/lib.rs`](src/lib.rs) | The authoritative design statement (pure core + thin shell) and the whole public re-export surface. Read it first. |
| [`src/model.rs`](src/model.rs) | `SessionModel` — one agent's fold. `apply` (line 565) is the *only* mutator; `replay` (line 1125) rebuilds it from a log. |
| [`src/render.rs`](src/render.rs) | Leaf panels the deck draws inside its own guarded bands — stat box, transcript rows, and the rest of the single-session drawing code. |
| [`src/panel_guard.rs`](src/panel_guard.rs) | The panel panic boundary itself (L-T7), with `guarded_band` and `guarded_overlay` as its entry points — shared by the deck and the fleet dashboard — and the written argument for what a caught panic can leave in `DeckUi`. |
| [`src/term.rs`](src/term.rs) | `TerminalGuard` + `PanicHookGuard`. Open this before touching anything about raw mode or panics. |
| [`src/deck.rs`](src/deck.rs) | `WorkspaceModel` — N `SessionModel`s plus cross-agent read-models (file ledger, route log, prompt queue, unified trace). |
| [`src/deck_ui.rs`](src/deck_ui.rs) | `DeckUi` and `handle_deck_key` — every deck keybinding, and the documented Esc precedence list (line 1488). The largest file here. |
| [`src/deck_render.rs`](src/deck_render.rs) | `render_deck`: tab bar · active view · trace strip · progress bar · composer · footer · status band. |
| [`src/deck_shell.rs`](src/deck_shell.rs) | `run_deck(...)`: the deck's `select!` loop, plus a third arm — a 33 ms tick that advances the clock and samples resources. |
| [`src/envelope.rs`](src/envelope.rs) | The multi-agent wire types: `Inbound` in, `WorkspaceInput` out, and every out-of-band snapshot the driver pushes. |
| [`src/composer.rs`](src/composer.rs) | The input model: textarea semantics, `classify_enter`, paste chips, and the slash popup shared by both surfaces. |
| [`src/views/*.rs`](src/views) | One module per deck tab (session · agents · installed · traces · graph · files · skills · mcp · issues · settings), each exposing `render(model, ui, area, buf)`. `engine` is the exception: the config editor SETTINGS hosts, with its own `render_panel` and key handler. |
| [`src/diff.rs`](src/diff.rs), [`src/syntax.rs`](src/syntax.rs), [`src/markdown.rs`](src/markdown.rs), [`src/textline.rs`](src/textline.rs) | Shared text presentation — one implementation each of "how a diff looks", source coloring, markdown, and the event→wording table (also consumed by `stella-cli`'s plain renderer). |
| [`src/theme.rs`](src/theme.rs), [`src/palette.rs`](src/palette.rs) | Every color and glyph. `palette.rs` mirrors the brand kit at `docs/brand/`; `theme.rs` is the only module allowed to reference it. |
| [`src/v2/status_bar.rs`](src/v2/status_bar.rs) | SPEC 5's one-row status bar — `worker · stage · ctx [meter] % · $spend · saved $x · ✉ n`, with `? help` pinned right. `cells` is the one decision function (pure over `Status`, so the golden frames are fixture data); `render_band` draws it plus the cache-diagnosis row beneath. Replaced the two-row v1 statline in #4123/#4129; `src/statline.rs` is deleted (#4128) and this module's doc keeps the record of why the two-row shape lost. |
| [`src/v2/status_source.rs`](src/v2/status_source.rs) | `WorkspaceModel` → the v2 status bar's input struct. The **one** projection of SPEC 5's six values, since #4187 deleted the second one — which was the one that drew, and which printed the wire string `context_recall` where SPEC 5 says `context recall`. |
| [`src/progress.rs`](src/progress.rs), [`src/cache_panel.rs`](src/cache_panel.rs), [`src/splash.rs`](src/splash.rs) | Chrome widgets: the unified stage stepper + progress row, the cache formatters behind the status bar's `saved` cell / context overlay, and the launch mark held over session init. |
| [`src/views/cards.rs`](src/views/cards.rs) + [`plan_card`](src/views/plan_card.rs) · [`models_card`](src/views/models_card.rs) · [`budget_card`](src/views/budget_card.rs) | The three floating cards over one shared chrome; their modal key handlers live in [`src/deck_ui/cards.rs`](src/deck_ui/cards.rs). |
| [`src/views/subagents.rs`](src/views/subagents.rs) | The SESSION tab's nested `└─ ◆` subagent blocks under the lead's header. |
| [`src/scroll.rs`](src/scroll.rs), [`src/input.rs`](src/input.rs), [`src/graph.rs`](src/graph.rs), [`src/resource.rs`](src/resource.rs), [`src/attach.rs`](src/attach.rs), [`src/clipboard.rs`](src/clipboard.rs) | Small leaf modules: line-exact viewport math, the outbound message enum, the graph snapshot types, CPU/MEM sampling, pasted-path detection, `⌃V` clipboard capture. |
| [`src/fleet_dashboard.rs`](src/fleet_dashboard.rs) | A separate full-screen surface for `stella fleet` — its own fold (`FleetMsg`), its own `run`, monotonic `Instant` clocks only. |
| [`src/scenario.rs`](src/scenario.rs) | A deterministic scripted multi-agent scenario, driving both `examples/deck_demo.rs` and the snapshot test. |

## Key concepts

**"Pure fold" is a code-level split, not a slogan.** Verify it in two places:
`SessionModel` has exactly one mutator, `apply` ([`src/model.rs`](src/model.rs)),
and `replay` ([`src/model.rs`](src/model.rs)) is just `apply` in a loop. The
panels in [`src/render.rs`](src/render.rs) take `&SessionModel` — the model is
immutable during a draw. So every question a contributor has — "what does this
key do", "what does the screen show after these events" — is answerable by
calling a function with no terminal, no engine, and no tokio runtime.

**Why that matters here.** AGENTS.md's [witness-test contract](../../AGENTS.md)
names TUI rendering as the case where a witness is "genuinely impractical" —
you cannot assert on a real terminal. The fold/render split is how this crate
gets one anyway: fold synthetic events into a model, render into a
`ratatui::backend::TestBackend`, and assert on the flattened **cell buffer**,
never on ANSI bytes. [`tests/progress_brand_fill.rs`](tests/progress_brand_fill.rs) is the
shape to copy — its header states the goal, why it failed before the change,
and what it asserts after.

**One shell.** [`deck_shell::run_deck`](src/deck_shell.rs) drives N agents in a
near-logic-free `select!` loop over (inbound events, key events), plus a third
arm, a 33 ms tick, because gauges and elapsed timers must repaint on a clock
rather than only when the model streams. Anything you are tempted to write
inside that loop belongs in `deck_ui.rs` instead. A second, single-session
shell was deleted in #936 — see [`src/lib.rs`](src/lib.rs)'s module header.

**Keys are a precedence chain, not a match arm.** `handle_deck_key`
([`src/deck_ui.rs`](src/deck_ui.rs)) runs modal contexts first (splash,
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
([`src/deck_ui.rs`](src/deck_ui.rs)) intercepts only the deck-local ones —
`/files`, `/diff`, `/graph`, `/agents`, `/skills`, `/mcp`, `/mcp-search`,
`/settings`, `/sessions`, `/context`, `/inspect`, `/inbox`, and the five
floating cards `/plan`, `/models`, `/budget` — because
they change view state the driver has no say over. (`/budget` renders locally
but its *edit* leaves as `WorkspaceInput::SetBudget`; the deck shows only the
cap the budget stream folds back.) Everything else, `/help` included, is
enqueued as a prompt so the answer lands in the transcript.

**Terminal restoration survives panics in raw mode** — read
[`src/term.rs`](src/term.rs) before changing any of it. `TerminalGuard` is
constructed *before* the first state is acquired and flags each one (raw, alt
screen, bracketed paste, mouse, kitty) as it lands, so a failure partway through
`enter` still rolls back exactly what was entered. Release builds set
`panic = "abort"`, where destructors never run, so `PanicHookGuard::install`
([`src/term.rs`](src/term.rs)) additionally restores from inside the panic
hook — but **only** under `cfg!(panic = "abort")`, then delegating to the
previous hook so the panic message prints on the real screen, not the alternate
one. In unwind builds the hook deliberately does *not* restore: it fires for
every panic on every thread, including panel panics the session survives.

## Gotchas

- **The panic boundary covers every band, but only in unwind builds.**
  [`src/panel_guard.rs`](src/panel_guard.rs) wraps the single-session panels,
  every deck band (tab bar, active tab, trace strip, progress bar, composer,
  footer, status band, splash, each overlay) and the fleet dashboard, so a
  panicking view becomes an error card and the session continues. Release
  builds set `panic = "abort"`, where `catch_unwind` never runs its handler —
  so the shipped binary still dies on a view panic. A new draw surface is
  covered only if you route it through `guarded_band`/`guarded_overlay`, and a
  new `&mut DeckUi` write in a view has to be classified in that module's
  "what a caught panic can leave behind" list or the argument there stops
  being true.
- **Digits never switch tabs, deliberately.** They quick-pick answers on a
  pending question card and must stay typeable as a prompt's first character,
  so `Tab` / `Shift-Tab` are the only tab navigation
  ([`src/deck_ui.rs`](src/deck_ui.rs)). Adding a digit hotkey would eat a
  keystroke meant for the composer.
- Mouse capture is off by default in both `RunOptions` and `DeckOptions` so
  native terminal selection and copy keep working. The deck's event loop
  discards mouse events entirely today — turning the flag on currently costs
  selection and buys nothing.
- Bracketed paste is always enabled. Without it a multi-line paste arrives as
  raw key events and every newline acts as Enter, turning one paste into N
  submissions ([`src/term.rs`](src/term.rs)).
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
witness. `proptest` covers `src/scroll.rs` and `src/syntax.rs`.
No fixtures, env vars, or feature flags are needed. The one uncovered path is
[`tests/deck_pty_smoke.rs`](tests/deck_pty_smoke.rs): it is `#[ignore]`d
because it needs a real TTY.

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
   `ui.metrics`. Register it in [`src/views.rs`](src/views.rs).
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

- [`../../AGENTS.md`](../../AGENTS.md) — "Workspace layout" for the routing rule, and
  "The definition of done: witness tests" for why this crate is named as the
  hard case.
- [`../stella-protocol`](../stella-protocol) — `AgentEvent`, the type every fold
  in here consumes.
- [`../stella-cli`](../stella-cli) — the driver: `src/command_deck.rs` and
  `src/tui.rs` own the slash vocabulary, the on-disk snapshots, and dispatch.
- [`../../website/content/docs/agent-modes.mdx`](../../website/content/docs/agent-modes.mdx)
  — the Command Deck from a user's point of view.
- [`../../website/content/docs/agent-tools/commands.mdx`](../../website/content/docs/agent-tools/commands.mdx)
  — how a user extends the slash menu this crate renders.
