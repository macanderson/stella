export const meta = {
  name: 'ultra-audit',
  description: 'Exhaustive multi-model engineering audit: 17-dimension rubric, 100% file coverage, fix authority scoped to the language\'s gate, cross-model refutation where no model grades its own work, a blind three-model scoring panel reported as median plus spread, and an independent second opinion',
  phases: [
    { title: 'Regression check', detail: 'one verifier per prior open finding — did the fix land, and is it any good' },
    { title: 'Unit audits', detail: 'one agent per partition, models rotated, fix authority scoped to the gate' },
    { title: 'Fix review', detail: 'every applied fix re-read by a DIFFERENT model than wrote it' },
    { title: 'Cross-cutting', detail: 'workspace-wide lenses; the two heaviest run twice on two models' },
    { title: 'Fresh sweep', detail: 'new eyes on the post-fix tree — what did the first pass miss' },
    { title: 'Verify', detail: 'the real gate, cache defeated, running alone' },
    { title: 'Adversarial', detail: 'cross-model refutation; the finding\'s own author model never judges alone' },
    { title: 'Panel', detail: 'three frontier models score all 17 dimensions blind; median and spread' },
    { title: 'Second opinion', detail: 'independent critique of the scorecard — challenge it, do not restate it' },
    { title: 'Synthesis', detail: 'reconcile the panel, resolve contested dimensions, justify every delta' },
  ],
}

// ── spliced by build_workflow.py (scripts cannot touch the filesystem) ────────
const ROOT = '__ROOT__'
const RUN = /*__RUN__*/ {}                 // { date, commit, depth, tool_versions }
const GATE = /*__GATE__*/ {}               // detect.py output, minus the file inventory
const UNITS = /*__UNITS__*/ []             // partition.py units, with owns_globs
const PRIOR = /*__PRIOR__*/ null           // prior dimension scores, or null on round 1
const PRIOR_ITEMS = /*__PRIOR_ITEMS__*/ [] // prior open findings, to re-verify
const BASELINE = `__BASELINE__`            // gate measured on a pristine tree, as prose
const SALVAGE = /*__SALVAGE__*/ null       // results carried over from an aborted run

const DEPTH = RUN.depth || 'deep'          // standard | deep | max
const PANEL = ['opus', 'fable', 'sonnet']  // frontier only. haiku never judges or scores.
const CONF = {
  standard: { verifiers: 1, refuteCap: 40, sweep: 0, critics: 1 },
  deep:     { verifiers: 2, refuteCap: 64, sweep: 0, critics: 1 },
  max:      { verifiers: 3, refuteCap: 0,  sweep: 6, critics: 3 },
}[DEPTH]

const DIMENSIONS = [
  'formatting', 'language', 'documentation', 'file_names', 'symbol_names',
  'file_organization', 'system_architecture', 'data_storage', 'vendor_lock_avoidance',
  'security', 'durability', 'maintainability', 'loop_correctness',
  'token_efficiency', 'agent_performance', 'compute_efficiency', 'end_user_experience',
]
const WEIGHTS = {
  loop_correctness: 3, security: 3, agent_performance: 3, system_architecture: 3,
  durability: 2.5, token_efficiency: 2.5,
  maintainability: 2, end_user_experience: 2, compute_efficiency: 2,
  data_storage: 1.5, documentation: 1.5, file_organization: 1.5, vendor_lock_avoidance: 1.5,
  formatting: 1, language: 1, file_names: 1, symbol_names: 1,
}
function weightedOverall(s) {
  let n = 0, d = 0
  for (const k of DIMENSIONS) {
    const v = s ? s[k] : null
    if (typeof v !== 'number' || v <= 0) continue
    n += v * WEIGHTS[k]; d += WEIGHTS[k]
  }
  return d ? n / d : 0
}
function median(xs) {
  const a = xs.filter((x) => typeof x === 'number' && x > 0).sort((p, q) => p - q)
  if (!a.length) return null
  const m = a.length >> 1
  return a.length % 2 ? a[m] : Math.round((a[m - 1] + a[m]) / 2)
}
const SCORE_PROPS = {}
for (const d of DIMENSIONS) SCORE_PROPS[d] = { type: 'integer', minimum: 0, maximum: 100 }

// Provenance header. progress.py --provenance reads this back out of each transcript and
// cross-checks it against the model actually used; a mismatch voids every cross-model
// claim in the report. See reference/model-panel.md.
const hdr = (phase, role, model) =>
  `ULTRA-AUDIT-AGENT phase=${phase} role=${role} model=${model}\n\n`

const NO_LLM = GATE.llm_surface && GATE.llm_surface.present === false

// ── the calibration bar, verbatim in every scoring prompt ─────────────────────
const CALIBRATION = `
SCORING — 0-100, ABSOLUTE, against these fixed anchors:

  100      the Rust project itself (rust-lang/rust): RFC-gated change control, Crater
           ecosystem-wide regression proof, editions keeping decade-old code compiling, a
           merge queue that tests the merge commit, UI tests pinning exact diagnostic text,
           unsoundness as a release blocker, doc examples executed by CI, per-change
           performance tracking. RESERVED. Never awarded.
  96-99    reserved for Rust-grade PROCESS: an ecosystem-wide regression gate, a decade of
           proven compatibility, formal or qualified specification work. To score here you
           must name which of those mechanisms the project actually has.
  90-95    reference-grade for its language: ripgrep, tokio, rust-analyzer, SQLite, curl,
           PostgreSQL. Exemplary — and still not Rust.
  80-89    strong; minor nits only
  70-79    solid, with real gaps a new contributor would hit
  60-69    mediocre; notable debt
  <60      deficient

BINDING RULES:
- Above 92 on any dimension, you must justify it against the 100-anchors by naming the
  MECHANISM, not the intention. "The code is very clean" does not earn 93.
- A wall of 90s is a failed audit. So is a wall of 60s. Spread is evidence that you read.
- Thin evidence scores LOWER. A score with no file:line behind it is a guess.
- Effort is not an input. Neither is diff size, fix count, or how hard the work was.
- Process counts only where a gate enforces it. A documented rule with no guard behind it
  scores as an unenforced rule — check whether the guard can be defeated.
- 0 means NOT APPLICABLE to what you examined, and is excluded from every mean. Say so in
  score_notes. Never score 0 to dodge forming a judgment.${NO_LLM ? `
- This workspace has NO LLM surface: score token_efficiency 0 (N/A).` : ''}
`

const FIX_RULES = {
  full: `You MAY fix what is safely fixable. A compile/type gate exists (${(GATE.toolchains || [])
    .filter((t) => t.type_gate).map((t) => t.name).join(', ')}), so a bad edit fails the Verify
phase rather than shipping silently.
  SAFE to fix: doc comments, naming, dead-code removal, error-message quality, comment
  accuracy, clarifying refactors local to one function.
  REPORT INSTEAD (do not attempt): public API or signature changes, trait/interface reshaping,
  async or lifetime changes, dependency changes, anything crossing a unit boundary.`,
  'tested-files-only': `Fix authority is RESTRICTED. There is no type gate, so a bad edit ships
silently — the only thing that would catch it is a test. Before editing any line, confirm a
test actually exercises it; if you cannot confirm that, REPORT instead of fixing. Text-level
fixes (comments, docs, messages) remain allowed anywhere.`,
  'text-only': `Fix authority is TEXT ONLY. There is no type gate and no meaningful test suite,
so nothing would catch a bad edit. You may change ONLY: doc comments, comment accuracy,
error-message text, and dead code you have PROVEN unreachable by exhaustive search. Everything
else is REPORT ONLY. Do not "improve" logic you cannot verify.`,
  none: `*** STRICTLY READ-ONLY. This workspace is not a git repository, so a bad edit cannot
be reverted. Do NOT create, modify or delete ANY file. Report everything. ***`,
}
const FIX_AUTHORITY = FIX_RULES[(GATE.fix_authority || {}).level || 'none']

