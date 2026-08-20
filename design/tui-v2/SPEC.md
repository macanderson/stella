# SPEC: stella TUI v2 (command deck)

Status: draft for implementation
Owner: Mac Anderson, Oxagen, Inc.
Scope: full visual and interaction redesign of the stella TUI (Rust, ratatui)
Companion docs: `IMPLEMENTATION-PLAN.md`, `prompt.md`, `renderings/`

---

## 1. Product theses the UI must express

Every screen exists to make these four claims visible without saying them:

1. **Deterministic first, model last.** Anything answerable by computation never goes to a model. The UI prices deterministic work at $0.00 and shows the det/model split everywhere work is summarized.
2. **Tasks close on checks, not on model self-report.** A task is a contract (done means), evidence, and cost. The model cannot mark its own homework.
3. **Drift is recorded, not hidden.** The plan graph distinguishes `[:NEXT]` (planned) from `[:THEN]` (actual). Divergence is rendered, causal, and feeds the learner.
4. **Traces are the product.** Verified traces train the customer's private model. The UI shows trace capture, learned skills, and receipts as first-class objects.

## 2. Design principles

- **The turn is the unit of the transcript.** Turn boundaries are full-width rules with a begin line and an end receipt. Tool calls are details inside a turn, never the top-level rhythm.
- **Two-metal rule.** Gold means stella acting on the world (edit, write, gate, brand, money). Silver means the world coming in (read, skill injection, memory). Red and green appear only for fail and pass semantics, desaturated and cool.
- **Red is the rarest color on screen.** Because red never appears in a healthy frame, a red gate reads as an alarm without animation or sound. Do not use red for anything except failure and destructive events.
- **Evidence over vibes.** Progress is files, tests, gates, and graph writes, not percentages alone.
- **Cell-grid honest.** Every element must be renderable on a terminal character grid. The only corner rounding is `BorderType::Rounded` (`╭ ╮ ╰ ╯`). Bars are block glyphs. No pixel gradients; per-cell color steps only.
- **Collapse what did not change.** Reads fold to one line by default. Edits stay expanded. Compaction is a one-line whisper.
- **Never color alone.** Every state also has a glyph. Diffs always carry `+` and `-` prefixes and a sign column.

## 3. Theme

### 3.1 Palette (black and gold)

All values are `Color::Rgb`. Grays are neutral or one to two points blue above red. This is what keeps the scheme from reading warm or brown on cheap panels.

| Token | Hex | Role |
|---|---|---|
| `bg` | `#0A0A0C` | canvas |
| `panel` | `#0F0F12` | code blocks, panels, tables |
| `hl` | `#17171B` | selected and highlighted rows |
| `border` | `#26262C` | panel borders, dividers |
| `rule` | `#2C2C33` | turn boundary rules |
| `gold` | `#EFC53F` | stella acting: edit, write, gate, brand, money, active tab |
| `gold_bright` | `#F7D96B` | tiny live indicators only: spinner, hot marker, drift glyph |
| `silver` | `#A9AAB5` | world coming in: read, skill, memory, secondary emphasis |
| `silver_type` | `#BFC1CC` | syntax types |
| `text` | `#E8E8EC` | primary text |
| `muted` | `#777782` | secondary text |
| `dim` | `#4B4B56` | hints, keybinding rows, line numbers |
| `comment` | `#565660` | code comments |
| `green` | `#74C991` | pass, `+` diff sign |
| `red` | `#E0687A` | fail, `-` diff sign, delete events, destructive |
| `diff_add_bg` | `#10201A` | added diff row background |
| `diff_del_bg` | `#241019` | removed diff row background |

### 3.2 Hue clamp (enforced in code)

Every color in the gold role must satisfy `r > g > b`, `g >= 0.78 * r`, and `b <= 0.35 * r`. Below the green ratio the color is orange, and orange on black reads brown. Ship this as a unit test on the theme struct so the palette cannot drift.

Every gray token must satisfy `r == g` and `b >= g` (neutral or blue-tipped). Same test.

### 3.3 Wordmark

The wordmark is `stella*`: the word `stella` in `text` white, immediately followed by an asterisk in `gold`, no space, weight 500. It sits in the upper right corner of the tab bar on every screen. The asterisk is the only brand ornament. Never render the wordmark all-gold and never use the old `✦ stella` form.

