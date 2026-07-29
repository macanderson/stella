#!/usr/bin/env bash
# End-to-end dry run of the whole pipeline, minus the (expensive) agent phases.
#
# Everything except the Workflow launch is exercised against a real repository: detect the
# gate, partition with a coverage proof, build and validate the harness, score a realistic
# result, render the HTML and validate it, and check model provenance both ways.
#
#     tests/run_all.sh [repo-path]
#
# Exits non-zero on the first failure. Intended to be run after any edit to this skill.
set -uo pipefail

SKILL="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO="${1:-}"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
PASS=0; FAIL=0

ok()   { printf '  \033[32mPASS\033[0m  %s\n' "$1"; PASS=$((PASS+1)); }
bad()  { printf '  \033[31mFAIL\033[0m  %s\n' "$1"; FAIL=$((FAIL+1)); }
run()  { if "$@" >"$TMP/out.txt" 2>&1; then return 0; else return 1; fi }

# Pick a repo to exercise against: the argument, else the first git repo with source in it.
if [ -z "$REPO" ]; then
  for c in "$PWD" "$HOME/Projects"/*; do
    [ -d "$c/.git" ] && REPO="$c" && break
  done
fi
[ -d "$REPO" ] || { echo "no repository to test against; pass one as \$1"; exit 2; }
echo "ultra-audit self-test"
echo "repo: $REPO"
echo

# ── 1. harness invariants ────────────────────────────────────────────────────
if run python3 "$SKILL/tests/check_template.py"; then
  ok "template invariants (JS parses, every agent sets model+phase+header, no Date.now)"
else
  bad "template invariants"; cat "$TMP/out.txt"
fi

# ── 2. detect ────────────────────────────────────────────────────────────────
if run python3 "$SKILL/scripts/detect.py" "$REPO" --out "$TMP/gate.json" --quiet; then
  ok "detect.py produced a gate"
  python3 - "$TMP/gate.json" <<'PY' && ok "gate has languages, a fix-authority decision and an inventory" || bad "gate is malformed"
import json,sys
g=json.load(open(sys.argv[1]))
assert g["languages"], "no languages"
assert g["fix_authority"]["level"] in ("full","tested-files-only","text-only","none")
assert g["inventory"], "no inventory"
PY
else
  bad "detect.py"; cat "$TMP/out.txt"
fi

# ── 3. partition, with the coverage proof ────────────────────────────────────
if run python3 "$SKILL/scripts/partition.py" "$TMP/gate.json" --out "$TMP/units.json"; then
  ok "partition.py succeeded"
  python3 - "$TMP/units.json" <<'PY' && ok "partition is disjoint, complete, and has no oversized unit" || bad "partition proof failed"
import json,sys
u=json.load(open(sys.argv[1])); p=u["proof"]
assert p["disjoint"], f"overlapping ownership: {list(p['duplicated'])[:3]}"
assert p["complete"], f"unowned files: {p['unowned'][:3]}"
assert p["coverage_pct"] == 100.0, p["coverage_pct"]
over=[x["unit"] for x in u["units"] if x["loc"] > u["target"]*1.25]
assert not over, f"oversized units: {over[:3]}"
PY
else
  bad "partition.py"; cat "$TMP/out.txt"
fi

# ── 4. build + validate the harness ──────────────────────────────────────────
printf 'fmt clean; lint clean; tests 100 passed\n' > "$TMP/baseline.txt"
for depth in standard deep max; do
  if run python3 "$SKILL/scripts/build_workflow.py" --gate "$TMP/gate.json" \
       --units "$TMP/units.json" --baseline "$TMP/baseline.txt" --depth "$depth" \
       --out "$TMP/audit-$depth.js"; then
    ok "build_workflow.py --depth $depth"
  else
    bad "build_workflow.py --depth $depth"; cat "$TMP/out.txt"
  fi
done

# A baseline containing a backtick or ${...} must not break the generated script.
printf 'tests: 2 failed in `wal` with ${HOME} unset\n' > "$TMP/bad-baseline.txt"
if run python3 "$SKILL/scripts/build_workflow.py" --gate "$TMP/gate.json" \
     --units "$TMP/units.json" --baseline "$TMP/bad-baseline.txt" --out "$TMP/audit-esc.js"; then
  ok "baseline containing a backtick and \${} is escaped safely"
else
  bad "baseline escaping"; cat "$TMP/out.txt"
fi

# ── 5. score a realistic result ──────────────────────────────────────────────
run python3 "$SKILL/tests/make_fixture.py" --out "$TMP/result.json" && ok "fixture built" \
  || { bad "fixture"; cat "$TMP/out.txt"; }

# score.py exits non-zero when it finds a problem; the fixture deliberately contains one
# (an unverified severe finding), so a non-zero exit here is the CORRECT outcome.
python3 "$SKILL/scripts/score.py" "$TMP/result.json" --json "$TMP/scoring.json" \
  >"$TMP/score.txt" 2>&1
if [ -f "$TMP/scoring.json" ]; then ok "score.py reconciled and emitted scoring.json"
else bad "score.py"; cat "$TMP/score.txt"; fi

python3 - "$TMP/scoring.json" "$TMP/score.txt" <<'PY' && ok "delta is full-precision; contested dimensions detected; unverified findings flagged" || bad "score.py checks"
import json,sys
s=json.load(open(sys.argv[1])); txt=open(sys.argv[2]).read()
assert s["delta_precise"] is not None and abs(s["delta_precise"]) % 1 != 0, \
    f"delta looks rounded: {s['delta_precise']}"
assert s["contested"], "no contested dimensions detected"
assert any("never refuted" in p for p in s["problems"]), "unverified findings not flagged"
assert "WEIGHT DRIFT" not in txt, "weight drift against recorded history"
PY

# ── 6. render + validate the HTML ────────────────────────────────────────────
if run python3 "$SKILL/scripts/render_report.py" "$TMP/result.json" \
     --scoring "$TMP/scoring.json" --baseline "$TMP/baseline.txt" --out "$TMP/report.html"; then
  ok "render_report.py produced a validated, self-contained report"
else
  bad "render_report.py"; cat "$TMP/out.txt"
fi

python3 - "$TMP/report.html" <<'PY' && ok "hostile payloads survived escaping (</script>, \${}, non-ASCII)" || bad "escaping"
import sys
h=open(sys.argv[1]).read()
assert h.count("</script>") == 1, f"{h.count('</script>')} closing script tags"
assert "elapsed_secs" in h and "sanitizer strips" in h, "payload missing"
assert h.lstrip().startswith("<!doctype html>"), "not a standalone document"
assert "prefers-color-scheme" in h and 'data-theme="dark"' in h, "not theme-aware"
PY

# ── 7. provenance, both directions ───────────────────────────────────────────
python3 - "$TMP" <<'PY'
import json,os,sys
d=os.path.join(sys.argv[1],"wf"); os.makedirs(d,exist_ok=True)
SLOT={"opus":"claude-opus-5","fable":"claude-fable-5","sonnet":"claude-sonnet-5"}
j=open(os.path.join(d,"journal.jsonl"),"w")
for i,(phase,m) in enumerate([("unit","opus"),("unit","fable"),("lens","sonnet"),
                              ("panel","opus"),("panel","fable"),("panel","sonnet")]):
    aid=f"a{i:016x}"; key=f"v2:{i:064x}"
    with open(os.path.join(d,f"agent-{aid}.jsonl"),"w") as fh:
        fh.write(json.dumps({"type":"user","agentId":aid,"message":{"role":"user",
          "content":f"ULTRA-AUDIT-AGENT phase={phase} role=r{i} model={m}\n\nbody"}})+"\n")
        fh.write(json.dumps({"type":"assistant","agentId":aid,
          "message":{"model":SLOT[m]}},separators=(",",":"))+"\n")
    j.write(json.dumps({"type":"started","agentId":aid,"key":key})+"\n")
    j.write(json.dumps({"type":"result","agentId":aid,"key":key,"result":{}})+"\n")
j.close()
PY
python3 "$SKILL/scripts/progress.py" "$TMP/wf" --provenance --json "$TMP/prov.json" >/dev/null 2>&1
python3 - "$TMP/prov.json" <<'PY' && ok "provenance verifies a correct model mix" || bad "provenance happy path"
import json,sys
p=json.load(open(sys.argv[1]))
assert p["ok"] is True, p["detail"]
assert len(p["distinct_models"]) == 3, p["distinct_models"]
PY

# Flip one agent to a model it was not assigned; it must be caught.
python3 - "$TMP/wf" <<'PY'
import json,os,sys
f=os.path.join(sys.argv[1],"agent-a0000000000000000.jsonl")
l=open(f).read().splitlines(); o=json.loads(l[1]); o["message"]["model"]="claude-sonnet-5"
l[1]=json.dumps(o,separators=(",",":")); open(f,"w").write("\n".join(l)+"\n")
PY
python3 "$SKILL/scripts/progress.py" "$TMP/wf" --provenance --json "$TMP/prov2.json" >/dev/null 2>&1
python3 - "$TMP/prov2.json" <<'PY' && ok "provenance CATCHES a model that ran off-assignment" || bad "provenance negative control"
import json,sys
p=json.load(open(sys.argv[1]))
assert p["ok"] is False and len(p["mismatches"]) == 1, p
PY

echo
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ] || exit 1
