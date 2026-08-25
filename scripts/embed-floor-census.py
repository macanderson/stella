#!/usr/bin/env python3
"""Census: can an admission floor reject anything at all, for a model nobody
has measured? (#3096)

`crates/stella-embed/src/http.rs` keeps `DEFAULT_ADMISSION_FLOOR = 0.25` for a
model absent from its `MEASURED_FLOORS` table, and `SimilarityPosture::Semantic`
says a floor must come from a measured separation point.

For `voyage-code-3` that measurement now exists and is in `MEASURED_FLOORS`: the
labelled harness `crates/stella-tools/tests/relevance_calibration.rs` was run
with a real key and found the relevant and irrelevant distributions *overlap*
-- tightest separation -0.0439, so no floor separates them and the model
declares `Surface`. Every other model in `KNOWN_DIMS` is still unmeasured.

This script is the part of that question answerable with **no key**, for those
models. It reads an existing `codegraph.db` and scores stored chunk vectors
against each other, which probes the same space: `HttpEmbedder::embed` sends
`{"model": ..., "input": texts}` with no `input_type` field, so Voyage's
query/document asymmetry is never engaged and a stored chunk vector is what a
query embedding of that same text would be. The vectors are L2-normalized at
`http.rs`, so cosine is a plain dot product.

It locates the irrelevant **tail** and cannot locate the frontier, so it can
show that a floor is inert and can never show that one is correct. On
`voyage-code-3` it independently corroborates what the labelled run found, from
the other side: the tail is dense between 0.41 and 0.72, which is why no floor
fits underneath a real answer. Run it before spending a key on a new backend --
a tail that never approaches the shipped floor says the floor is inert there
too.

Usage:
    python3 scripts/embed-floor-census.py [path/to/codegraph.db] [--pairs N]
"""

import argparse
import os
import random
import struct
import sqlite3
import sys

DEFAULT_DB = os.path.join(".stella", "private", "codegraph.db")

# The value shipped in crates/stella-embed/src/http.rs, quoted here so the
# census reports against the number actually in the binary.
SHIPPED_FLOOR = 0.25


def pct(xs, p):
    xs = sorted(xs)
    if not xs:
        return float("nan")
    k = (len(xs) - 1) * p
    lo, hi = int(k), min(int(k) + 1, len(xs) - 1)
    return xs[lo] + (xs[hi] - xs[lo]) * (k - lo)


def decode(blob, dims):
    """Little-endian f32, the encoding `stella_embed`'s store writes."""
    return struct.unpack("<%df" % dims, blob)


def dot(a, b):
    return sum(x * y for x, y in zip(a, b))


def main(argv):
    ap = argparse.ArgumentParser()
    ap.add_argument("db", nargs="?", default=DEFAULT_DB)
    ap.add_argument("--pairs", type=int, default=2000, help="random pairs to sample")
    ap.add_argument("--seed", type=int, default=0)
    args = ap.parse_args(argv)

    if not os.path.isfile(args.db):
        print("no code-graph index at %s -- run `stella init` first" % args.db, file=sys.stderr)
        return 2

    db = sqlite3.connect("file:%s?immutable=1" % os.path.abspath(args.db), uri=True)
    try:
        db.execute("pragma quick_check").fetchone()
    except sqlite3.DatabaseError as e:
        print("the index is unreadable: %s" % e, file=sys.stderr)
        return 2

    rows = db.execute(
        "select v.fingerprint, v.dims, v.vector, f.path, v.name, v.kind "
        "from code_graph_chunk_vectors v join code_graph_files f on f.id = v.file_id"
    ).fetchall()
    if not rows:
        print("the index holds no chunk vectors, so there is no tail to measure", file=sys.stderr)
        return 2

    fingerprints = sorted({r[0] for r in rows})
    print("index: %s" % os.path.abspath(args.db))
    print("chunk vectors: %d   fingerprint(s): %s" % (len(rows), ", ".join(fingerprints)))
    if len(fingerprints) > 1:
        print("more than one embedder is stored; scores across fingerprints are not comparable")
        return 2
    dims = rows[0][1]
    print("dimensions: %d" % dims)

    vecs = [decode(r[2], r[1]) for r in rows]
    norms = [dot(v, v) ** 0.5 for v in vecs[: min(200, len(vecs))]]
    print(
        "L2 norm over the first %d vectors: min %.7f max %.7f (cosine is a plain dot product)"
        % (len(norms), min(norms), max(norms))
    )

    rng = random.Random(args.seed)
    scores = []
    n = len(vecs)
    seen = set()
    while len(scores) < args.pairs and len(seen) < args.pairs * 4:
        i, j = rng.randrange(n), rng.randrange(n)
        if i == j or (i, j) in seen:
            seen.add((i, j))
            continue
        seen.add((i, j))
        # Two chunks of the same file are related by construction, so they are
        # not a sample of the irrelevant tail.
        if rows[i][3] == rows[j][3]:
            continue
        scores.append(dot(vecs[i], vecs[j]))

    print()
    print("cosine between %d random cross-file chunk pairs -- the irrelevant tail:" % len(scores))
    print(
        "  min %.4f  p1 %.4f  p25 %.4f  p50 %.4f  p75 %.4f  p95 %.4f  p99 %.4f  max %.4f"
        % (
            min(scores),
            pct(scores, 0.01),
            pct(scores, 0.25),
            pct(scores, 0.5),
            pct(scores, 0.75),
            pct(scores, 0.95),
            pct(scores, 0.99),
            max(scores),
        )
    )
    below = sum(1 for s in scores if s < SHIPPED_FLOOR)
    print(
        "  below the shipped floor of %.2f: %d of %d (%.2f%%)"
        % (SHIPPED_FLOOR, below, len(scores), 100 * below / len(scores))
    )
    print(
        "  the floor would have to reach %.4f to reject even 1%% of this tail, and %.4f for half"
        % (pct(scores, 0.01), pct(scores, 0.5))
    )

    print()
    print("A floor is only defensible if it sits between this tail and the scores real")
    print("answers get, and this census cannot see the second. Measuring it needs a key:")
    print("  VOYAGE_API_KEY=pa-... cargo test -p stella-tools --test relevance_calibration \\")
    print("      -- --ignored --nocapture")
    print("For voyage-code-3 that run is done: the distributions overlap, so the model")
    print("declares Surface and admits nothing. See MEASURED_FLOORS in stella-embed.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