const SHARED = `
You are a principal engineer on a paid, high-stakes audit of the workspace at ${ROOT}.
Treat it as a $100,000 engagement: observations must be concrete, file-and-line specific,
and defensible. Generic advice is worthless and actively harmful here.

${PRIOR ? `This workspace has been audited before. You are NOT scoring the improvement — you are
scoring the tree as it stands today, on its own merits, against the same absolute bar. New code
carries new defects; hunt those as hard as the old ones.` : 'This is the first audit of this workspace.'}

HARD RULES. Violating any of these corrupts the engagement:
1. DO NOT run any build, test, lint, or install command. Many agents run concurrently and share
   the build lock; an install would rewrite the lockfile underneath every other agent. A single
   central phase runs the real gate after you. STATIC READING ONLY.
2. ${FIX_AUTHORITY}
3. Write ONLY inside the paths you own (stated below, when you own any). A file outside them
   belongs to another agent running right now, and your write will be silently clobbered.
4. Preserve behavior. This is an audit with cleanup privileges, not a rewrite.
5. Match the surrounding code's idiom, comment density, and voice.

*** OUTPUT DISCIPLINE — this is what keeps you alive. Report your 12 most significant
findings, not every nit. Each 'issue' 2-3 sentences. Each 'summary' at most 6 sentences. Call
StructuredOutput as soon as your analysis is done; do NOT narrate your findings in prose
first. Depth of READING is expected in full — it is only the WRITE-UP that must be economical.
Long terminal responses are what the recurring mid-response connection drops actually kill. ***
${CALIBRATION}`

const FINDING_ITEM = {
  type: 'object',
  required: ['severity', 'dimension', 'file', 'issue'],
  properties: {
    severity: { type: 'string', enum: ['critical', 'high', 'medium', 'low'] },
    dimension: { type: 'string' },
    file: { type: 'string' },
    line: { type: 'integer' },
    issue: { type: 'string' },
    remediation: { type: 'string' },
    fixed: { type: 'boolean' },
    proved_by_reading: { type: 'boolean', description: 'true = you read the code that makes this true; false = inferred' },
    exploitability: { type: 'string', description: 'For security: the concrete attack path, or why it is only theoretical' },
    regression_of: { type: 'string', description: 'Set if plainly introduced by recent work' },
  },
}

// ── Phase 0 — did the prior findings actually land? ───────────────────────────
phase('Regression check')

const REGRESSION_SCHEMA = {
  type: 'object',
  required: ['id', 'status', 'evidence', 'reasoning'],
  properties: {
    id: { type: 'string' },
    status: { type: 'string', enum: ['fixed', 'partially_fixed', 'not_fixed', 'fixed_but_regressed', 'not_applicable'],
      description: 'fixed = the defect CLASS is gone; partially_fixed = reported instance closed but a named gap remains; fixed_but_regressed = landed then undone or bypassed' },
    quality: { type: 'string', enum: ['excellent', 'adequate', 'shallow', 'wrong'] },
    test_pinned: { type: 'boolean', description: 'Is there a test that would catch re-introduction? Name it in evidence.' },
    evidence: { type: 'array', items: { type: 'string' }, description: 'file:line citations proving the status' },
    reasoning: { type: 'string' },
    residual_risk: { type: 'string' },
  },
}

const regressionFresh = (PRIOR_ITEMS.length && !(SALVAGE && SALVAGE.phase0))
  ? (await parallel(PRIOR_ITEMS.map((p, i) => () => {
      const m = PANEL[i % PANEL.length]
      return agent(
        hdr('regression', `check-${i}`, m) +
`You are a skeptical verification engineer on a follow-up audit of the workspace at ${ROOT}.
Time has passed since the finding below was filed and substantial work has landed.

*** RULE #1: STRICTLY READ-ONLY. Do NOT create, modify or delete ANY file. Do NOT run build,
test or lint tooling. If you are about to edit or run something, STOP. ***

  FINDING (${p.severity || 'unrated'}): ${p.title || p.id}
  ${p.detail || ''}
  RECOMMENDED FIX: ${p.remediation || '(none recorded)'}

Treat that as A LEAD TO CHECK, NOT A FACT. Some prior findings were fixed, some were fixed
shallowly, and some fixes introduced new defects.

Determine whether the fix landed and whether it is any good. Do NOT accept a commit message,
changelog line, or a comment claiming the fix as evidence — read the code that would have to be
correct for the defect to be gone, and read its callers.

1. Locate the code the finding names. Files may have been renamed, split or moved — search by
   symbol and by behavior, not only by the old path.
2. Decide whether the DEFECT is gone, not whether a diff happened. A fix that closes the exact
   example while leaving the same class open elsewhere is partially_fixed — name the remaining
   hole with file:line.
3. Actively hunt for a bypass. If a deny-list landed, look for a second path that loads the same
   thing. If a guard landed, look for a path that skips it. If a cap landed, check it applies
   before the expensive work and on every branch.
4. Name a regression test that would catch re-introduction, or record that none exists.
5. If the fix is shallow or symptomatic, say so plainly. An organization that papers over
   findings is a worse signal than one that leaves them open.

Cite the lines you read. "It looks fine" is worthless.`,
        { label: `check:${(p.id || p.title || 'item').slice(0, 36)}`, phase: 'Regression check',
          schema: REGRESSION_SCHEMA, model: m, effort: 'high' }
      ).then((r) => (r ? { ...r, id: r.id || p.id || p.title, prior_severity: p.severity, judged_by_model: m } : null))
    }))).filter(Boolean)
  : []

const regressionResults = (SALVAGE && SALVAGE.phase0) ? SALVAGE.phase0 : regressionFresh
if (regressionResults.length) {
  const t = regressionResults.reduce((a, r) => (a[r.status] = (a[r.status] || 0) + 1, a), {})
  log(`Phase 0: ${regressionResults.length} prior findings re-verified — ${JSON.stringify(t)}`)
}

// ── Phase 1 + 2 — unit audits, each pipelined straight into a cross-model fix review ──
// pipeline(), not two parallel() barriers: unit ownership is disjoint, so unit V's fixes can
// be reviewed while unit W is still reading. That saves a full concurrency wave.
phase('Unit audits')

const UNIT_SCHEMA = {
  type: 'object',
  required: ['unit', 'scores', 'summary', 'findings', 'fixes_applied', 'strengths'],
  properties: {
    unit: { type: 'string' },
    summary: { type: 'string' },
    scores: { type: 'object', properties: SCORE_PROPS },
    score_notes: { type: 'string', description: 'Justify the 3 lowest and 2 highest. Note any N/A.' },
    strengths: { type: 'array', items: { type: 'string' } },
    findings: { type: 'array', items: FINDING_ITEM },
    fixes_applied: {
      type: 'array',
      items: { type: 'object', required: ['file', 'change', 'rationale'],
        properties: { file: { type: 'string' }, change: { type: 'string' },
                      rationale: { type: 'string' }, risk: { type: 'string' } } },
    },
    coverage_note: { type: 'string', description: 'Anything you owned but did NOT read, and why' },
  },
}

