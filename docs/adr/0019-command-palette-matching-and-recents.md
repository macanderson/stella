---
id: adr/0019-command-palette-matching-and-recents
title: "ADR 0019: Command-palette matching, history, and position"
status: proposed
---

# ADR 0019: Command-palette matching, history, and position

- Status: **Proposed** — awaiting ratification by the repository owner.
- Date: 2026-08-26
- Deciders: repository owner (pending)
- Issue: [#5048](https://github.com/macanderson/stella/issues/5048); remainder
  of [#4338](https://github.com/macanderson/stella/issues/4338)
- Scope note: not part of the Phase 0 adaptive-context series (ADRs 0001–0012).
  Filed here because this is where Stella's numbered, ratifiable decision
  records live — see [README](README.md).

## Context

SPEC 10 (`design/tui-v2/SPEC.md`, rendering `08-command-palette`) describes a
command palette the deck had only partly built. Three gaps, each of which
turned out to be a decision rather than a task:

1. **Matched letters.** The spec says matched letters render gold *inside*
   each command name, and names `nucleo` as the matcher. The shipped matcher
   (`SlashMenu::filter_with`) was a hand-rolled three-tier rank — name prefix,
   name substring, description substring — that returned no match positions,
   so the renderer re-derived a `starts_with` and lit a **typed prefix** only.
   A mid-name match appeared in the list with no visible reason, and a query
   whose letters were merely *in order* (`ga` for `/graph query`) matched
   nothing at all.
2. **`recent`.** The spec's section order ends with `recent`; the deck had no
   such section. #4338's own doc comment said why — it needs per-workspace
   persistence, and the deck has no store. That crate boundary is real and is
   not going away.
3. **Position.** The spec asks for a centered `Rect` with a `panel`
   background. The deck anchors the overlay to the composer's left edge,
   opens it upward, and paints no `panel` ground.

Standing decision SD-2 (durability-first) applies to all three: choose the
option that reads as obviously correct in ten years, and record it.

## Decision

### 1. Extend the matcher in-tree; do not take `nucleo`

`crates/stella-tui/src/composer/fuzzy.rs` is a new ~60-line pure module over
two `&str`s. It keeps the existing tiers, inserts a **subsequence** tier
between "name substring" and "description", and every tier now reports the
byte offsets of the characters it matched.

Considered: taking `nucleo` (MPL-2.0, on `deny.toml`'s allow-list, so the
licence was not the obstacle).

Chosen against, for three reasons in descending weight:

- **The palette does not rank by fuzziness.** It ranks by *what* matched —
  a prefix beats a substring beats a scattered match beats a description hit
  — and then reorders inside each tier by what the session is doing
  (`relevant_now`) and what has been run here before. That policy is a product
  decision with tests pinning it. `nucleo` supplies one opaque score for all
  of it, so taking it would mean either discarding the policy or running a
  whole matcher to recover a *by-product* — the indices — of a decision it did
  not make. A dependency held for its side effect is the shape that rots.
- **It stays a pure function.** Every case is a table row in a unit test
  rather than a screenshot, which is the same property that makes
  `relevant_now` reviewable.
- **The corpus is thirty ASCII slugs.** `nucleo` is built to rank a hundred
  thousand paths at frame rate; its `Matcher` is `&mut self` for the scratch
  buffers that make that possible, and `crate::render` is documented `&`-only
  with no interior mutability (its panel-panic boundary rests on it). The
  scratch would have had to live in view state to serve a menu of thirty rows.

The third reason alone would not justify refusing a dependency the spec asked
for. The first would.

**Why durable:** the matching *rule* is now stated in one place, in the
language of the product ("a prefix beats a substring"), testable without a
terminal, with no version to track and no scoring model to be surprised by.
If the vocabulary ever grows by two orders of magnitude, swapping the tier
walk for a scored matcher is a change behind `match_name`, and the palette's
own ranking policy survives it.

### 2. `recent` persists in the workspace-private state tier

`<workspace>/.stella/private/palette-recents.json`, reached through
`stella_store::workspace_private_state_path` — the existing owner-only tier
that already holds `reflections.jsonl`, the self-tuning ledger and
`mcp_oauth.json`. **No new store.** The file is written whole (move-to-front,
deduplicated, capped at five) by
`crates/stella-cli/src/command_deck/palette_recents.rs`.

The deck does not read or write it. `stella-tui` owns no store and must not
learn where a workspace keeps its files, so the list travels the same route
the command vocabulary already travels: the driver pushes
`Inbound::PaletteRecents`, and the deck reports a run back with
`WorkspaceInput::PaletteRan`. The move-to-front rule itself is one shared
function, so the deck's optimistic update and the driver's durable one cannot
disagree.

Considered and rejected: giving `stella-tui` a `stella-store` dependency
(drags SQLite into a rendering crate and inverts the ports rule), and
folding the history from the prompt stream (a third of the vocabulary —
`/files`, `/diff`, `/graph` and the other tab switches — is consumed
deck-side and never reaches the driver, so that history would silently be a
history of the commands that happened to need a model turn).

**Why durable:** it reuses a tier whose security, gitignore and
worktree-redirect behaviour are already decided and tested; the failure mode
is "the menu is in the wrong order", never a lost action; and the crate
boundary that made #4338 defer this is respected rather than worked around.

### 3. The anchored position wins; the spec is amended

The palette stays anchored to the composer, opening upward. SPEC 10's overlay
bullet was amended in the same change to say so, and a guard
(`spec_10_anchors_the_palette_to_the_composer`) fails if the document and the
deck disagree again — the pattern `keymap.rs` established for SPEC 11's plan
chord (#4341).

The reason is that this palette has no input line of its own. Every centered
palette it resembles owns its query field; here the query is typed into the
**composer**, which is pinned to the bottom of the frame. A centered box puts
the letters being typed and the list they filter at opposite ends of the
screen, with the caret nowhere near the rows moving under it. Growing the list
upward out of the text that produced it is the completion-popup shape, and
that is what the surface actually is.

The other half of that bullet was right and now ships: the overlay paints
`token::PANEL` under every cell it covers, so it reads as a surface lifted off
the transcript rather than a bordered hole punched in it.

**Why durable:** the alternative was not "centered vs anchored" but "the
document and the code disagree, silently". Either answer is survivable; the
divergence is not. The amendment plus its guard is the durable half.

## Consequences

- One new module in `stella-tui` (`composer::fuzzy`) and one in `stella-cli`
  (`command_deck::palette_recents`). No new crate, no new dependency, no new
  store.
- `PaletteState` is no longer `Copy` — it carries the `recent` list. It is
  passed by reference everywhere it is read, so the clone is paid once per
  frame.
- `SlashMenu::matches` is now `Vec<SlashMatch>` rather than
  `Vec<&SlashCommand>`, so a match's lit offsets travel with the command they
  belong to instead of in a parallel collection a later sort could take out of
  step.
- SPEC 10 now carries two amendment notes. The guards that read it assert on
  the **normative** half of each bullet, since an amendment note necessarily
  quotes the wording it retires.
- The `08-command-palette` rendering (PNG/SVG) still depicts the centered
  overlay. Regenerating it is filed as follow-up work rather than done here:
  it needs the rendering toolchain, not a code change.
