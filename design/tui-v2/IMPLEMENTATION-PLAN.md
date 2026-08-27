# IMPLEMENTATION PLAN: stella TUI v2

Companion to [`SPEC.md`](SPEC.md). Phases are ordered so each ships value alone and each later phase builds on tested earlier code; section 3 is where each one actually stands.

## 1. Architecture

- **Immediate mode, pure projection.** The AEP event stream is the state. The draw function is a pure projection of state onto the buffer. The TUI mutates nothing.
- **State crate boundaries**, as they landed:
  - `stella-tui-theme`: palette consts, hue clamp tests, glyph consts, 16-color fallback map. Shipped as its own crate, `ratatui` its only dependency.
  - *(no widgets crate.)* The plan put transcript, plan panel, tabs, palette overlay and plugin panel host in a `stella-tui-widgets`. They landed in `stella-tui`'s own `src/views/`, each a `render(model, ui, area, buf)` over the fold rather than a `Widget` impl — the deck draws into bands it owns, and a second crate would have bought a boundary nothing crossed.
  - *(no highlighter crate — see below.)* An earlier draft of this plan put a
    `stella-tui-highlight` here: a syntect wrapper owning the lexer, the theme
    build and the per-event cache. Only the **cache** was the deck's to build.
    The lexer is `stella_transcript::syntax`'s, shared with the export grid and
    the Observatory, and the deck keeps the palette (`syntax::tok_style`) and
    nothing else (#4036, #4196).
  - `stella-cli`: event loop, key routing, state reduction from AEP events.
- **Event model**: every transcript event is a struct with its kind, subject, timing and `task: Option<u32>` (`views::transcript::Event`). The plan's per-event `OnceCell<Vec<Line>>` is not how the cache landed — the fold keeps a settled prefix and a tail memo instead (`views/session/fold.rs`), which reuses across events rather than per event. The budget it was for is enforced either way: rendering never recomputes an unchanged highlight, witnessed by `an_unchanged_tail_is_highlighted_once_not_once_per_frame`.
- **Async boundary**: registry searches (skills, MCP), tracker sync, and oauth run as tokio tasks that post results back into state between ticks. The draw path never awaits.

## 2. Dependencies

| Crate | Purpose | Notes |
|---|---|---|
| `ratatui` + `crossterm` | already in use | `BorderType::Rounded` for panels |
| ~~`syntect`~~ | ~~syntax highlighting~~ | **Not taken.** The deck highlights through `stella_transcript::syntax`, which the export grid and Observatory already share. A syntect crate here would be a *fourth* lexer for the same bodies — the drift #3644 and #4036 each closed once (#4196). |
| ~~`nucleo`~~ | ~~fuzzy matching for the palette~~ | **Not taken.** The palette matches through `composer::fuzzy`, which returns the char offsets a query consumed so the letters can light wherever they fell (#5048). A command name is a short ASCII slug and the row order comes from the match kind plus session relevance, so the scoring model a fuzzy-finder crate exists for would be computed and discarded. |
| `insta` | snapshot tests | pairs with `TestBackend` |
| `tokio` | async registry, tracker, oauth | already likely present |

No other new dependencies without explicit approval.

## 3. Where this stands

The phases below are the **plan as written**, kept because each one's acceptance
criteria are still the bar. This table is what shipped against them, from a
spec-vs-implementation audit on 2026-08-26. A phase is `landed` only where its
acceptance criteria are met by tests in the tree; everything else names the
issue that carries the remainder, so a reader chasing a gap goes to the tracker
rather than to this file's history.

| Phase | State | What is left, and where it is tracked |
|---|---|---|
| P0 theme | landed | The palette, the hue clamp, the wordmark and the one-row status bar all ship with tests. The 16-colour half has two tables that disagree (`theme::FALLBACKS` vs `fallback::ansi16`) — #5000. |
| P1 transcript | mostly landed | Turn rules, receipts, rails, metals, queued-steer labels, `^Z`, the compaction one-liner, per-head wall time and the read fold all ship. The skill, memory, gate and model events render but have no live producer — #5031, #5032, #5033, #5034. The `→ task N` tag waits on its only producer, #5039. |
| P2 highlighting | landed | Highlight-on-arrival with tail reuse, two-layer diffs, sign column, gutter. The write footer and the pre-execution delete graph check are #5034. |
| P3 plan and tasks | part landed | The breadcrumb and the plan card ship. Per-task economics, the running-task card and drift rows are #5040; the task zoom is #5041; six task states is #5038; the `[:NEXT]`/`[:THEN]` graph the drift lanes read is #5037; the evidence ledger and per-task cost are #5039. |
| P4 gates | not started | #5042 (board and failure block), #5043 (the proposed revision, which needs #5037). |
| P5 issues and start work | part landed | The ISSUES tab, its state glyphs and its PR strip ship. Heat sort, the linked plan and the sync rule are #4336. `w start work` landed in #5044: the draft overlay, the approval gate, and an approval that takes the `issue:<n>` claim and opens the branch. The r1 plan graph it authors is not yet persisted and the breadcrumb does not yet fill — #5274. |
| P6 graph, skills, MCP | mostly landed | All three tabs ship. The node card's session tags and the footer's query time landed with #5045; the node list's `● hot` still names no turn — #5220. The learned-skill lifecycle is #5046 and its economics and signatures #4337; the MCP pin, latency, tier and first-enable handshake are #5047. |
| P7 palette and plugins | part landed | The palette overlay ships whole: a context section, scattered match letters lit gold, a `recent` section kept per workspace, and the `panel` ground (#5048 — which also amended SPEC 10 to the anchored position the deck ships). Its remaining gaps are the `2m ago` column (#5213) and a rendering that still draws headings under a typed query (#5215). The plugin panel protocol does not exist in any form — #5054 (wire contract), #5055 (host), #5056 (handshake). |

The epics that group these are #5057–#5062. Section 7's definition of done is
unchanged and unmet: it is what closes those epics.

## 4. Phases

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

Acceptance: snapshot of a Rust diff with add and remove rows; a call-counter test proving highlight runs once per event rather than once per frame — shipped as `an_unchanged_tail_is_highlighted_once_not_once_per_frame` in `crates/stella-tui/src/views/session/fold.rs`; delete event refuses to render without a graph check result in state.

### P3: plan panel and task contracts

- Breadcrumb strip plus `tab` expand.
- Task list with per-task economics, running-task card, `⌥` drift rows, footer counts (SPEC 7.3).
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

- Palette overlay with fuzzy matching, gold match letters, context section derived from session state, groups, recents, arg hints (SPEC 10).
- Plugin panel host: `Rect` lease, styled-line blit API, host-owned chrome, handshake rendering, frame budget throttle (SPEC 12).

Acceptance: palette snapshot for query `ga` during a verify turn showing `/gates` first; a test plugin that attempts to draw outside its `Rect` is clipped; over-budget plugin shows the throttle tag.

## 5. Testing strategy

- **Snapshot tests** are the backbone: render into `ratatui::backend::TestBackend`, assert the buffer with `insta`. Every phase adds snapshots; CI fails on unreviewed changes.
- **Theme lint**: the hue clamp tests plus a repo-wide check that no rendering code contains hex literals (all color goes through the theme crate).
- **Red scarcity assertion**: a helper that counts red cells in a healthy-frame snapshot and asserts zero.
- **Performance budgets**: highlight once per event (counter test); full-frame draw under 3ms for a 500-event transcript on the dev machine (bench, not CI-gating initially).
- **Key routing tests**: table-driven tests mapping keys to emitted actions per focus context.

## 6. Risks and mitigations

- **Lexer accuracy**: ~~a real cost, accepted for now~~ — **paid, in
  `stella-transcript`, by #4283.** `stella_transcript::syntax` is now
  grammar-backed: tree-sitter lexes Rust, TypeScript/JavaScript, Python, Go,
  Java, C, SQL and PHP, and the hand-written scans survive only for Markdown,
  TOML and JSON, which have no grammar resident in this workspace. Two token
  classes a keyword table cannot produce — `Tok::Type` and `Tok::Function` —
  joined the shared vocabulary, and all three palettes (`tok_style`,
  `tok_color`, `tok_class`) moved in the same PR.

  The mitigation held as written: it landed **in `stella-transcript`**, so the
  deck, the export grid and the Observatory gained it together rather than a
  deck-local highlighter buying the deck accuracy by making the three surfaces
  disagree again (#4036, #4196). syntect was declined — it loads grammar and
  theme assets at runtime, which invariant #2 forbids in this layer — and
  tree-sitter cost no new supply chain, because `stella-graph` already compiles
  every one of those grammars as a default feature.

  What is still flat: any language with no grammar in this tree, shell, YAML
  and HTML among them. Adding one is new supply chain and a fresh
  `cargo deny check licenses` decision, not a change this bullet pre-approves.
- **Terminal variance on Windows**: legacy conhost degrades truecolor. Mitigation: fallback map plus recommending Windows Terminal; the desktop shell solves it permanently later.
- **Registry and tracker latency**: never block the draw path; all remote work is async with visible `◐` states and stale-data tags.
- **Scope creep in tabs**: AGENTS and SETTINGS are restyle-only in this cycle (SPEC 9.5).

## 7. Definition of done (whole project)

- Every rendering in `renderings/` is reproducible as a live screen or a scripted demo state.
- All snapshots reviewed and green; theme lint green; red scarcity assertion green.
- No warm hex anywhere; wordmark is `stella*` on every screen.
- The four theses in SPEC 1 are each visibly expressed by at least one shipped surface.