const FIXREVIEW_SCHEMA = {
  type: 'object',
  required: ['unit', 'verdict', 'reviewed_count'],
  properties: {
    unit: { type: 'string' },
    verdict: { type: 'string', enum: ['all_sound', 'minor_concerns', 'unsound_fixes_present'] },
    reviewed_count: { type: 'integer' },
    bad_fixes: {
      type: 'array',
      items: { type: 'object', required: ['file', 'problem', 'action'],
        properties: { file: { type: 'string' }, problem: { type: 'string' },
                      action: { type: 'string', enum: ['revert', 'repair', 'accept_with_note'] },
                      breaks_build: { type: 'boolean' } } },
    },
    notes: { type: 'string' },
  },
}

// pipeline() returns only the LAST stage's value, so stage 1's audit results are collected
// here as they land. Plain closure capture — the stages are ordinary JS callbacks.
const unitResults = []

const fixReviewsRaw = await pipeline(
  UNITS,
  // Stage 1 — audit, with fix authority.
  (u, _orig, i) => {
    const m = PANEL[i % PANEL.length]
    return agent(
      hdr('unit', u.unit, m) + SHARED + `

YOUR UNIT: ${u.unit}  (~${u.loc} lines, ${u.n_files} files)
YOU OWN — and may write to — EXACTLY these paths and nothing else:
${(u.owns_globs || []).map((g) => '  - ' + g).join('\n') || '  (see enumerated list below)'}
${u.owns_globs && u.owns_globs.length ? '' : (u.owns_files || []).slice(0, 200).map((f) => '  - ' + f).join('\n')}
${u.note ? '\nWHY THIS UNIT MATTERS: ' + u.note : ''}

*** PARTITION DISCIPLINE: this codebase is split across ${UNITS.length} agents working right
now. READ anything anywhere in the workspace. WRITE only to the paths above. ***

METHOD:
1. Read your unit's modules end to end. Do not sample — the partition is sized so you can
   afford to read all of it.
2. Read enough of the callers and surrounding code to judge your unit in context.
3. Hunt real defects: correctness bugs; panics or crashes on attacker- or environment-
   controlled input; unbounded growth; missing cancellation; silently swallowed errors; TOCTOU;
   resource leaks; busy-wait loops; needless allocation on hot paths; blocking IO on an async
   runtime; god-modules; dead code; misleading names; comments that LIE about the code beneath
   them; missing docs on public API.
4. Pay special attention to recently changed code — fixes applied under time pressure are where
   new defects hide. Set regression_of when a defect was plainly introduced recently.
5. Set proved_by_reading=false on anything you inferred rather than confirmed. A later phase
   will try to refute every serious finding, and an honest flag there costs you nothing.
6. Score all 17 dimensions for THIS UNIT, post-fix.

Report economically, as instructed.`,
      { label: `audit:${u.unit}`, phase: 'Unit audits', schema: UNIT_SCHEMA, model: m, effort: 'high' }
    ).then((r) => {
      if (!r) return null
      const rec = { ...r, unit: r.unit || u.unit, group: u.group || u.unit, found_by_model: m,
                    n_files: u.n_files, loc: u.loc }
      unitResults.push(rec)
      return rec
    })
  },
  // Stage 2 — a DIFFERENT model re-reads the fixes. Rule #1 of the panel: no model grades
  // its own homework. A model is worst at seeing the bug it just wrote.
  (res, u, i) => {
    if (!res || !(res.fixes_applied || []).length) return null
    const author = PANEL[i % PANEL.length]
    const reviewer = PANEL[(i + 1) % PANEL.length]
    return agent(
      hdr('fixreview', u.unit, reviewer) +
`You are reviewing code edits made minutes ago by a DIFFERENT model during an audit of ${ROOT}.
You were chosen because the model that wrote these edits cannot reliably see its own mistakes.

*** RULE #1: READ-ONLY. Do NOT edit any file. Do NOT run build, test or lint tooling.
If you are about to do either, STOP and just report. ***

The author claimed these fixes in ${u.unit}:
${JSON.stringify((res.fixes_applied || []).slice(0, 24))}

For EACH claimed fix: open the file, read the actual current code, and judge it.

- Does the edit do what the rationale says? Authors describe intent, not always outcome.
- Would it compile / run? Look for a renamed symbol, a dropped import, a changed arity, an
  unbalanced brace, a now-unused variable that the project's lint treats as an error.
- Did it change BEHAVIOR? This was supposed to be cleanup, not a rewrite. A "dead code"
  removal that was actually reachable is the single most dangerous outcome here — search for
  every reference before accepting one.
- Does a comment or doc it rewrote now contradict the code beneath it?
- Is the fix cosmetic while the defect it claims to close survives?

Set action=revert for anything that changes behavior or may not build; repair for a fix that is
right in spirit but wrong in detail; accept_with_note for sound work worth a remark. Be
specific and cite lines — the Verify phase acts on what you say.`,
      { label: `fixreview:${u.unit}`, phase: 'Fix review', schema: FIXREVIEW_SCHEMA,
        model: reviewer, effort: 'high' }
    ).then((v) => (v ? { ...v, unit: v.unit || u.unit, author_model: author, reviewer_model: reviewer } : null))
  }
)

// Stage 2 yields null for a unit that applied no fixes, and pipeline yields null for a unit
// whose stage-1 agent died — both are absences, not reviews.
const fixReviews = fixReviewsRaw.filter((r) => r && r.verdict)

const totalFixes = unitResults.reduce((n, r) => n + (r.fixes_applied || []).length, 0)
log(`Phase 1: ${unitResults.length}/${UNITS.length} units audited across ${PANEL.join('/')}; ` +
    `${totalFixes} fixes applied. ` +
    `Phase 2: ${fixReviews.length} fix-reviews by a different model than the author, ` +
    `${fixReviews.filter((f) => f.verdict === 'unsound_fixes_present').length} flagged unsound`)

// ── Phase 3 — cross-cutting lenses on the post-fix tree ──────────────────────
phase('Cross-cutting')

const XCUT_SCHEMA = {
  type: 'object',
  required: ['lens', 'scores', 'summary', 'findings', 'strengths'],
  properties: {
    lens: { type: 'string' }, summary: { type: 'string' },
    scores: { type: 'object', properties: SCORE_PROPS },
    score_notes: { type: 'string' },
    strengths: { type: 'array', items: { type: 'string' } },
    findings: { type: 'array', items: FINDING_ITEM },
    top_recommendations: { type: 'array', items: { type: 'string' } },
  },
}

