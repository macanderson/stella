---
id: search-tool
title: "search — one tool that replaces six, ranked and budget-bound"
status: implemented
---

# search — one tool that replaces six, ranked and budget-bound

**Status:** Implemented. This document records what shipped. A design record
carries the measurement and the reasoning behind it. An epic tracks the rest
of the search work.

## 1. What replaced what

Six old tools are gone: `grep`, `glob`, `graph_query`, `read_symbol`,
`project_overview`, and `gather_context`. One tool does their job now:
`search(query)`.

Their old names still live in `crates/stella-tool-facts/src/catalog.rs`, marked as
retired. `grep` and `glob` sit in a second list, because both are also plain
English words. Nothing was deleted. The code each old tool ran — the code
graph, the embedding index, the file scan — still runs. It just runs inside
`search` now, not behind six separate names.

`search` takes one thing: the query. Nothing else. A parameter can narrow a
job. It must never pick a different job. `search(query, mode="regex")` would
put the same six-way choice back behind one schema. `search` picks its own
method. The caller never does.

## 2. The four rungs

`crates/stella-tools/src/search/engine.rs` holds a function called
`dispatch`. It runs rungs in cost order. It stops at the first rung that
finds something. One rung breaks that rule, on purpose.

1. **Exact symbol.** The code graph checks whether the query names a real
   symbol. This is not a ranking. It is a fact: the symbol exists here, or it
   does not. A hit here goes first, ahead of anything the next rung finds. It
   never gets pushed aside by a guess. Early on, this rung sat dead: the next
   rung almost always found something, so nothing below it ever ran.
2. **Meaning.** The query and the indexed files get turned into vectors and
   ranked by how close they are. This runs whenever an embedder is set up. If
   it fails partway, the next rung runs instead, and the failure is named.
3. **Names.** The code graph's paths and symbol names get matched by word.
   This is the answer when no embedder is set up.
4. **File scan.** The whole workspace gets searched directly, by path and by
   text. No index needed. This is the only rung that works on a workspace
   with no code-graph support at all — which is common, not rare, on a bare
   test container.

Every search runs rung 1. Every search runs rung 2, if an embedder is set up.
So both the code graph and the embedding index get used on every call, not
just the calls where the model happens to reach for them. The answer lists
which rungs ran, in a `via:` line. A reader can tell a ranked answer from a
plain name match.

**A gap.** Rungs 3 and 4 still only run when everything above them finds
nothing. A hit from rung 1, followed by any hit from rung 2, means rung 4
never runs. So `search` does not yet return every plain-text match in the
workspace. A follow-up issue tracks fixing this: run rung 4 every time, and
merge its hits in the same way rung 1's hits already get merged.

## 3. How much detail, and who pays for it

Ranking picks which files matter. A second question follows: how much to
say about each one. Just the name? Or the name, the doc comment, who calls
it, and its full body? `crates/stella-tools/src/search/budget.rs` answers this — from
what got found and what fits, never from a guess at what the query meant.
Guessing was ruled out on purpose: `search` has no mode flag, so a wrong
guess could never be corrected. The model would just go back to the old
tools.

Two plain, side-effect-free pieces make this work:

- **A depth dial**, set once by configuration
  (`STELLA_SEARCH_DEPTH`), never by the model. It runs 1 to 10. Each step up
  adds more detail; it never removes any. So two runs at different depths can
  be compared fairly — the higher one is a strict superset of the lower one.
  The default is the top: every detail, with the budget below in charge of
  what actually fits. A lower default was tried first and cut too soon: a
  three-hit answer often spent under a tenth of its budget, and the model
  then paid for a separate file read to get the rest.
- **A character budget**
  (`STELLA_SEARCH_BUDGET`, 9,000 characters by default), spent across the
  ranked hits. The top hit gets the most detail; each hit after it gets less,
  roughly half as much as the one before. Once that pass is done, any budget
  left over goes back to the best hits first, so nothing sits unspent while
  a top hit could still use more room. The answer always says which hits it
  could not afford — never a silent cut.

## 4. What has not shipped

A design record asked for one more step this build does not take: split the
answer into labelled groups — code, meaning, structure, what got skipped —
and spend the budget across those groups, not just down one ranked list.
Each group would get a small floor, so a schema table or a doc page could
never be crowded out entirely by code hits. `search` does not do this yet.
It returns one ranked list with one `via:` line. A follow-up issue tracks the
grouped answer and its per-group count of what got left out.

## 5. Related

A GitHub design issue proposed this tool and carries the measurement behind
it, including the argument behind §4's gap. This document replaces that
issue as the normative record. A GitHub epic tracks the rest of the search
work. Two follow-up issues track the gaps named in §2 and §4.
