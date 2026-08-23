---
id: deck-film-script
title: "The deck film — narration script"
status: living
---

# The deck film — narration script

The voice track for `docs/demo/stella-deck.mp4`, cut to the shot list in
`crates/stella-tui/examples/deck_film.rs` (`--shots` prints the timings this
page is built from). One cue per shot. A cue's window is the shot's screen
time; its line is written to fit that window at a measured read of about
150 words a minute, with the pause tags carrying the slack. The film is
**68.7 seconds** at 60 fps, silent; the narration is mixed over it.

## For the ElevenLabs hand-off

- **Voice and model.** A calm, low-energy read — the film is a product tour,
  not a launch trailer. Multilingual v2 or v3; stability around 0.55 and
  similarity around 0.75 hold the line reads level across cues.
- **Pauses are in the text.** `<break time="0.4s" />` is the supported pause
  tag; every cue ends on one so the join between cues never lands mid-breath.
  Do not add a trailing silence in the export — the tags already carry it.
- **Read the cues as one script, in order**, then align each cue's audio to
  its `start` time in the table. The windows are deliberately loose (each line
  runs short of its shot by half a second or more) so a slightly slower read
  still lands before the next cut.
- **Spell-outs.** The text below already writes the words the way they should
  be said — `P R` for PR, `C I` for CI, `M C P` for MCP — so the model does
  not have to guess.

### Pronunciation

| Written | Say |
| --- | --- |
| Stella | STEL-uh |
| Oxagen | OX-uh-jen |
| sub-agent | SUB AY-jent |
| `edit_file` (not in the script) | edit file |
| PR | pee-ARR |
| CI | see-EYE |
| MCP | em-see-PEE |
| GLM | gee-el-EM |

## Cues

| # | start | window | shot | words |
| --- | --- | --- | --- | --- |
| 0 | 00:00.0 | 1.7 s | splash | 3 |
| 1 | 00:01.7 | 3.4 s | session · the lead boots and plans | 6 |
| 2 | 00:05.1 | 2.6 s | session · the fan-out | 6 |
| 3 | 00:07.7 | 3.5 s | session · the edit lands | 8 |
| 4 | 00:11.2 | 4.5 s | session · the diff, inline | 10 |
| 5 | 00:15.7 | 6.0 s | session · a lane's own view | 14 |
| 6 | 00:21.7 | 2.6 s | files · the ledger | 6 |
| 7 | 00:24.3 | 5.5 s | files · the diff pane | 12 |
| 8 | 00:29.8 | 6.0 s | graph · the neighborhood | 12 |
| 9 | 00:35.8 | 3.6 s | traces · the event log | 8 |
| 10 | 00:39.4 | 4.2 s | agents · installed definitions | 8 |
| 11 | 00:43.6 | 3.6 s | skills | 9 |
| 12 | 00:47.2 | 3.0 s | mcp · connected servers | 7 |
| 13 | 00:50.2 | 3.0 s | issues | 8 |
| 14 | 00:53.2 | 3.2 s | settings · agents · global | 7 |
| 15 | 00:56.4 | 3.4 s | settings · agents · default | 7 |
| 16 | 00:59.8 | 3.4 s | settings · tools | 7 |
| 17 | 01:03.2 | 5.5 s | session · shipped, and waiting at the gate | 13 |
| — | 01:08.7 | — | end (fade to black) | — |

## Script

Each cue is the text to paste, verbatim, including the tags.

### 0 · 00:00.0 — splash

The mark assembles and resolves into the deck.

```text
This is Stella. <break time="0.6s" />
```

### 1 · 00:01.7 — the lead boots and plans

Wide. The lead registers, recalls two context frames, states the plan, and
posts the task board.

```text
The lead boots, recalls, and plans. <break time="0.4s" />
```

### 2 · 00:05.1 — the fan-out

Wide. Two sub-agent lanes appear above the transcript.

```text
Then fans out to two sub-agents. <break time="0.4s" />
```

### 3 · 00:07.7 — the edit lands

Wide. The lead reads a file, then edits it.

```text
It reads the page, and makes the edit. <break time="0.4s" />
```

### 4 · 00:11.2 — the diff, inline

The camera pushes onto the transcript: the edit's diff, red and green, line
numbers, word-level highlights, under the call that made it.

```text
The diff lands inline, as a pull request shows it. <break time="0.5s" />
```

### 5 · 00:15.7 — a lane's own view

Enter on a lane: the sub-agent's own transcript fills the tab. A search, a
new file with its diff, then a question card waiting for an answer.

```text
Each sub-agent is its own session: its search, its new file, its open question. <break time="0.5s" />
```

### 6 · 00:21.7 — the ledger

Cut to FILES, wide. Two rows: which file, which agent, what operation.

```text
Files: every path the run touched. <break time="0.4s" />
```

### 7 · 00:24.3 — the diff pane

Enter on a row opens the pane below the list; the cursor moves to the second
file halfway through.

```text
Open a row: the full diff, who changed it, and how much. <break time="0.5s" />
```

### 8 · 00:29.8 — the neighborhood

Cut to GRAPH, wide, then in on the node list and across to the relations
panel.

```text
Graph is the code index Stella reads before editing: symbols, callers, coupling. <break time="0.5s" />
```

### 9 · 00:35.8 — the event log

Cut to TRACES: every event of the run, one line each.

```text
Traces: the whole run, one event per line. <break time="0.4s" />
```

### 10 · 00:39.4 — installed definitions

Cut to AGENTS: the definitions on disk, their scope, version, and toolbelt.

```text
Agents are definitions on disk: scope, version, toolbelt. <break time="0.4s" />
```

### 11 · 00:43.6 — skills

Cut to SKILLS: installed skills, and one Stella wrote itself from its traces.

```text
Skills too; Stella writes some from its own wins. <break time="0.4s" />
```

### 12 · 00:47.2 — connected servers

Cut to M C P: connected servers, tool counts, auth state.

```text
M C P servers, their tools, their auth. <break time="0.4s" />
```

### 13 · 00:50.2 — issues

Cut to ISSUES: the tracker's backlog, and the open P R at the top.

```text
Issues: your tracker, the open P R on top. <break time="0.4s" />
```

### 14 · 00:53.2 — settings · global

Cut to SETTINGS, the agents pane: global routing.

```text
Settings is a real editor. Routing first. <break time="0.4s" />
```

### 15 · 00:56.4 — settings · default

The agent's own tab: model, effort, thinking, per role.

```text
Then each agent's model, effort, and thinking. <break time="0.4s" />
```

### 16 · 00:59.8 — settings · tools

The tool surface: groups, switches, and one tool locked by the org.

```text
And the tool surface, org locks included. <break time="0.4s" />
```

### 17 · 01:03.2 — shipped, and waiting at the gate

Back to SESSION, wide and still. The P R is open, the board has moved, and
the lead's next, larger change is waiting at the gate for a human.

```text
The P R is open. The bigger change waits at the gate, for you. <break time="0.8s" />
```

## Keeping it in sync

The cue starts are the shot starts, and the shot list is data. When a shot is
re-timed, re-run `cargo run -q --release -p stella-tui --example deck_film --
--shots` and move the table; when a shot is added, add a cue. A cue whose
words outrun its window shows up as narration crossing a cut — the one thing
the loose windows above exist to prevent.