const LENSES = [
  { key: 'security', weight3: true, focus: `SECURITY. You are an adversarial security auditor.
Cover credential handling and every path where a secret can reach disk, logs, telemetry or a
prompt; command injection, path traversal, symlink escape, TOCTOU; parsing of untrusted input;
every network listener; supply chain (lockfiles, pinned CI actions, and whether the guard that
enforces pinning can itself be defeated); unsafe/FFI blocks; process spawning and argument
construction; authn/authz and the permission model, including whether it can be bypassed by a
second code path. Then take every claim the docs make about a trust boundary and VERIFY it by
finding the code that enforces it. Rate exploitability honestly: a fabricated critical is a
firing offense, and so is calling a working exploit chain theoretical.` },
  { key: 'loop-correctness', weight3: true, focus: `LOOP CORRECTNESS and HOT-PATH LATENCY. Judge
termination guarantees, timeout backstops, cancellation propagation (does interrupt actually
stop in-flight work AND its child process groups?), budget enforcement, retry storms, error
classification (retryable vs terminal — and whether a timeout bounding a whole operation is
misclassified as retryable), state-machine correctness, deadlocks, races, orphaned children,
unbounded channels and queues. Then judge LATENCY: what sits on the critical path, what
blocking work runs on an async runtime without being offloaded, what repeats redundantly, what
is needlessly serialized.` },
  { key: 'architecture', focus: `SYSTEM ARCHITECTURE and VENDOR LOCK AVOIDANCE. Map the real
dependency graph and judge the layering: does anything depend upward, are there cycles, are
there god modules? Verify claimed invariants by exhaustive search rather than trusting one
example. Judge every abstraction seam: is it a boundary you could actually swap, or have vendor
specifics bled into core? Name leaks with file:line. Judge whether the decomposition fits the
size of the system.` },
  { key: 'data-durability', focus: `DATA STORAGE and DURABILITY. Read the persistence layer and
every call site that writes state. Judge schema and migrations (forward-only? tested?
versioned? idempotent?), transaction boundaries and atomicity, crash recovery, concurrent
access from multiple processes, file locking, retention and growth bounds on EVERY store, and
behavior when the disk is full or the data is corrupt. Check every persisted file that is not
in a database: is any written in place rather than write-temp-then-rename?` },
  { key: 'efficiency', focus: `${NO_LLM ? '' : `TOKEN EFFICIENCY and `}COMPUTE EFFICIENCY.${NO_LLM ? '' : `
This is an LLM application: read every prompt template, tool description, tool RESULT
formatting and context injection. Verify any prompt-cache claim by checking that nothing
volatile (a timestamp, a relative age, a random ordering, a set iteration) is spliced into the
cached prefix. Check that EVERY tool-result path is bounded before it enters context.`}
For compute: hot-path allocations, needless clones, O(n^2) scans, blocking IO inside async,
unbounded buffers, redundant passes over the same data, render cost. Quantify where you can
("this clones the whole transcript every step").${NO_LLM ? ' Score token_efficiency 0 (N/A).' : ''}` },
  { key: 'naming-consistency', focus: `NAMING, FORMATTING, FILE ORGANIZATION and LANGUAGE. Judge
the tree as a reader meeting it for the first time. Cover naming conventions and how many run
at once, module organization and god-modules (and whether any size limit is enforced by
anything at all), whether similar concepts use the same word everywhere or drift, vocabulary
consistency between code, CLI and docs, public-API doc coverage, and user-facing string
quality. Find comments that LIE about the code beneath them — the worst defect class here.
Check for mojibake and double-encoded UTF-8 in user-facing strings.` },
  { key: 'ux-readiness', focus: `END USER EXPERIENCE and RELEASE READINESS. Walk the real user
journey by reading code: install, first run, auth setup, the command surface (consistent?
discoverable? are flags named uniformly? is help any good?), error messages when things go
wrong, and machine-readable output (one contract, or several incompatible idioms?). Find where
a new user falls off. Then judge the repo as a stranger who wants to contribute: README honesty
(does it claim anything the code does not do?), license consistency across every metadata file,
contribution and security-disclosure process, CI transparency, and whether a contributor can
get a working dev loop from the README alone.` },
  { key: 'maintainability', focus: `MAINTAINABILITY and TEST ADVERSARIALISM. Judge the test
suite as an adversary would: do tests assert real behavior or merely that the code ran? Is
there a test that would FAIL if the central guarantee broke — find it, or record that it does
not exist. Look for tests asserting on mocks, tests with no assertions, skipped or ignored
tests, flakiness suppression, and coverage thresholds set below what is already achieved. Judge
lint policy (are warnings allowed to accumulate?), reviewability of recent diffs, and whether
CI actually blocks a merge or merely reports.` },
]

// The two weight-3 lenses run twice, on two different models, and their findings are UNIONED.
// Intersecting at discovery time would throw away exactly the defects only one model can see.
const LENS_JOBS = []
LENSES.forEach((l, i) => {
  LENS_JOBS.push({ ...l, model: PANEL[i % PANEL.length], pass: 1 })
  if (l.weight3) LENS_JOBS.push({ ...l, model: PANEL[(i + 1) % PANEL.length], pass: 2 })
})

const xcutResults = (await parallel(LENS_JOBS.map((l) => () => agent(
  hdr('lens', `${l.key}#${l.pass}`, l.model) + SHARED + `

YOUR LENS: ${l.key}${l.pass > 1 ? ` (independent second pass by a different model — do NOT assume the first pass found everything, and do not try to agree with it; you cannot see it)` : ''}

*** RULE #1: YOU ARE READ-ONLY THIS PHASE. Do NOT create, modify or delete ANY file. Do NOT run
build, test or lint tooling. If you are about to, STOP. ***
Unit agents have just applied fixes to this tree. You read the POST-FIX state and score THAT.

${l.focus}

METHOD: range across the WHOLE workspace at ${ROOT} — you are not scoped to one unit. Use
ripgrep aggressively to find EVERY instance of a pattern rather than reasoning from the first
one you see. Read the files that matter end to end. Every finding needs file:line and a
concrete consequence. Distinguish what you PROVED by reading code (proved_by_reading=true) from
what you INFER (false). Do not report style opinions as defects. If you find nothing critical,
say so — inventing a critical to look thorough is the worst thing you can do here.

Score the 17 dimensions: a real workspace-wide score for the dimensions your lens covers, and 0
for the rest (synthesis ignores the zeros).`,
  { label: `lens:${l.key}#${l.pass}`, phase: 'Cross-cutting', schema: XCUT_SCHEMA,
    model: l.model, effort: 'high' }
).then((r) => (r ? { ...r, lens: r.lens || l.key, pass: l.pass, found_by_model: l.model } : null))))).filter(Boolean)

log(`Phase 3: ${xcutResults.length}/${LENS_JOBS.length} lenses (${LENSES.filter((l) => l.weight3).length} run twice on ` +
    `different models); ${xcutResults.reduce((n, r) => n + (r.findings || []).length, 0)} findings`)

// ── Phase 4 — fresh sweep (max depth only) ───────────────────────────────────
phase('Fresh sweep')