### 3.4 Typography

JetBrains Mono is the brand terminal font (user-configurable, the TUI inherits the terminal font). Two weights only in any rendered docs or marketing captures: regular and medium.

### 3.5 Degradation

Detect truecolor via `COLORTERM`. On non-truecolor terminals fall back to a 16-color mapping: gold to yellow, silver to white, muted and dim to bright black, red and green to their ANSI counterparts. Glyphs carry state, so the UI stays legible.

## 4. Glyph language

One vocabulary across every panel, including plugin-drawn UI:

| Glyph | Meaning | Metal |
|---|---|---|
| `✓` | done, pass | green |
| `◐` | running (spinner frames `◐◓◑◒`) | gold_bright when acting, silver when observing |
| `○` | queued, pending | dim |
| `◇` | gate (deterministic, merge-blocking) | gold |
| `✗` | failed, delete | red |
| `⑂` | drift, plan revision (fallback `↯` on fonts lacking U+2442) | gold_bright |
| `▸` | collapsed, expandable | matches event metal |
| `＋` | write (new file) | green sign, gold rail |
| `◆` | memory | silver |
| `✦` | skill | silver |
| `▤ ▢ ƒ` | graph node kinds: file, type, fn | silver |

## 5. Layout

Top to bottom:

1. **Tab bar** (1 row): `SESSION AGENTS TRACES GRAPH FILES SKILLS MCP ISSUES SETTINGS`, active tab gold, wordmark right-aligned.
2. **Plan breadcrumb strip** (1 row, SESSION only): `▸ plan r3 · task 3 wire dedup digest · 2/6`. `tab` expands to the full plan panel. The transcript gets full width by default; the old permanent side panel is gone.
3. **Body**: tab content.
4. **Prompt block**: pipeline line (`✓ plan ▸ execute [bar] 50% · verify`), input line `>>>`, keybinding hint row.
5. **Status bar** (1 row, replaces the old two-row wall): `worker · stage · ctx [bar] 35% · $spend · saved $x · det 86% · ✉ n · ? help`. MODEL detail, CPU, MEM, WARMTH, and ENGINE move behind `?` and the AGENTS tab. Money renders gold. Meters render gold fill on `border` gray. No pink, no green meters.

## 6. Transcript

### 6.1 Turn boundaries

- **Turn begin**: full-width rule in `rule` color with an embedded label: `turn 14 · execute · kimi-k3 · budget $0.60`. If a queued steer is being consumed, append it: `queued: "<message>"`, so queue-never-blocks has a visible payoff.
- **Turn end receipt**: rule labeled `turn 14 done · 0:42`, followed by one receipt line: `$0.11 · 18k tok · det 86% · 2 files · 4/4 tests · 1 memory · ↵ audit`. `↵ audit` opens the full trace.
- `^Z` folds an entire turn (everything between two rules).

### 6.2 Event anatomy

Every event renders: a colored left rail (`│` glyphs, 2 cells) for each visual row it owns, a head line (glyph, verb, subject, metrics right-aligned), an optional body, and an optional dim footer. Every event carries its task id, rendered as `→ task 3` on the head when a plan is active. Task tagging is what makes per-task cost attribution and evidence ledgers free.

Rail metals: read silver-dim, edit gold, write gold, delete red, run gold, skill silver, memory silver, gate gold, model gold_bright.

### 6.3 Event types

