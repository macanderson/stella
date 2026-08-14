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

`resolve(&EmbedderEnv)` is a pure function; `from_env()` is the one-line
wrapper that reads the process environment.

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
- **The default admission floor is provisional, not measured.** The
  `SimilarityPosture` contract asks for a floor derived from a model's observed
  relevant/irrelevant score distributions on a real corpus, and no such
  measurement exists yet for these backends on code. The shipped value is
  deliberately permissive and its job is to drop the obviously-unrelated tail
  from an ordered list, not to certify anything. Measuring it per model is
  tracked work.

## God files — do not add lines

This crate has no god files: no file exceeds the gate's 1500-line ratchet
(`scripts/check-file-size.sh`), and none may appear — a new file crossing
1500 lines fails the gate outright, and `scripts/file-size-baseline.txt`
accepts no new entries. When a file here approaches the limit, split it before
it crosses.