const SWEEP_ANGLES = [
  'What does this system do when a dependency it trusts returns something malformed? Trace every external boundary.',
  'Find the invariant this codebase most depends on, then find the code path that violates it.',
  'Where does this system lose data, or lose a write, under concurrency or crash?',
  'Read the newest code in the tree (git log --since is unavailable to you, so use comments, TODOs and structure). New code carries new defects.',
  'Find the defect class that appears more than five times. One instance is a bug; six is an architectural fact.',
  'What does the documentation promise that the code does not deliver? Verify each claim against enforcing code.',
]
const sweepResults = CONF.sweep
  ? (await parallel(SWEEP_ANGLES.slice(0, CONF.sweep).map((q, i) => () => agent(
      hdr('sweep', `angle-${i}`, PANEL[i % PANEL.length]) + SHARED + `

*** RULE #1: READ-ONLY. Do NOT edit any file. Do NOT run build, test or lint tooling. ***

You are a fresh pair of eyes on a tree that has ALREADY been audited unit-by-unit and by eight
cross-cutting lenses. Everything obvious has been found. Your job is what a partitioned read
structurally cannot see: defects that only appear when you follow one question across every
module boundary at once.

YOUR QUESTION: ${q}

Pursue it across the whole workspace. Go deep on one thing rather than broad on many. Report
only what you can support with file:line — if the honest answer is "the codebase handles this
well", report that with the evidence, and score nothing.

Score only the dimensions your question actually bears on; 0 for the rest.`,
      { label: `sweep:${i}`, phase: 'Fresh sweep', schema: XCUT_SCHEMA,
        model: PANEL[i % PANEL.length], effort: 'high' }
    ).then((r) => (r ? { ...r, lens: `sweep:${r.lens || i}`, pass: 1, found_by_model: PANEL[i % PANEL.length] } : null))))).filter(Boolean)
  : []
if (CONF.sweep) log(`Phase 4: ${sweepResults.length} sweep angles; ` +
  `${sweepResults.reduce((n, r) => n + (r.findings || []).length, 0)} additional findings`)

// ── Phase 5 — verify the real gate ───────────────────────────────────────────
phase('Verify')

const VERIFY_SCHEMA = {
  type: 'object',
  required: ['fmt_clean', 'lint_clean', 'tests_pass', 'regressions_found', 'regressions_fixed', 'final_state'],
  properties: {
    fmt_clean: { type: 'boolean' }, lint_clean: { type: 'boolean' },
    tests_pass: { type: 'boolean' }, typecheck_clean: { type: 'boolean' },
    commands_run: { type: 'array', items: { type: 'string' } },
    regressions_found: { type: 'array', items: { type: 'string' } },
    regressions_fixed: { type: 'array', items: { type: 'string' } },
    reverted: { type: 'array', items: { type: 'string' } },
    preexisting: { type: 'array', items: { type: 'string' } },
    unfixed: { type: 'array', items: { type: 'string' } },
    test_summary: { type: 'string' }, final_state: { type: 'string' },
  },
}

const suspectFixes = fixReviews.flatMap((f) => (f.bad_fixes || [])
  .filter((b) => b.action === 'revert' || b.breaks_build)
  .map((b) => ({ unit: f.unit, ...b })))

const GATE_CMDS = (GATE.project_gate ? [`PROJECT'S OWN GATE (prefer this): ${GATE.project_gate.command}`] : [])
  .concat((GATE.toolchains || []).flatMap((t) => ['fmt_check', 'fmt_write', 'lint', 'typecheck', 'test', 'docs']
    .filter((k) => t[k]).map((k) => `${t.name} ${k}: ${t[k]}`)))
  .concat((GATE.toolchains || []).filter((t) => t.cache_note).map((t) => `CACHE TRAP (${t.name}): ${t.cache_note}`))

const verify = (GATE.fix_authority || {}).level === 'none'
  ? { skipped: true, final_state: 'Read-only audit: no edits were made, so no gate was run.' }
  : await agent(
      hdr('verify', 'gate', 'opus') +
`You are the integration verifier for the workspace at ${ROOT}. ${UNITS.length} agents just
applied audit fixes in parallel. Get the tree to a fully green gate and report honestly.

MEASURED BASELINE, taken on a PRISTINE checkout of the same commit BEFORE any agent ran:
${BASELINE}

Anything worse than that baseline IS a regression from this audit's edits. Anything already in
it is NOT — report those under 'preexisting' and fix them anyway if you can.

You ARE permitted and required to run build tooling. You run alone; there is no lock contention.

THE GATE:
${GATE_CMDS.map((c) => '  ' + c).join('\n')}

*** The cache traps above are not advisory. A cached lint run reports zero warnings whether or
not there are any. Use the forced form. Check results BY EXIT CODE, not by grepping output —
this shell forces ANSI colour, so pattern-matching on output silently misses matches. ***

A different model already reviewed the applied fixes and flagged these as unsound. Deal with
each one first, before anything else:
${suspectFixes.length ? JSON.stringify(suspectFixes.slice(0, 30)) : '  (none flagged)'}

STEPS: format; then lint with all targets and features and fix EVERY error and warning,
iterating until clean; then run the full test suite and fix genuine regressions. If a fix is
beyond you, revert that specific hunk with git rather than leaving the tree broken — a broken
tree is a total failure of this engagement. Re-run the formatter and confirm the linter is clean
at the end. Record every command you actually ran in commands_run.

Be scrupulously honest. If tests still fail, say so with counts. Do not report success you did
not achieve: a later step re-runs these commands independently and will catch you.`,
      { label: 'verify:gate', phase: 'Verify', schema: VERIFY_SCHEMA, model: 'opus', effort: 'high' }
    )
log(`Phase 5: fmt=${verify && verify.fmt_clean} lint=${verify && verify.lint_clean} ` +
    `tests=${verify && verify.tests_pass} reverted=${((verify || {}).reverted || []).length}`)

// ── Phase 6 — cross-model adversarial refutation ─────────────────────────────
phase('Adversarial')

const allFindings = []
for (const r of unitResults) for (const f of (r.findings || [])) allFindings.push({ ...f, source: r.unit, found_by_model: r.found_by_model })
for (const r of [...xcutResults, ...sweepResults]) for (const f of (r.findings || [])) allFindings.push({ ...f, source: `lens:${r.lens}`, found_by_model: r.found_by_model })

const severeAll = allFindings.filter((f) => f.severity === 'critical' || f.severity === 'high')
const severe = CONF.refuteCap ? severeAll.slice(0, CONF.refuteCap) : severeAll
const notRefuted = severeAll.length - severe.length
if (notRefuted > 0) log(`!! Phase 6 cap: ${notRefuted} severe findings will NOT be refuted ` +
  `(depth=${DEPTH}, cap=${CONF.refuteCap}). They enter the report as UNVERIFIED, not as confirmed.`)
log(`Phase 6: refuting ${severe.length} critical/high of ${allFindings.length} total, ` +
    `${CONF.verifiers} cross-model verifier(s) each`)

const VERDICT_SCHEMA = {
  type: 'object',
  required: ['refuted', 'confidence', 'reasoning'],
  properties: {
    refuted: { type: 'boolean' },
    confidence: { type: 'string', enum: ['high', 'medium', 'low'] },
    corrected_severity: { type: 'string', enum: ['critical', 'high', 'medium', 'low', 'not-a-defect'] },
    already_fixed: { type: 'boolean', description: 'The code now reads correctly — refuted as a CURRENT defect' },
    reasoning: { type: 'string', description: 'Cite the specific code you read. Under 200 words.' },
  },
}

