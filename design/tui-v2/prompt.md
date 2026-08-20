# prompt.md: build stella TUI v2

You are executing the stella TUI v2 redesign in the stella Rust workspace. Work phase by phase, tests first, and never invent scope.

## Inputs

Read these before writing any code, in this order:

1. `SPEC-stella-tui-v2.md`: the design contract. It defines the palette, glyphs, layout, event vocabulary, task contract model, every tab, the gate-failure and start-work scenarios, the palette overlay, and the plugin panel protocol.
2. `IMPLEMENTATION-PLAN.md`: phases P0 through P7 with acceptance criteria. The phase order is fixed.
3. `renderings/svg/` and `renderings/png/`: the visual targets. When a spec sentence and a rendering disagree on wording or numbers, the spec wins; when the spec is silent on layout detail, match the rendering.

Also query the code graph before changing existing code: find the current status bar, transcript, and tab implementations and their callers before touching them.

## Operating rules

1. **One phase per work session.** Start at the lowest incomplete phase. Do not start a phase until the previous phase's acceptance criteria are green.
2. **Tests before implementation.** For each phase, write the acceptance tests from the plan first (insta snapshots against `ratatui::backend::TestBackend`, unit tests for rules), watch them fail, then implement until green.
3. **All color goes through the theme crate.** No hex literals outside `stella-tui-theme`. The hue clamp tests (spec 3.2) and the neutral-gray tests are written in P0 and must never be weakened.
4. **Two-metal rule.** Gold is stella acting, silver is the world coming in, red only for failure and destructive events, green only for pass. If you are unsure which metal an element takes, check spec section 4, then ask; do not guess a new color.
5. **Red scarcity.** Add the red-cell-count assertion to every healthy-frame snapshot. Red in a healthy frame is a bug.
6. **Cell-grid honesty.** No feature may assume sub-cell layout beyond the eighth-block bar glyphs. Rounded corners are `BorderType::Rounded` only.
7. **Wordmark.** `stella` in white plus a gold asterisk, `stella*`, upper right of the tab bar, every screen. Remove every `✦ stella` occurrence.
8. **Never block the draw path.** Registry, tracker, and oauth work is async; results land in state between ticks with visible `◐` states.
9. **Contract rule.** Diff-producing tasks require at least one done-means check; read-only tasks must not have one. Enforce at plan validation with a test.
10. **Approval gates.** Gate-failure revision proposals and start-work drafts execute nothing before an explicit `a`. Write the key tests that prove it.
11. **No new dependencies** beyond ratatui, crossterm, syntect (fancy-regex feature), nucleo, insta, and tokio without stopping and asking.
12. **Commit per phase**, message format: `tui-v2 P<n>: <summary>`. Include snapshot files in the commit. Do not squash phases together.

## Per-phase loop

For the current phase:

1. Re-read the matching spec sections and the plan's acceptance list. List the acceptance criteria verbatim at the top of your working notes.
2. Query the graph for every file you expect to touch; note inbound references so you know the blast radius.
3. Write the acceptance tests. Run them. They must fail for the right reason.
4. Implement the smallest code that makes them pass, following the architecture in plan section 1 (pure projection, event cache, crate boundaries).
5. Run the full test suite plus clippy plus fmt. Fix everything.
6. Self-review against this checklist before committing:
   - every new color references a theme token
   - every state has a glyph, never color alone
   - reads collapse, edits expand, compaction is one dim line
   - every event carries its task tag when a plan is active
   - metrics are right-aligned, keybinding hints are in `dim`
   - snapshots reviewed by eye against the matching rendering
7. Commit. Report: phase, criteria status, snapshot list, anything deferred with a reason.

## When blocked

If the spec is ambiguous, the existing code contradicts an assumption, or an acceptance criterion cannot be met as written: stop, state the conflict in one paragraph with the two options you see, and ask. Do not resolve spec conflicts silently.

## Done

The project is done when plan section 6 (definition of done) is fully green and each of the four product theses in spec section 1 can be pointed to on a shipped screen.