| Event | Head | Body | Notes |
|---|---|---|---|
| `read` | `▸ read <path> · <n> lines · ⚡ms · ↵ open` | none (collapsed by default) | expanding shows syntax-highlighted excerpt |
| `edit` | `● edit <path> +a -b · ⚡ms` | syntax-highlighted diff (6.4) | expanded by default |
| `write` | `＋ write <path> · new file · n lines` | first 5 highlighted lines, `⋯ n more · ↵ expand` | footer: `registered in graph as module node` |
| `delete` | `✗ delete <path> · -n lines · git-backed · u undo` | one line: `graph check: 0 inbound refs · det` | graph check runs before the tool executes |
| `run` | `● run <cmd>` | result line: pass green or fail red with counts | |
| `skill` | `✦ skill <name> · auto\|/cmd · n tok` | `injected <summary> · used n× this repo` | |
| `memory` (log) | `◆ memory logged · mem_id` | quoted text, then `OBSERVATION ▸ RULE ▸ FACT` ladder with current class lit, `conf 0.62 · kind · decays`, footer `promotes at 0.85 · e edit · x reject` | |
| `memory` (promote) | `◆ memory promoted OBSERVATION → RULE · conf 0.87` | `audit event <id> · now prompt-injected` | one line total |
| `gate` | `◇ gate <name> · state` | see section 8.1 on failure | always shows `$0.00` when deterministic |
| `model` | `◐ model <activity> · tok/s` | footer: `irreducible generation · n of m budgeted model calls this turn` | |
| `compaction` | `↓ compacted 74k→69k · 0 evicted · 0 deduped` | none | dim single line, deliberately quiet |

### 6.4 Syntax highlighting and diffs

- Engine: `syntect` with a custom theme built from section 3.1 (keywords gold, types silver_type, identifiers text, comments comment, strings silver, primitives silver_type). Two metals plus white is the full syntax palette on purpose.
- Highlight once when the event arrives, cache `Vec<Line<'static>>`. Never highlight per frame.
- Diff rows are two-layer: `Line.style` carries the bg tint (`diff_add_bg` or `diff_del_bg`), spans keep syntax fg. ratatui composes them per cell.
- Line number gutter in `dim`. Sign column (`+`/`-`) is mandatory and colored; color is never the only diff signal.

## 7. Plan and task contracts

### 7.1 Task model

A task has three parts:

- **done means**: the contract. A list of checks, each deterministic (`graph`, `unit`, `harness`) where possible, model-judged only when irreducible. A task closes when its checks pass, never when the model says so.
- **evidence**: the ledger of events tagged with this task id (edits, runs, graph writes).
- **cost**: `$ · tok · cache rd% · det/model split · model calls · est remain`.

Rule: contracts are **required only for tasks that produce diffs**. Read-only tasks close on completion of their events. This prevents fake checks written to satisfy the UI.

### 7.2 States

`✓ done`, `◐ running`, `○ queued`, `◇ verify` (gate task, blocks merge), `✗ blocked`, `⑂` drift-inserted.

### 7.3 Plan panel

Collapsed: the breadcrumb strip. Expanded (tab): task list with per-task right-aligned economics (`9k tok`, `det 94%`), the running task as a highlighted card showing its contract line, evidence line, and cost line. Drift-inserted tasks render with `⑂` in gold_bright and an `inserted` tag. Footer: `planned 6 · actual 7 · ⑂ 1 drift`, then `drift is recorded, not hidden. it trains your model.`

### 7.4 Drift

Planned path comes from `[:NEXT]` edges, actual from `[:THEN]`. The task zoom renders both lanes; each divergence carries a cause (for example a compiler error code) and is recorded as a trace outcome. See rendering `03-task-zoom`.

### 7.5 Task zoom

`↵` on a task opens a full-screen view: contract block (checks with per-check mechanism and det/model tag), evidence block, planned vs actual lanes, spend strip, and an action row: `r re-run checks · s split task · b hand to worker · i promote to issue · ⑂ diff plan`. Closing line: `a task closes when its checks pass, not when the model says so.`

## 8. Scenario specs

These are the moments that define the product. The first two are the priority demo scenarios.

### 8.1 Gate failure (rendering `09-gate-failure`)

Red as alarm. When a gate fails during a verify turn:

1. The gate board row flips to `✗ <gate> failed` with a red rail and a red-tinted row background. Every other row keeps its normal metal, so the red row is the only saturated non-gold element on screen.
2. A failure block renders under the gate: failing case name, a two-line stderr or assertion excerpt in dim, and keys `^N jump to failure · l full log · r rerun gate`.
3. stella responds with a **proposed plan revision**, never a silent fix: `⑂ propose r4: add task 3b "<title>"` with the linked cause and any linked issue. Action row: `a approve r4 · e edit · x dismiss`. Nothing runs until approval.
4. The receipt area states `merge blocked · unblocks on green`. Verify work continues to price at `$0.00 · det`.