const judged = (await parallel(severe.map((f, i) => () => {
  // Rule #1 of the panel: the finding's own author model never judges it alone.
  const others = PANEL.filter((m) => m !== f.found_by_model)
  const pool = CONF.verifiers >= 3 ? PANEL : others.slice(0, CONF.verifiers)
  return parallel(pool.map((m) => () => agent(
    hdr('refute', `f${i}`, m) +
`You are a skeptical verifier on the audit at ${ROOT}.

*** RULE #1: READ-ONLY. Do NOT edit any file. Do NOT run build, test or lint tooling. ***

A DIFFERENT model reported this ${String(f.severity).toUpperCase()} finding against "${f.source}":
  dimension: ${f.dimension}
  file: ${f.file}${f.line ? ':' + f.line : ''}
  claim: ${f.issue}
  proposed remediation: ${f.remediation || '(fixed in place)'}
  the author marked this as ${f.proved_by_reading === false ? 'INFERRED, not proven by reading' : 'proven by reading'}

YOUR JOB IS TO REFUTE IT. Open that file, read the surrounding code and its callers, and check:
is the claim actually true? Is the severity justified? Does another layer already mitigate it —
a guard upstream, a type invariant, a permission gate, a test that pins it?

Audit agents already applied fixes to this tree, so it may ALREADY BE FIXED. If the code now
reads correctly, set already_fixed=true and refuted=true — it is not a current defect.

Be rigorous, not contrarian. If it is real and correctly rated, refuted=false. If real but
overstated, refuted=false with a corrected_severity. Set refuted=true only when the code
genuinely does not support the claim. Cite what you read; an unsupported verdict is worthless
in either direction.`,
    { label: `refute:${i}:${m}`, phase: 'Adversarial', schema: VERDICT_SCHEMA, model: m, effort: 'high' }
  ).then((v) => (v ? { ...v, model: m } : null)))).then((vs) => {
    const votes = vs.filter(Boolean)
    if (!votes.length) return { finding: f, verdicts: [], agreement: 'unjudged', real: null }
    const cross = votes.filter((v) => v.model !== f.found_by_model)
    const basis = cross.length ? cross : votes
    const nReal = basis.filter((v) => !v.refuted).length
    const real = nReal * 2 > basis.length ? true : (nReal * 2 < basis.length ? false : null)
    const agreement = basis.length === 1 ? (real ? 'single-verifier-real' : 'single-verifier-refuted')
      : nReal === basis.length ? 'unanimous-real'
      : nReal === 0 ? 'unanimous-refuted'
      : real === null ? 'contested' : (real ? 'majority-real' : 'majority-refuted')
    const sev = basis.map((v) => v.corrected_severity).filter(Boolean).sort()
    return { finding: f, verdicts: votes, agreement, real,
             cross_model: cross.length > 0,
             corrected_severity: sev.length ? sev[sev.length >> 1] : null }
  })
}))).filter(Boolean)

const survived = judged.filter((j) => j.real === true || j.agreement === 'contested')
const killed = judged.filter((j) => j.real === false)
const unverified = severeAll.slice(severe.length).map((f) => ({ finding: f, agreement: 'unverified', real: null }))
log(`Phase 6: ${survived.length} survived (${judged.filter((j) => j.agreement === 'contested').length} contested), ` +
    `${killed.length} refuted by cross-model review, ${unverified.length} unverified`)

// ── Phase 7 — the blind three-model calibration panel ────────────────────────
phase('Panel')

const PANEL_SCHEMA = {
  type: 'object',
  required: ['scores', 'rationales', 'headline'],
  properties: {
    scores: { type: 'object', properties: SCORE_PROPS },
    rationales: {
      type: 'object',
      properties: DIMENSIONS.reduce((a, d) => (a[d] = { type: 'string' }, a), {}),
      description: '2-3 sentences of EVIDENCE per dimension. Name file:line or the mechanism.',
    },
    headline: { type: 'string', description: 'The single most important thing about this codebase, in one sentence' },
    worst_dimension: { type: 'string' },
    above_92_justification: { type: 'string', description: 'Required if you scored anything above 92: which 100-anchor mechanism earns it' },
  },
}

// One evidence bundle, identical for all three panelists. Capped so it fits, and the caps are
// logged rather than hidden.
const bundle = {
  workspace: { root: ROOT, languages: GATE.languages, total_loc: GATE.total_loc,
               total_files: GATE.total_files, llm_surface: (GATE.llm_surface || {}).present,
               commit: RUN.commit },
  coverage: { units: UNITS.length, files_owned: UNITS.reduce((n, u) => n + (u.n_files || 0), 0) },
  gate: verify,
  baseline: BASELINE,
  unit_scores: unitResults.map((r) => ({ unit: r.unit, scores: r.scores, notes: r.score_notes,
                                         n_findings: (r.findings || []).length })),
  lens_reports: [...xcutResults, ...sweepResults].map((r) => ({
    lens: r.lens, pass: r.pass, scores: r.scores, summary: r.summary,
    score_notes: r.score_notes, strengths: (r.strengths || []).slice(0, 6),
    findings: (r.findings || []).slice(0, 14).map((f) => ({ sev: f.severity, dim: f.dimension,
      file: f.file, issue: f.issue, proved: f.proved_by_reading })),
  })),
  confirmed_findings: survived.map((j) => ({ sev: j.corrected_severity || j.finding.severity,
    dim: j.finding.dimension, file: j.finding.file, issue: j.finding.issue,
    agreement: j.agreement })),
  refuted_count: killed.length,
  unverified_count: unverified.length,
  fix_review: fixReviews.map((f) => ({ unit: f.unit, verdict: f.verdict,
    n_bad: (f.bad_fixes || []).length })),
  prior_findings_recheck: regressionResults.map((r) => ({ id: r.id, status: r.status,
    quality: r.quality, test_pinned: r.test_pinned })),
}

const panelVotes = (await parallel(PANEL.map((m) => () => agent(
  hdr('panel', `score-${m}`, m) +
`You are an independent scoring panelist on a $100,000 engineering audit of ${ROOT}.

*** RULE #1: READ-ONLY. Do NOT edit any file. Do NOT run build, test or lint tooling. ***

Two other frontier models are scoring the SAME evidence independently and CANNOT see your
answer, and you cannot see theirs. The reported score is the MEDIAN of the three and the SPREAD
is published as a confidence signal. So: do not hedge toward the middle to look safe, and do not
posture. Score what the evidence supports. If you are the outlier and you are right, the spread
is the signal that makes the reader look closer — that is the design working.

You have NOT been shown any prior score for this workspace, deliberately. Do not speculate
about one; anchoring on a guess is the failure mode this blindness exists to prevent.

=== EVIDENCE BUNDLE ===
${JSON.stringify(bundle)}

You may read files in the workspace to resolve a disagreement between reports or to sanity-check
a number, and you should where a dimension's evidence looks thin.
${CALIBRATION}
Score all 17 dimensions and give a rationale for EACH that names evidence — a file:line or a
named mechanism. A rationale that restates the score in words is a failure. Where the evidence
for a dimension is thin, say so and score it lower rather than guessing high.`,
  { label: `panel:${m}`, phase: 'Panel', schema: PANEL_SCHEMA, model: m, effort: 'high' }
).then((r) => (r ? { model: m, ...r } : null))))).filter(Boolean)

const panelScores = {}
const panelSpread = {}
for (const d of DIMENSIONS) {
  const xs = panelVotes.map((v) => (v.scores || {})[d]).filter((x) => typeof x === 'number' && x > 0)
  panelScores[d] = median(xs)
  panelSpread[d] = xs.length > 1 ? Math.max(...xs) - Math.min(...xs) : 0
}
const contestedDims = DIMENSIONS.filter((d) => panelSpread[d] >= 10)
log(`Phase 7: ${panelVotes.length}/3 panelists scored; median overall ` +
    `${weightedOverall(panelScores).toFixed(2)}; ${contestedDims.length} contested dimension(s)` +
    (contestedDims.length ? `: ${contestedDims.join(', ')}` : ''))

