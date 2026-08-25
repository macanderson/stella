# stella-embed

The **embedding seam**: the `Embedder` trait, the `EmbedderFingerprint` that
stamps every stored vector, the `SimilarityPosture` a backend must declare
about what its scores are allowed to mean, a pure deterministic ranker, and
the two backends — an offline hashing projection and an OpenAI-shaped HTTP
embedder.

## Boundary

**A leaf: no workspace crate appears in its dependency list.** That emptiness
is the point, and it is the [`stella-diff`](../stella-diff/README.md) /
[`stella-home`](../stella-home/README.md) precedent (#1511, #1139). Two planes
now need to turn text into a vector: `stella-context` (retrieval, which owned
this code since it existed) and `stella-graph` (the code graph, which needs
semantic file lookup so `stella search` and CGP recall can answer a question
the asker can only phrase in English). Neither may depend on the other — `stella-graph` owns
`codegraph.db` independently of `stella-context`, and a
`stella-graph → stella-context` edge would drag retrieval, ANN and episodic
memory into the code indexer for the sake of one trait. So the seam moved down
instead. `stella-context::embed` re-exports every item from here, so its own
callers were untouched.

The same reasoning scales past these two callers. Stella is becoming an engine
other applications embed, with capabilities reached through ports and wrappers
supplied as plugins (`doc:engine-embedding`, `doc:turn-loop-wrappers`) — so
"whoever needs a vector links the seam, and nobody links a plane they do not use"
is the shape that keeps a host's build from dragging in Stella's retrieval stack to
get a cosine. The `SimilarityPosture` a backend must declare is the same discipline
pointed at meaning rather than dependencies: a score that cannot be compared across
fingerprints says so, instead of being silently ranked.

Inside the crate the invariant-2 split is by module, not by convention:

| module | I/O? | what it is |
|---|---|---|
| `seam` | no | the trait, the fingerprint, the posture |
| `rank` | no | cosine + a total ordering — property-tested |
| `hash` | no | `HashEmbedder`, the offline fallback |
| `http` | **yes** | `HttpEmbedder`, the semantic backend; feature `http`, off by default |

## Why the semantic backend is HTTP, not in-process ONNX

The alternative was a vendored ONNX/`candle` runtime with checksum-pinned
bge-small weights. It buys a zero-configuration semantic default and costs a
large native dependency tree, a ~130 MB weight fetch on the indexing path, and
a materially bigger binary — a supply-chain decision with an owner, not a
detail to slip into a retrieval change.

The HTTP shape answers the *quality* question today with **no new third-party
dependency at all**: `reqwest` and `serde_json` were already in the workspace,
so `cargo deny`'s licence, bans and advisories gates see nothing new. It is
also not a hosted-only answer — the OpenAI `POST {base}/embeddings` shape is
what Ollama, llama.cpp's server and HuggingFace TEI all speak, so the same
adapter is a fully offline embedder pointed at `127.0.0.1`.

What it gives up, stated plainly: with nothing configured you get
`HashEmbedder`, which is lexical, declares `SimilarityPosture::Surface`, and
therefore cannot admit a candidate on its own. Semantic search is opt-in, and
every surface that degrades to the fallback says so rather than presenting a
lexical answer as a semantic one.

## Configuration

`resolve(&EmbedderEnv)` is a pure function; `from_env()` resolves
`process_env()`, which is `EmbedderEnv::from_process()` — the real environment
— unless a host called `install_process_env` first. `ENV_VARS` names every
variable read, so a caller that must clear the whole surface enumerates it
instead of transcribing it.

A host installs when it holds the credential somewhere the environment must
never see it: Stella's launcher hands the embedding key down an inherited pipe
precisely so a `bash` call the agent makes cannot inherit it, and `setenv`-ing
it for this crate's benefit put it straight back (#3093).

| variable | meaning |
|---|---|
| `STELLA_EMBED_URL` | base URL, e.g. `http://127.0.0.1:11434/v1` — wins over every shortcut, so a stray vendor key cannot redirect an offline setup to the network |
| `STELLA_EMBED_MODEL` | model id; **required** whenever `STELLA_EMBED_URL` is set |
| `STELLA_EMBED_API_KEY` | bearer token; optional for a local server |
| `STELLA_EMBED_DIMS` | vector width; required for a model this build does not know |
| `STELLA_EMBED_FLOOR` | override the admission floor |
| `VOYAGE_API_KEY` | shortcut: `voyage-code-3` (code-specialised) at `api.voyageai.com/v1` |
| `OPENAI_API_KEY` | shortcut: `text-embedding-3-small` at `api.openai.com/v1` |

Resolution has three outcomes and never two: `Configured`, `Unconfigured`
(nothing set — degrade, labelled), and `Incomplete` (something set but not
enough — say what is missing). Collapsing the last two is how a typo becomes a
silent quality regression.

## Semantics worth knowing

- **The fingerprint is the invalidation mechanism.** Every stored vector is
  keyed by `(content_hash, fingerprint)`. Change the model, the width, the
  normalization or the adapter revision and the fingerprint changes, so old
  vectors become invisible to retrieval and get re-embedded incrementally on
  next touch — never mixed into a live vector space.
- **`cosine` returns `0.0` on a length mismatch** rather than panicking or
  truncating. Callers filter by fingerprint first; this is the backstop that
  makes a leak harmless instead of wrong.
- **`top_k`'s ordering is total** — score descending, then key ascending — so
  output that reaches the prompt is byte-stable for identical input
  (invariant 7). Two property tests pin it: ranking is independent of input
  order, and `limit` is always a prefix of the full ranking.
- **A rate limit is an instruction, not a failure.** `HttpEmbedder::embed`
  re-issues a `429` or a `5xx` up to four attempts with exponential backoff,
  honouring `Retry-After`'s delta-seconds form under an 8-second ceiling — a
  backend may ask for a delay, but not park a session-start backfill for an
  hour. The HTTP-date form is deliberately ignored rather than trusting the
  caller's clock to agree with the server's. A `4xx` that will say the same
  thing next time is issued exactly once, and the error names how many
  attempts were made so a slow pass is not mistaken for one that never waited.
  This matters because the chunk embedding pass keeps several requests in
  flight (#4144), and concurrency is precisely what provokes a rate limit.
- **`voyage-code-3` admits nothing, and that is a measured result.** On
  2026-08-24 `crates/stella-tools/tests/relevance_calibration.rs` was run
  against it over this repository's own index (28 744 chunks, four labelled
  queries, 40 candidates deep). The relevant and irrelevant distributions
  **overlap**: tightest separation −0.0439, with one query's labelled answer
  ranking 39th at 0.5978 beneath 38 irrelevant chunks scoring 0.6006–0.6436,
  and two of the four queries returning no labelled answer in 40 candidates at
  all. No floor separates those, so the model declares
  `SimilarityPosture::Surface` — its cosines order candidates and certify none,
  exactly as `HashEmbedder` already reports. #2993 named that outcome in
  advance as the one to expect.
- **Every other backend is unmeasured and says so.** `MEASURED_FLOORS` in
  `src/http.rs` is the per-model table, and a model absent from it keeps
  `DEFAULT_ADMISSION_FLOOR` (0.25) and declares `Semantic`. That number is
  still not a separation point; what it now has is a stated scope — it is the
  value for a model nobody has measured, and a permissive floor is all such a
  backend can defend. `text-embedding-3-small`,
  `text-embedding-3-large` and a local `nomic-embed-text` are the three #2993
  also names, and each needs its own run of the harness. An explicit
  `STELLA_EMBED_FLOOR` outranks the table either way: the operator is then
  making the claim, about a corpus the measurement did not see.
- **The harness lives in `stella-tools`, not here**, so this crate's own tests
  stay hermetic and its leaf status is untouched. Point it at an already-filled
  index with `STELLA_CALIBRATION_INDEX` and one run costs a query embedding per
  labelled query instead of a full pass over the repository.

## God files — do not add lines

This crate has no god files: no file exceeds the gate's 1500-line ratchet
(`scripts/check-file-size.sh`), and none may appear — a new file crossing
1500 lines fails the gate outright, and `scripts/file-size-baseline.txt`
accepts no new entries. When a file here approaches the limit, split it before
it crosses.
