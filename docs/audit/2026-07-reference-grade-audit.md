# Reference-grade audit — July 2026

Scope: the whole tree at `b5def9f`, plus `macanderson/stella-website`.
Method: eleven independent passes — one per crate group, plus brand, both sites,
and the docs — each reading its scope end to end, fixing what was safely
fixable, and filing what was not.

**Scores are post-fix.** Everything this pass repaired is counted as repaired.

## The scale

**100 is the Rust project itself** — rustc, Cargo, the standard library, and the
process around them. That is a deliberately brutal reference: a compiler with a
decades-long correctness culture, a crater run before every release, an RFC
process, and a test suite that is itself a research artifact. Almost nothing
scores near it.

For calibration: SQLite would score in the high 90s on durability and testing
and lower on API ergonomics. A well-run commercial product at Series B is
usually 60–70. Below 50 means a dimension is not being actively maintained.

## Overall: 84 / 100

Stella is a genuinely well-engineered system. The architecture is real rather
than aspirational, the test culture is above the norm for its age, and the
places it is weak are weak in *specific, nameable* ways rather than diffusely.

The single largest gap is not in the code. It is that **none of it is measured**
(#909).

---

## Correctness and engineering

| # | Dimension | Score | Why |
|---|---|---|---|
| 1 | Functional correctness | 88 | Bugs found were real but narrow — a recycled `call_id` evicting a speculation entry, a CRLF needle that made every multi-line edit of a Windows file impossible. None were architectural. |
| 2 | Error handling | 87 | `ProviderError` classification is set at the source and never re-derived; dragged down by `stella-cli` returning one exit code for every failure. |
| 3 | Concurrency safety | 86 | No lock is held across an `.await` anywhere in `stella-core`. Offset by a genuine lock-order inversion in `stella-fleet` that cost *both* contending tasks. |
| 4 | Memory and resources | 84 | Three separate unbounded-collection bugs, each described in its own doc comment as bounded. RAII guards elsewhere are exemplary. |
| 5 | Determinism and reproducibility | 78 | Runtime determinism is a design goal and holds. Build reproducibility does not: release binaries bake the builder's home directory (#910). |
| 6 | Input validation | 84 | Every numeric knob is clamped and framing is now strict; tool inputs are still hand-destructured, so a wrong type reports "missing field". |
| 7 | Crash safety and durability | 92 | `durable::write_atomic` centralises fsync-and-fsync-parent and every private write routes through it. |
| 8 | Cancellation and lifecycle | 90 | `setsid` + group-kill guards + `kill_on_drop` with grandchild-leak witness tests. `stella-cli`'s signal handling is the best code in the tree. |
| 9 | State management | 90 | The pure-core / thin-shell boundary is genuinely held: nothing in the session model that is not reconstructible from sequence 1. |
| 10 | Cost and token accounting | 85 | Real per-provider accounting with cache economics; two estimators disagree on whether a token is chars or bytes (#925). |

## Architecture and design

| # | Dimension | Score | Why |
|---|---|---|---|
| 11 | Modularity and boundaries | 95 | "Ports, not concretions" is enforced, not asserted: `stella-core` has no I/O dependency and no I/O call site. Verified, not taken on faith. |
| 12 | Internal API design | 86 | Narrow injectable ports and additive builders; a handful of `pub` items with no caller. |
| 13 | Public / wire API design | 80 | `AgentEvent::Unknown` plus `serde(default)` forward-compat is reference-grade. The HTTP surface exposes turns but not sessions (#931). |
| 14 | Extensibility | 86 | Hooks, MCP, skills, custom tools, custom agents — a broad and coherent surface. |
| 15 | Dependency hygiene | 86 | 486 crates, all licence-clean; `scraper` was pulling a second argument parser into the binary. Ten tree-sitter grammars compile unconditionally. |
| 16 | Configuration design | 85 | Precedence is correct and centralised, but the chain is parsed 4–5× per launch with a real TOCTOU between validation and use. |

## Security

| # | Dimension | Score | Why |
|---|---|---|---|
| 17 | Sandboxing and confinement | 82 | Path confinement rejects `..`, absolutes, and outward symlinks with adversarial tests. A TOCTOU on intermediate components remains, and the OS sandbox covers `bash` alone. |
| 18 | Authn / authz | 78 | One static bearer token, constant-time compared, now checked before the body is read. No scopes, no rotation, no mTLS. |
| 19 | Secret handling | 90 | `ApiKey` has no `Display`, a redacted `Debug`, and a zeroizing drop. The session export was world-readable until this pass. |
| 20 | Supply-chain integrity | 88 | All 45 actions SHA-pinned with a guard that re-checks it; Sigstore provenance on release. The tools that *enforce* this are themselves installed unpinned (#915). |
| 21 | Privacy and telemetry posture | 84 | The design is genuinely local-first and the enterprise boundary is real. The documentation describing it was wrong in two places (#927). |

## Performance

| # | Dimension | Score | Why |
|---|---|---|---|
| 22 | Latency | 80 | 20–28 ms warm CLI start. The TUI was repainting a full frame per streamed token and re-scanning the transcript per keystroke; both fixed here. |
| 23 | Throughput and scalability | 78 | Fleet fan-out is real. The server has no backpressure — its frame channel is unbounded. |
| 24 | Memory footprint | 82 | Three unbounded caches bounded in this pass; subprocess capture now capped. |
| 25 | Model-usage efficiency | 82 | Prompt-cache-native memory with byte-stable prefixes, now test-pinned. Undercut by a full transcript clone per model call (#921). |

## Quality assurance

| # | Dimension | Score | Why |
|---|---|---|---|
| 26 | Test coverage | 90 | ~4,400 tests across 16 crates, and they run in seconds. |
| 27 | Test quality | 91 | Tests are witnesses, named for the defect they pin, with the rationale in the doc comment. This is the strongest cultural signal in the repository. |
| 28 | Fuzz / property / adversarial | 72 | Proptests exist where they matter most, but the SSE decoder, the path resolver, and the step loop are all hand-enumerated rather than generated. |
| 29 | CI rigour | 90 | Nine gate steps including a file-size ratchet and a doc-citation checker — both well above the norm. MSRV is verified weekly rather than per-PR. |
| 30 | Observability | 62 | `stella-core` emits a typed event at every boundary and reports discarded work explicitly. `stella-serve` emits two `println!`s and nothing else (#930), and the workspace has no logging framework at all. |

## Product and craft

| # | Dimension | Score | Why |
|---|---|---|---|
| 31 | TUI quality | 84 | `⌃G INSPECT` — replaying a past model call's exact context and re-hashing each block against its digest — has no equivalent in any competitor. |
| 32 | Documentation accuracy | 78 | Every documented CLI flag was machine-checked against the binary and none was wrong. Two egress claims were (#927). |
| 33 | Documentation IA and prose | 74 | 80 pages → 61, 76.8k words → 67.2k in this pass. Seven CLI surfaces still have no page (#928). |
| 34 | Brand and visual design | 88 | Was roughly 40: four surfaces carried three different identities and the marketing site's own contrast values failed AA. Now one normative spec, mirrored and measured. |
| 35 | Onboarding and first run | 76 | Install → provider → init → first verified change is short and works. Nothing task-shaped exists above the reference (#929). |
| 36 | Accessibility | 72 | The web surfaces are strong (28/28 pairs pass AA). The TUI never positions the terminal cursor, which locks out screen readers and CJK input entirely (#935). |

## Stewardship

| # | Dimension | Score | Why |
|---|---|---|---|
| 37 | Readability and idiom | 93 | Comments explain the decision and often the rejected alternative. Occasionally over-narrated. |
| 38 | Dead-code and comment hygiene | 85 | 49 `#[allow]` across 310k lines, no `todo!()`, no `FIXME`. Offset by ~2,400 lines of unreachable REPL (#936). |
| 39 | Release engineering | 82 | Attested, checksummed, SHA-pinned — and one of the two release paths silently shipped unattested until this pass. |
| 40 | Governance | 90 | AGPL-3.0-only with a stated dual-licence track, CLA automation, security policy, code of conduct, OpenSSF scorecard. |

---

## Where Stella already beats Claude Code

These are not aspirations; they ship today.

- **`stella inspect`** reconstructs the exact context a past model call received and verifies it against recorded digests. Nothing else in the category does this.
- **`verify_done`** replays a new test against the previous code in a shadow worktree. The test must fail there and pass on the change. A green suite alone is not accepted.
- **`apply_edits`** — transactional multi-file edits with in-memory validation, `dry_run`, per-edit reporting, and rollback.
- **The read→edit drift oracle**, which attributes a failed match to out-of-band change, unchanged file, or never-read.
- **`--budget`**, a hard USD ceiling that aborts between steps and never mid-tool.
- **Cost accounting** to `$/resolved task`, per workspace and across every project.
- **`repo_*` structural refusals** that git itself does not enforce.
- **A tested no-truecolor path** down to ANSI-16, per token.

## Where it does not

- **No sub-agent primitive** (#922). The largest engine gap: context economy, not parallelism.
- **No sessions over HTTP** (#931), so API consumers cannot get the prompt-cache discount the CLI gets.
- **Grep has no context lines or output modes**, so every search costs a second turn to read around the hit.
- **No `@`-file mention, no stdin prompts, no per-invocation tool policy.**
- **No plan mode, no per-hunk diff approval.**
- **And, above all, no measurement.** The Harbor adapter is built to run in the same container and verifier as Claude Code. It has never been run (#909).

## The honest summary

Stella is an 84 that is *shaped like* a 90-plus. The engineering discipline is
already there — the invariants are enforced, the tests are witnesses, the gate
is strict, and the architecture holds under inspection.

What is missing is mostly instrumentation and evidence: nothing measures the
agent against a competitor, nothing observes the server in production, and a
handful of claims in the documentation were not true. Those are the cheapest
points on this scorecard to win, and they are worth more than any amount of
further code polish.
