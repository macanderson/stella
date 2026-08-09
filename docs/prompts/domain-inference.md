---
id: prompt-domain-inference
title: "domain_inference — the effective prompt"
status: living
---

# `domain_inference`

Inferring the workspace's domains, for memory tagging and recall. Runs during
`stella init` and writes `.stella/domains.toml` — the closed vocabulary that
[reflection.md](reflection.md) later tags lessons against.

| | |
|---|---|
| Call role | `ModelCallRole::DomainInference` (`"domain_inference"`) |
| Dispatch | raw completion, `tools: []` |
| Built in | `infer_domains`, `crates/stella-cli/src/domains.rs` |
| Model | the worker model (`model_hint`) |
| Output cap | 2,048 visible + 4,096 reasoning headroom |
| Temperature | 0.0 |
| Repair | one bounded retry, in-thread |
| Override | none |

A second call site, `crates/stella-cli/src/ingest_cmd/extract.rs`, records
under this same role for record extraction during ingest. Its output is a whole
claim list rather than a 4-10 element taxonomy, so it **states its own cap** —
sized to the document and doubled on truncation, up to 131,072 — and keeps it.
That is the one standalone call site with a per-call reason for a number; see
[README.md](README.md#output-caps).

## Wire shape

```
[ system("You infer domain taxonomies from repository structure. Respond with only valid JSON.")
  user(prompt) ]
```

## User message (template)

```text
Analyze this repository's shape and infer its semantic DOMAINS — the 4-10 major functional areas of the codebase (examples from other projects: auth, billing, ingestion, cli, knowledge-graph, api, ui). For each domain give: name (short kebab-case), description (one line), paths (the workspace-relative directory prefixes that belong to it — only prefixes that actually appear in the listing below).

Respond with ONLY a JSON array, no prose:
[{"name": "...", "description": "...", "paths": ["..."]}]

{summary}
```

`{summary}` is `summarize_repo(root)` — the repository's shape, not its
content.

The "only prefixes that actually appear in the listing below" clause is the
load-bearing constraint: a domain whose `paths` name directories that do not
exist tags nothing, and the failure is silent — recall simply never matches.

## The repair retry

Two attempts, in the same growing message list. On an unparseable first reply
the model's own response is appended and followed by:

```text
That was not a valid non-empty JSON array of domains. Respond with ...
```

Because the retry is a continuation rather than a fresh call, there is no echo
to bound — the model still has its own reply in context. That is the opposite
choice from [plan-repair.md](plan-repair.md), which is a fresh completion and
must therefore echo.

## Fallback

Any failure — provider error, budget exhaustion, both attempts unparseable —
falls back to a **heuristic** taxonomy derived from the directory listing. The
call is never allowed to leave the workspace without domains, because an empty
tag vocabulary would make every subsequent reflection untaggable.

Cost accumulates across both attempts, and the remaining budget is recomputed
before the second so a repair cannot overrun a limit the first attempt nearly
spent.

## Related

- [reflection.md](reflection.md) — consumes the tag vocabulary this produces
- `.stella/domains.toml` — where the result lands