The alarm quality comes entirely from red scarcity (section 2). No blinking, no bell by default.

### 8.2 Start work (rendering `10-start-work`)

The single best demo moment: an issue becomes a plan while the user watches, and nothing executes before approval.

1. In ISSUES, `w` on a triaged issue opens a centered overlay (ratatui `Clear` over a centered `Rect`).
2. Header: `w start work · <issue id> <title>`, subtitle `draft plan r1 · built from issue text + graph + memory`.
3. Sources line names exactly what was used: the issue, the coupled files from the graph, and any applied memory RULEs with their text.
4. Task list with contract previews: read-only tasks marked `read only · no contract`, diff-producing tasks each show one `done means` line with its mechanism and `det` tag, final task is `◇ verify · n gates · blocks merge`.
5. Estimate line: `~$ · ~tok · det est % · ~minutes`.
6. Action row: `a approve and start · e edit tasks · x cancel`. Footer states plainly: `nothing runs before approval`.
7. On approval, stella creates the branch, the plan becomes r1 with `[:NEXT]` edges, and the breadcrumb strip updates.

### 8.3 Other specced scenarios

Turn begin, turn end receipt, skill auto-trigger, memory log and promotion, compaction, delete with pre-execution graph check, and queued-steer consumption are all defined by section 6 and shown in renderings `01` and `02`.

## 9. Tabs

### 9.1 GRAPH (rendering `04-graph`)

- Query bar: current selector (`file:...`) plus `q` for free-form graph queries. Right side: `438 nodes · 12ms · det`.
- Left: node list for the current scope, glyph-typed (`▤ ▢ ƒ`), right column is edge count. `● hot` marks nodes touched this session, tagged with the turn.
- Right top: node card with **grouped relations including the reverse direction**: `imports 24 → · ← imported-by 12 · writes 2 · tests 6`, then a short mixed sample of edges. Reverse edges may carry session tags (`edited turn 14`).
- Right bottom: **coupling ranking**: neighbors sorted by edge count with gold block-char bars and counts. Caption: `high coupling = blast radius if you edit this file`. This replaces the decorative dot-matrix neighborhood.
- Footer prices the view: `every answer here is deterministic · $0.00 · 12ms`.

### 9.2 SKILLS (rendering `05-skills`)

- **One search box** hits installed skills and the web registry together. Results split into sections: installed, learned, registry.
- Installed rows: name, version, scope, one-line summary, and economics right-aligned (`18× · 0.9k` tok per inject). Caption: prune what never fires.
- **Learned section**: skills stella synthesized from repeated winning traces. Human-named (auto-rename from hash suffixes, keep `was <hash>` provenance), tagged `learned`, showing source trace count and turn. Keys: `r rename · ctrl+o show source traces · x reject teaches the learner`.
- Registry results: name, install count, signature status. `signed` green, `unsigned · blocked by policy` red with `v view anyway`. `i install` lands the skill **disabled until previewed and enabled**.
- Keys: `space on/off · ctrl+o preview · i install · p pin · n new from prompt`.

### 9.3 MCP (rendering `06-mcp`)

- Server table: status dot (gold connected, dim not), name, transport, tool count, latency, auth state. The graph server is pinned first with the caption `graph is pinned · it is the product, not an integration`.
- Disconnected rows highlight with `o login`. Login hint names the mechanism: `opens the browser · returns via stella:// deep link · token stays in keychain`.
- Registry search below, same pattern as skills: results with source tier (official, vendor, community), tool count, installs, signature. Unsigned blocked. New servers land **disabled; the first-enable handshake shows declared capabilities** before any tool call.
- Keys: `↵ tools · a auth · e enable · x remove · r refresh · i install`.

### 9.4 ISSUES (rendering `07-issues`)

- Backlog table over the tracker MCP (Linear, GitHub, or any tracker server). Same state glyphs as the plan panel: `▶ in progress`, `○ triage`, `◇ blocked (gate red)`, `✓ done`.
- **Heat sort**: default ordering is coupling of the issue's touched files times age, computed from the graph. Caption: `from the graph, not vibes`.
- In-progress rows show their linked plan and live task inline (`plan r3 · task 3 live`).
- Detail pane: excerpt, `linked` line (plan progress, branch, evidence summary), and the sync rule: `status syncs back to <tracker> on gate green · no manual updates`.
- `w start work` runs section 8.2. Bottom strip explains the flow in one sentence.
- Keys: `↑↓ select · w start work · n new issue · / search tracker · r refresh`.

