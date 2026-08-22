# IMPLEMENTATION PLAN: stella TUI v2

Companion to `SPEC-stella-tui-v2.md`. Phases are ordered so each ships value alone and each later phase builds on tested earlier code.

## 1. Architecture

- **Immediate mode, pure projection.** The AEP event stream is the state. The draw function is a pure projection of state onto the buffer. The TUI mutates nothing.
- **State crate boundaries** (adjust names to the existing workspace):
  - `stella-tui-theme`: palette consts, hue clamp tests, glyph consts, 16-color fallback map.
  - `stella-tui-widgets`: transcript, plan panel, task zoom, tabs, palette overlay, plugin panel host. Each widget implements `Widget` or `StatefulWidget` and renders into `Buffer` directly.
  - *(no highlighter crate — see below.)* An earlier draft of this plan put a
    `stella-tui-highlight` here: a syntect wrapper owning the lexer, the theme
    build and the per-event cache. Only the **cache** was the deck's to build.
    The lexer is `stella_transcript::syntax`'s, shared with the export grid and
    the Observatory, and the deck keeps the palette (`syntax::tok_style`) and
    nothing else (#4036, #4196).
  - `stella-cli`: event loop, key routing, state reduction from AEP events.
- **Event model**: every transcript event is a struct with `turn_id`, `task_id: Option<TaskId>`, `kind`, timing, and a lazily built `rendered: OnceCell<Vec<Line<'static>>>`. Rendering an event never recomputes highlights.
- **Async boundary**: registry searches (skills, MCP), tracker sync, and oauth run as tokio tasks that post results back into state between ticks. The draw path never awaits.

## 2. Dependencies

| Crate | Purpose | Notes |
|---|---|---|
| `ratatui` + `crossterm` | already in use | `BorderType::Rounded` for panels |
| ~~`syntect`~~ | ~~syntax highlighting~~ | **Not taken.** The deck highlights through `stella_transcript::syntax`, which the export grid and Observatory already share. A syntect crate here would be a *fourth* lexer for the same bodies — the drift #3644 and #4036 each closed once (#4196). |
| `nucleo` | fuzzy matching for the palette | match indices drive gold letters |
| `insta` | snapshot tests | pairs with `TestBackend` |
| `tokio` | async registry, tracker, oauth | already likely present |

No other new dependencies without explicit approval.

## 3. Phases

### P0: theme foundation

- Add `stella-tui-theme` with every token from SPEC 3.1, glyphs from SPEC 4, and the wordmark helper (`stella` in `text` + `*` in `gold`).
- Write the hue clamp and neutral-gray unit tests first (SPEC 3.2). They must fail on any warm hex.
- Migrate the status bar to the single-line form (SPEC 5): kill the pink CPU meter, the green money, and the two-row wall. Money gold, meters gold-on-gray.
- 16-color fallback map plus `COLORTERM` detection.

Acceptance: theme tests green; `TestBackend` snapshot of the status bar; grep of the workspace finds no orange-band hex (`#E?F?[89AB]...` warm ranges) outside the theme crate.

### P1: transcript v2

- Turn boundary rules with embedded labels, begin line, end receipt line, queued-steer label (SPEC 6.1).
- Event anatomy: rails, heads, right-aligned metrics, task tags (SPEC 6.2).
- Collapse model: reads folded by default, `▸`/`●` toggle, `^Z` turn fold.
- Compaction as a dim one-liner.

Acceptance: snapshots for a scripted turn (begin, skill, read, run, receipt); fold and unfold snapshots; every event row shows its rail in the correct metal.

### P2: highlighting and code events

- **Highlight cache** (shipped, #4127): highlight-on-arrival with tail reuse in
  `crates/stella-tui/src/views/session/fold.rs`, witnessed by
  `an_unchanged_tail_is_highlighted_once_not_once_per_frame`. This was P2's real
  deliverable. The lexer, theme mapping and extension-based language detection
  it was going to wrap are `stella_transcript::syntax`'s
  (`body_paint`/`paint_line`), and the deck supplies only `syntax::tok_style`.
  Do not fork them (#4036, #4196).
- Diff rendering: two-layer rows, sign column, line-number gutter.
- `edit`, `write`, `delete` (with the pre-execution graph check line), and expanded `read` bodies.

Acceptance: snapshot of a Rust diff with add and remove rows; benchmark proving highlight runs once per event (call counter in tests); delete event refuses to render without a graph check result in state.

### P3: plan panel and task contracts

- Breadcrumb strip plus `tab` expand.
- Task list with per-task economics, running-task card, `⑂` drift rows, footer counts (SPEC 7.3).
- Task zoom view: contract, evidence, planned vs actual lanes from `[:NEXT]` and `[:THEN]`, spend strip, action row (SPEC 7.5).
- Enforce the contract rule: diff-producing tasks require at least one check; read-only tasks must not.

Acceptance: snapshots of collapsed strip, expanded panel, and zoom; a unit test that a diff-producing task without checks is rejected at plan validation.

### P4: gates and the failure scenario

- Gate events and the gate board in verify turns, `$0.00 · det` pricing.
- Failure flow per SPEC 8.1: red row, failure block, `^N` jump, proposed revision with `a/e/x`, merge-blocked banner.
- Revision approval mutates the plan graph (`[:NEXT]` for the new task) and logs the drift cause as a trace outcome.

Acceptance: snapshot of a five-gate board with one failure; key test that `a` emits the approve event and nothing executes before it; red appears in exactly the failure rows and nowhere else in the frame (assert on buffer cells).

### P5: issues and start work

- Backlog table over the tracker MCP with heat sort (graph coupling × age).
- Detail pane with linked plan and evidence summary; sync-on-gate-green outbound status update.
- Start-work overlay per SPEC 8.2 including the estimate line and the approval gate.

Acceptance: snapshots of backlog and overlay; heat sort unit test with a fake graph; approval test proving no branch or execution before `a`.

### P6: graph, skills, and MCP tabs

- GRAPH: query bar, glyph-typed node list, grouped relations with reverse edges and session tags, coupling bars (SPEC 9.1).
- SKILLS: unified search across installed, learned, and registry; learned section with rename and reject; signature policy rendering; install-disabled flow (SPEC 9.2).
- MCP: server table with pinned graph row, oauth login flow with deep-link copy, registry search, first-enable handshake (SPEC 9.3).

Acceptance: snapshots per tab; policy test that unsigned results never render an install key; async test that registry results arriving mid-tick render next frame without blocking.

### P7: palette and plugin panels

- Palette overlay with nucleo matching, gold match letters, context section derived from session state, groups, recents, arg hints (SPEC 10).
- Plugin panel host: `Rect` lease, styled-line blit API, host-owned chrome, handshake rendering, frame budget throttle (SPEC 12).

Acceptance: palette snapshot for query `ga` during a verify turn showing `/gates` first; a test plugin that attempts to draw outside its `Rect` is clipped; over-budget plugin shows the throttle tag.

## 4. Testing strategy

- **Snapshot tests** are the backbone: render into `ratatui::backend::TestBackend`, assert the buffer with `insta`. Every phase adds snapshots; CI fails on unreviewed changes.
- **Theme lint**: the hue clamp tests plus a repo-wide check that no rendering code contains hex literals (all color goes through the theme crate).
- **Red scarcity assertion**: a helper that counts red cells in a healthy-frame snapshot and asserts zero.
- **Performance budgets**: highlight once per event (counter test); full-frame draw under 3ms for a 500-event transcript on the dev machine (bench, not CI-gating initially).
- **Key routing tests**: table-driven tests mapping keys to emitted actions per focus context.

## 5. Risks and mitigations

- **Lexer accuracy**: `stella_transcript::syntax` is hand-written and covers a
  fixed `Lang` set, so it is heuristic where a grammar-based lexer (syntect,
  tree-sitter) would be accurate, and silent on languages it does not know.
  That is a real cost and it is accepted for now. Mitigation is about **where** a
  better lexer lands, not whether: it goes into `stella-transcript`, so the deck,
  the export grid and the Observatory gain it together. A deck-local highlighter
  buys the deck accuracy by making the three surfaces disagree again, which is
  the one outcome #4036 forbade (#4196).
- **Terminal variance on Windows**: legacy conhost degrades truecolor. Mitigation: fallback map plus recommending Windows Terminal; the desktop shell solves it permanently later.
- **Registry and tracker latency**: never block the draw path; all remote work is async with visible `◐` states and stale-data tags.
- **Scope creep in tabs**: AGENTS and SETTINGS are restyle-only in this cycle (SPEC 9.5).

## 6. Definition of done (whole project)

- Every rendering in `renderings/` is reproducible as a live screen or a scripted demo state.
- All snapshots reviewed and green; theme lint green; red scarcity assertion green.
- No warm hex anywhere; wordmark is `stella*` on every screen.
- The four theses in SPEC 1 are each visibly expressed by at least one shipped surface.