// ── Phase 8 — independent second opinion ─────────────────────────────────────
// The highest-value cross-model step observed on prior runs: it moved three scores, caught
// that the wrong tree had been scored, and caught fresh breakage on the default branch.
phase('Second opinion')

const CRITIQUE_SCHEMA = {
  type: 'object',
  required: ['verdict', 'miscalibrated', 'missed', 'disputed'],
  properties: {
    verdict: { type: 'string', description: 'Your own one-paragraph verdict on this codebase' },
    miscalibrated: { type: 'array', items: { type: 'object',
      required: ['dimension', 'direction', 'argued_score', 'why'],
      properties: { dimension: { type: 'string' },
        direction: { type: 'string', enum: ['too_high', 'too_low'] },
        argued_score: { type: 'integer' }, why: { type: 'string' } } } },
    missed: { type: 'array', items: { type: 'string' }, description: 'Important things the audit did not look at' },
    disputed: { type: 'array', items: { type: 'object', required: ['claim', 'why'],
      properties: { claim: { type: 'string' }, why: { type: 'string' } } } },
    agree_with: { type: 'array', items: { type: 'string' } },
    process_critique: { type: 'string', description: 'What is wrong with how this audit was conducted' },
  },
}

const CRITIC_MODELS = CONF.critics >= 3 ? PANEL : ['fable']  // not the synthesis model
const critiques = (await parallel(CRITIC_MODELS.map((m) => () => agent(
  hdr('critique', `second-opinion-${m}`, m) +
`You are an independent senior reviewer. An audit of ${ROOT} was just produced by
${UNITS.length} unit agents, ${LENS_JOBS.length} cross-cutting lenses, cross-model refutation of
every serious finding, and a three-model blind scoring panel.

Your job is a SECOND OPINION. **Do not restate the report — critique it.**

*** RULE #1: READ-ONLY. Do NOT edit any file. Do NOT run build, test or lint tooling.
You MAY read any file in the workspace, and you SHOULD spot-check any claim you doubt. ***

Challenge scores that look miscalibrated IN EITHER DIRECTION — an audit that is too harsh is as
wrong as one that is too kind, and graders drift harsh when they are trying to look rigorous.
Name anything important the audit did not examine. Dispute any finding whose reasoning does not
hold. Then give your own verdict.

Be specific and direct. Vague agreement is worthless; so is contrarianism for its own sake.

=== THE PANEL'S SCORECARD (median of three blind panelists) ===
${JSON.stringify(DIMENSIONS.map((d) => ({ dimension: d, weight: WEIGHTS[d], score: panelScores[d],
  spread: panelSpread[d], contested: panelSpread[d] >= 10,
  rationale: (panelVotes[0] && panelVotes[0].rationales || {})[d] })))}

WEIGHTED OVERALL: ${weightedOverall(panelScores).toFixed(2)}
PANELIST HEADLINES: ${JSON.stringify(panelVotes.map((v) => ({ model: v.model, headline: v.headline, worst: v.worst_dimension })))}

=== CONFIRMED RISKS (survived cross-model refutation) ===
${JSON.stringify(survived.slice(0, 30).map((j) => ({ sev: j.corrected_severity || j.finding.severity,
  dim: j.finding.dimension, file: j.finding.file, issue: j.finding.issue, agreement: j.agreement })))}

=== GATE ===
${JSON.stringify(verify)}

=== KNOWN LIMITS OF THIS AUDIT (do not merely repeat these; test whether they mattered) ===
- ${unverified.length} severe findings went unverified at depth=${DEPTH}.
- ${killed.length} findings were refuted and excluded; a wrong refutation hides a real defect.
- The panel shares training lineage, so agreement is weaker evidence than it appears.
- Score the commit ${RUN.commit || '(unrecorded)'}: confirm the audit scored the tree it claims to have scored.`,
  { label: `second-opinion:${m}`, phase: 'Second opinion', schema: CRITIQUE_SCHEMA,
    model: m, effort: 'high' }
).then((r) => (r ? { model: m, ...r } : null))))).filter(Boolean)

log(`Phase 8: ${critiques.length} independent critique(s); ` +
    `${critiques.reduce((n, c) => n + (c.miscalibrated || []).length, 0)} score challenges, ` +
    `${critiques.reduce((n, c) => n + (c.disputed || []).length, 0)} disputed findings`)

// ── Phase 9 — synthesis ──────────────────────────────────────────────────────
phase('Synthesis')

const SYNTH_SCHEMA = {
  type: 'object',
  required: ['overall_score', 'dimension_scores', 'executive_summary', 'top_strengths', 'top_risks', 'roadmap'],
  properties: {
    overall_score: { type: 'integer', minimum: 0, maximum: 100 },
    dimension_scores: {
      type: 'object',
      properties: DIMENSIONS.reduce((p, d) => {
        p[d] = { type: 'object', required: ['score', 'rationale'],
          properties: {
            score: { type: 'integer', minimum: 0, maximum: 100 },
            grade: { type: 'string' },
            rationale: { type: 'string', description: '2-4 sentences of evidence for the ABSOLUTE score' },
            delta_rationale: { type: 'string', description: 'Why it moved, or what would have had to change and did not' },
            panel_resolution: { type: 'string', description: 'Required where the panel was contested: which way you went and why' },
            evidence: { type: 'array', items: { type: 'string' } },
          } }
        return p
      }, {}),
    },
    executive_summary: { type: 'string', description: '4-6 paragraphs for a technical founder' },
    remediation_verdict: { type: 'string', description: 'Judge the QUALITY of work since last time: root-cause or symptomatic, did any fix introduce a defect, converging or churning?' },
    second_opinion_response: { type: 'string', description: 'Where you accepted the critic and where you overruled it, and why' },
    top_strengths: { type: 'array', items: { type: 'object', properties: { title: { type: 'string' }, detail: { type: 'string' } } } },
    top_risks: { type: 'array', items: { type: 'object',
      properties: { title: { type: 'string' }, severity: { type: 'string' }, dimension: { type: 'string' },
        file: { type: 'string' }, detail: { type: 'string' }, remediation: { type: 'string' },
        agreement: { type: 'string' } } } },
    roadmap: { type: 'array', items: { type: 'object',
      properties: { horizon: { type: 'string', enum: ['now', 'next', 'later'] }, item: { type: 'string' },
        why: { type: 'string' }, effort: { type: 'string' }, dimension: { type: 'string' } } } },
    scoring_methodology: { type: 'string' },
    audit_limitations: { type: 'string', description: 'What this audit could NOT determine. Be specific.' },
  },
}