### 9.5 AGENTS and SETTINGS

Keep current information architecture (executions table, installed agents, agent editor, model params). Restyle to sections 3 through 5: kill pink and green meters, single-line status, two-metal rows, `stella*` wordmark. The agent editor gets the section 6.4 highlighter for YAML frontmatter and markdown.

## 10. Command palette (rendering `08-command-palette`)

- Overlay: `Clear` over a centered `Rect`, `panel` bg, `rule` border.
- **Fuzzy matching** via `nucleo`; matched letters render gold inside each command name.
- **Context section first**: `relevant now` derives from session state (verify running surfaces `/gates` with live gate status on the row). Then domain groups (plan, graph, skills, ...), then `recent`.
- Rows may carry live right-aligned state (`◐ 2/5 green`, `det · $0.00`) and arg hints (`⇥ <name>`).
- Keys: `↑↓ move · ↵ run · ⇥ args · esc`.

## 11. Keybindings (global)

| Key | Action |
|---|---|
| `tab` | expand or collapse plan panel |
| `^Z` | fold turn |
| `^N` | jump to next failure |
| `^F` | find in transcript |
| `esc` | steer current turn / close overlay |
| `⏎` | queue message (never blocks) |
| `!` | shell |
| `/` | command palette |
| `?` | help, full metric detail |
| `↵` | open or zoom selected object |
| `u` | undo (delete events) |
| `w` | start work (issues) |
| `a` / `e` / `x` | approve / edit / dismiss on any proposal |

## 12. Plugin panel protocol

- Plugins never emit raw ANSI. A plugin is leased a `Rect` and returns styled lines or a cell diff each tick; the host blits it into the buffer.
- The host draws the panel border and title; plugin pixels cannot spoof stella chrome. Plugin panels use a distinct border treatment (host-owned, labeled `◳ panel · <plugin>`).
- Handshake before any panel: manifest signature, declared capabilities, declared denials (no network, no write outside sandbox), then an explicit `[a]llow [d]eny` panel grant. Render the handshake in the transcript (silver rail).
- Frame budget per panel (for example 30fps equivalent); over-budget panels are throttled with a visible tag.

## 13. Accessibility

- Never color alone (section 2). All states carry glyphs; diffs carry sign columns.
- Minimum contrast: `muted` on `bg` is the floor; nothing dimmer than `dim` may carry required information.
- All overlays closable with `esc`; all lists navigable with arrows alone.

## 14. Non-goals

- No pixel gradients, no images, no mouse-required interactions.
- No per-task contract requirement for read-only tasks (section 7.1).
- No warm hues anywhere. The hue clamp test is the enforcement.
- The desktop shell (Tauri wrapper) is out of scope for this spec; the TUI must not depend on it.

## 15. Renderings index

| File | Shows |
|---|---|
| `01-session-turn-lifecycle` | turn begin, skill, collapsed read, highlighted edit diff, receipt, verify turn, single-line status |
| `02-event-vocabulary` | write, delete with graph check, memory log, memory promotion, compaction, queued-steer consumption, rail legend |
| `03-task-zoom` | task contract view: done means, evidence, planned vs actual, spend strip |
| `04-graph` | graph tab: grouped relations, reverse edges, coupling ranking |
| `05-skills` | unified installed + registry search, learned skills, signature policy |
| `06-mcp` | server table, oauth flow, registry install, sandbox-until-review |
| `07-issues` | backlog with heat sort, linked plans, start-work strip |
| `08-command-palette` | fuzzy palette with context section |
| `09-gate-failure` | red-as-alarm gate failure and proposed plan revision |
| `10-start-work` | issue-to-plan overlay with contract previews and approval |

SVGs are the source of truth; PNGs are 2x raster exports. Renders use DejaVu Sans Mono as a stand-in; the brand font is JetBrains Mono.
