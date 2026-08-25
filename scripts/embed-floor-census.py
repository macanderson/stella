#!/usr/bin/env python3
"""Census: can `DEFAULT_ADMISSION_FLOOR` reject anything at all? (#3096)

`crates/stella-embed/src/http.rs` ships an admission floor of 0.25, and
`SimilarityPosture::Semantic` (`crates/stella-embed/src/seam.rs`) says a floor
must be declared from a measured separation point. #3096 is the measurement.

`crates/stella-tools/tests/relevance_calibration.rs` is the harness that
settles the *whole* question, and it needs a paid embedding key: its queries
are English sentences, and only a real backend can put them in the same space
as the corpus. This script answers the half that needs no key, which is the
half a floor is a claim about -- the tail.

It reads an existing `codegraph.db` and scores stored chunk vectors against
each other. That is a faithful probe of the same space, because
`HttpEmbedder::embed` sends `{"model": ..., "input": texts}` with no
`input_type` field, so Voyage's query/document asymmetry is never engaged and a
stored chunk vector is what a query embedding of that same text would be. The
vectors are L2-normalized at `http.rs`, so cosine is a plain dot product.

The one thing it cannot show is the head: the true relevant/irrelevant frontier
for a natural-language query. Read a low floor here as "this floor cannot
reject anything", never as "this floor is correct".

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
    print("answers get. Measuring that head needs a key:")
    print("  VOYAGE_API_KEY=pa-... cargo test -p stella-tools --test relevance_calibration \\")
    print("      -- --ignored --nocapture")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