const synthesis = await agent(
  hdr('synthesis', 'lead', 'opus') +
`You are the lead partner writing the final scorecard of a $100,000 engineering audit of ${ROOT}.
Your job is CALIBRATION and SYNTHESIS, not new discovery.

*** RULE #1: Do NOT edit any file. Do NOT run build, test or lint tooling. You MAY read files to
resolve a disagreement or sanity-check a score, and you MUST do so for every contested
dimension. ***

${PRIOR ? `=== PRIOR SCORECARD (you are the ONLY phase that has seen this) ===\n${JSON.stringify(PRIOR)}` : '(No prior audit — this is round 1.)'}

${regressionResults.length ? `=== DID THE PRIOR RECOMMENDATIONS LAND? (independent verification) ===\n${JSON.stringify(regressionResults)}` : ''}

=== THE BLIND PANEL: three frontier models, same evidence, no contact ===
${JSON.stringify(DIMENSIONS.map((d) => ({
  dimension: d, weight: WEIGHTS[d], median: panelScores[d], spread: panelSpread[d],
  contested: panelSpread[d] >= 10,
  votes: panelVotes.map((v) => ({ model: v.model, score: (v.scores || {})[d],
                                  why: (v.rationales || {})[d] })),
})))}

=== SECOND OPINION(S) — an independent critic that could see the scorecard ===
${JSON.stringify(critiques)}

=== CONFIRMED RISKS (survived cross-model refutation) ===
${JSON.stringify(survived.map((j) => ({ source: j.finding.source, sev: j.corrected_severity || j.finding.severity,
  dim: j.finding.dimension, file: j.finding.file, issue: j.finding.issue,
  remediation: j.finding.remediation, agreement: j.agreement, cross_model: j.cross_model,
  found_by: j.finding.found_by_model,
  why_real: (j.verdicts || []).filter((v) => !v.refuted).map((v) => `${v.model}: ${v.reasoning}`) })))}

=== REFUTED (excluded — a different model read the code and disagreed) ===
${JSON.stringify(killed.slice(0, 40).map((j) => ({ file: j.finding.file, issue: j.finding.issue,
  already_fixed: (j.verdicts || []).some((v) => v.already_fixed),
  why: (j.verdicts || []).map((v) => `${v.model}: ${v.reasoning}`).slice(0, 2) })))}

${unverified.length ? `=== UNVERIFIED (${unverified.length} severe findings exceeded the depth cap — treat as UNCONFIRMED) ===\n${JSON.stringify(unverified.slice(0, 20).map((j) => ({ file: j.finding.file, issue: j.finding.issue })))}` : ''}

=== CROSS-MODEL FIX REVIEW ===
${JSON.stringify(fixReviews.map((f) => ({ unit: f.unit, verdict: f.verdict, author: f.author_model,
  reviewer: f.reviewer_model, bad: (f.bad_fixes || []).slice(0, 4) })))}

=== GATE ===
${JSON.stringify(verify)}
Pre-audit baseline: ${BASELINE}

=== COVERAGE ===
${UNITS.length} unit agents owned ${UNITS.reduce((n, u) => n + (u.n_files || 0), 0)} of
${GATE.total_files} source files. Commit scored: ${RUN.commit || '(unrecorded)'}.

BINDING RULES:
- Start from the panel MEDIAN. You may move a dimension off it, but every move needs a reason
  that cites evidence, and for a contested dimension (spread >= 10) you must READ CODE and put
  your resolution in panel_resolution.
- The second opinion is advisory, not binding — but you must respond to each of its score
  challenges in second_opinion_response, saying where you accepted it and where you overruled it.
- The bar is ABSOLUTE (100 = the Rust project; 90-95 = ripgrep/tokio/SQLite tier). A dimension
  rises only if the tree is genuinely better, never because effort was expended. Some
  dimensions may legitimately FALL, and saying so is the point of the exercise.
- A fix that closes the reported example but leaves the defect CLASS open does NOT earn
  root-cause credit. The regression-check evidence tells you which is which.
- If remediation introduced NEW defects, that is a real cost and must show in the scores.
- Lens scores are the primary signal per dimension; per-unit scores modulate by a few points.
  Ignore every 0.
- Weight units by product significance, not line count.
- top_risks draws ONLY from confirmed findings. Never from refuted ones. Mark any contested or
  unverified item as such in its agreement field.
- OVERALL is the weighted mean over the 17 dimensions using EXACTLY these weights — invent
  nothing: ${JSON.stringify(WEIGHTS)}. It is recomputed independently from your dimension_scores
  and any discrepancy is treated as an error.
- Every dimension needs a delta_rationale naming the specific work that moved it, or naming what
  would have had to change and did not. "Improved generally" is unacceptable.
- audit_limitations must be honest and specific: what could this audit NOT determine?
- The executive summary must be readable by a technical founder: direct, specific, no filler.`,
  { label: 'synthesis', phase: 'Synthesis', schema: SYNTH_SCHEMA, model: 'opus', effort: 'high' }
)

const finalScores = {}
for (const d of DIMENSIONS) {
  const c = synthesis && synthesis.dimension_scores && synthesis.dimension_scores[d]
  if (c && typeof c.score === 'number') finalScores[d] = c.score
}
const computed = weightedOverall(finalScores)
const priorComputed = PRIOR ? weightedOverall(PRIOR) : null
log(`Synthesis: agent said ${synthesis && synthesis.overall_score}; recomputed ${computed.toFixed(2)}` +
    (priorComputed ? ` (prior ${priorComputed.toFixed(2)}, delta ${(computed - priorComputed).toFixed(2)})` : ''))

return {
  run: { ...RUN, root: ROOT, depth: DEPTH, panel: PANEL,
         units: UNITS.length, lens_jobs: LENS_JOBS.length,
         files_owned: UNITS.reduce((n, u) => n + (u.n_files || 0), 0),
         total_files: GATE.total_files, total_loc: GATE.total_loc,
         languages: GATE.languages, fix_authority: GATE.fix_authority,
         llm_surface: (GATE.llm_surface || {}).present },
  regressionResults,
  unitResults,
  fixReviews,
  xcutResults,
  sweepResults,
  verify,
  findings: {
    total: allFindings.length,
    severe: severeAll.length,
    confirmed: survived.map((j) => ({ ...j.finding, agreement: j.agreement,
      corrected_severity: j.corrected_severity, cross_model: j.cross_model,
      verdicts: (j.verdicts || []).map((v) => ({ model: v.model, refuted: v.refuted,
        confidence: v.confidence, reasoning: v.reasoning })) })),
    refuted: killed.map((j) => ({ ...j.finding, agreement: j.agreement,
      verdicts: (j.verdicts || []).map((v) => ({ model: v.model, refuted: v.refuted,
        already_fixed: v.already_fixed, reasoning: v.reasoning })) })),
    unverified: unverified.map((j) => j.finding),
  },
  panel: { votes: panelVotes, median: panelScores, spread: panelSpread, contested: contestedDims },
  critiques,
  synthesis,
  scoring: {
    prior: PRIOR, prior_overall_computed: priorComputed,
    panel_median: panelScores, panel_overall_computed: Number(weightedOverall(panelScores).toFixed(2)),
    final: finalScores,
    final_overall_computed: Number(computed.toFixed(2)),
    final_overall_rounded: Math.round(computed),
    agent_claimed: synthesis && synthesis.overall_score,
    weights: WEIGHTS,
    // Full precision. Subtracting rounded values has already produced one wrong number
    // in a shipped report (operations.md §15).
    delta_precise: priorComputed === null ? null : Number((computed - priorComputed).toFixed(2)),
    deltas: PRIOR ? DIMENSIONS.reduce((a, d) => {
      if (typeof finalScores[d] === 'number' && typeof PRIOR[d] === 'number') a[d] = finalScores[d] - PRIOR[d]
      return a
    }, {}) : null,
  },
}
