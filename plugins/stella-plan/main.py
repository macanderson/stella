#!/usr/bin/env python3
"""`stella-plan` — the pipeline's plan stage, as a plugin.

Standard library only, deliberately — the same rule `plugins/stella-research`
ships under (`doc:pipeline-as-plugins` §9 rule 3: "if a plugin CANNOT be
written without an SDK, the protocol is too complicated"). `main.py` is the
whole program.

# The wire

`crates/stella-plugin/src/wire.rs` and `crates/stella-plugin/src/host_call.rs`
are the contract; these are its shapes as a plugin author meets them. The
host spawns `[runtime].argv` directly — no shell — writes one JSON request on
stdin and reads the response from stdout:

    {"point": "before_turn", "body": {...BeforeTurnRequest}}
 -> {"point": "before_turn", "body": {...BeforeTurnResponse}}

At the one stage this plugin contributes at (`plan`), one host call happens
in between:

    host   {"point": "before_turn", "body": {...}}
 -> plugin {"call": "child_turn", "id": 1, "args": {"role": "planner", "instruction": "..."}}
 -> host   {"result": 1, "ok": {"role": "planner", "seat": "plan", "report": "...", "completed": true}}
 -> plugin {"point": "before_turn", "body": {"context": [...]}}   <- ends it

Every table denies unknown fields, the envelope included, so this program
does too, at every level — the same discipline `stella-research/main.py`
documents at length and this file does not repeat.

There is no error variant in `BeforeTurnResponse`. A plugin that cannot
answer *fails* — non-zero exit, one line on stderr — and the host runs the
turn without the contribution.

# What it contributes, and why it is only ever a message

Everything this plugin returns rides as `VolatileContext`, whose only exit on
the host side is `into_message` — a *user* message after the byte-stable
system-prompt prefix (invariant 7). See `stella-research/main.py`'s module
doc for why that is enforced in the type rather than merely asked for.

# What it does not do, and what replaced it

The built-in stage this extracts — deleted with `stella-pipeline` in #3865 —
was `crates/stella-pipeline/src/plan.rs` (the pure half — `build_planner_prompt`,
`parse_plan`) plus `crates/stella-pipeline/src/pipeline/plan_stage.rs` (the I/O
half — the model call, the one bounded JSON-repair retry, absolute-path
resolution). This
plugin cannot make that model call itself: `doc:wrapper-socket` §7 hands a
plugin no engine, no provider and no credential. Until #3562 it also had no
way to *ask* for one; that gap is closed by the `child_turn` host call
(`doc:turn-loop-wrappers` §9.3) reaching a real dispatcher on the `stella run`
door this plugin was tested against.

**What the planner prompt this plugin builds leaves out, and why**:
`build_planner_prompt(goal, recall, research, repo_structure, revision)`
takes four inputs beside the goal. None of them cross this wire today:

  - `recall` — this plugin declares no `recall` call (see `plugin.toml`'s
    header: asking for both `recall` and `child_turn` in one `before_turn`
    would spend two ceilings for one stage, and `plugins/stella-research`
    already reads recall — running both together under one `--pipeline`
    selection is blocked structurally, #3801).
  - `research` — `ResearchFinding{question, answer}` has no wire
    representation at all; `stella-research`'s own findings ride as prose
    `VolatileContext`, not a typed value a second plugin could re-read.
  - `repo_structure` — no `BeforeTurnRequest` field carries it, and this
    plugin declares no `read_file`/`search` capability to approximate it
    itself (that would measure a second change alongside the one this
    extraction is meant to isolate) — #3562 item 2.
  - `revision` — the `--revise` flag's note has no wire representation
    either — #3562 item 3.

So the prompt this plugin sends is honestly narrower: goal only. The fixed
instruction block is copied byte-for-byte from `PLANNER_INSTRUCTIONS`
(`crates/stella-pipeline/src/plan.rs`) because that text is the output
contract `parse_plan` depends on, and drifting it here would make this
plugin's parser and the model's actual instructions two different programs.

**What the parsed plan does NOT become**: a typed, host-executed per-step
turn loop. `BeforeTurnResponse` carries no `Vec<PlanStep>` field and no
per-step mechanism (`crates/stella-plugin/src/wire.rs`'s only fields are
`context`, `role`, `scope`, `publish`) — `crates/stella-pipeline/src/pipeline/plan_steps.rs`'s
one-user-message-per-step walk is a host mechanism this plugin cannot drive.
So the parsed steps ride as one prose `VolatileContext` the worker reads
before its own turn, weaker than the built-in stage's execution loop and
honestly labelled as such in the text it contributes (#3562 item 4).

**`scope` is always empty.** `BeforeTurnResponse.scope` is "workspace-relative
paths the wrapper believes the turn should stay within" — advisory input to
the host's own scoping. This plugin has no candidate grant and reads no
workspace, so it has nothing structural to name there; and `PLANNER_INSTRUCTIONS`
itself tells the child turn never to guess a literal path (#2932's lesson),
so inventing scope entries by scanning the plan's prose for path-looking
tokens would be re-introducing exactly the failure that instruction exists to
avoid. "Any scope entries" this plugin could honestly contribute is zero,
every time, and that is a decision, not an omission.

# child_turn: a plugin may ask, never make the call itself

Every way the ask can fail to produce a report — the host offers no channel
(`unavailable`), the manifest declares no such role intent (`undeclared`),
the role resolves to the worker's own seat (`forbidden` — not this plugin's
case, since `planner` resolves to `ModelCallRole::Plan`, but the code path is
shared with every other `child_turn` asker), the allowance is spent
(`allowance-spent`), the host tried and the turn failed (`failed`), or nobody
answered — degrades to the empty contribution rather than a fabricated plan,
and says so on stderr. A completed child turn whose report simply does not
parse as a plan (no repair retry is affordable at `max_calls = 1`) degrades
the same way. The one case that is a **refusal** rather than a degradation is
an `ok` answer shaped differently than `ChildTurnResult` — that is the host
disagreeing with this plugin about the contract, not an ordinary outcome.

# Every capability arrives in the request

`goal` is the only thing this plugin reads from the request body. `candidate`,
when present, is validated for shape (deny-unknown-fields) and never read —
this plugin opens no file and asks the host for none. `published` signals are
validated for shape and ignored — the `if = "plans"` condition on the `plan`
stage in `plugin.toml` already means the host only asks this plugin to
contribute when triage said this turn plans; there is nothing left for this
program to re-decide.
"""

import json
import sys

# The version every message on this wire carries (`wire::PROTOCOL_VERSION`).
PROTOCOL_VERSION = 1

# The one point this plugin answers, matching `[loop].points`.
POINT = "before_turn"

# The one stage this plugin contributes at. Every other declared stage gets
# an empty contribution, byte-identical to a host with no plugin installed.
PLAN_STAGE = "plan"

# The fields each table on the request declares. Anything else is a typo.
BEFORE_TURN_REQUEST_FIELDS = {
    "protocol_version",
    "wrapper",
    "stage",
    "round",
    "goal",
    "candidate",
    "published",
}
CANDIDATE_GRANT_FIELDS = {"handle", "root", "test"}
PUBLISHED_SIGNAL_FIELDS = {"signal", "value"}

# `StageName`, closed — mirrored from `stella-research/main.py` verbatim. A
# stage name this plugin does not recognise is indistinguishable from "some
# stage that is not plan", which would silently contribute nothing to a turn
# this plugin was installed to plan.
STAGE_NAMES = (
    "triage",
    "recall",
    "research",
    "plan",
    "scope",
    "execute",
    "witness",
    "verify",
    "verdict",
    "reflect",
    "contextwrite",
    "complete",
)

# ── The host-call channel (`doc:wrapper-socket` §6b) ─────────────────────────
CALL_CHILD_TURN = "child_turn"
CALL_ANSWER_FIELDS = {"result", "ok", "err"}
# `host_call::ChildTurnResult` field for field — required keys are `role`,
# `seat`, `report`, `completed`, and every one of them is disjoint from
# `RecallResult`'s `frames`, per `HostCallOk`'s untagged-variant rule.
CHILD_TURN_OK_FIELDS = {"role", "seat", "report", "completed"}
CHILD_TURN_ANSWER = "the child turn answer"

# The `[roles.<name>]` key this plugin declares and the only role intent it
# ever names. Resolves to `ModelCallRole::Plan` under the host's
# `default_seats()`.
ROLE_INTENT = "planner"

# The planner's fixed instruction block, copied byte-for-byte from
# `PLANNER_INSTRUCTIONS` in `crates/stella-pipeline/src/plan.rs`. This is the
# output contract `parse_plan` below depends on — a model told anything else
# is a model this program cannot honestly parse.
PLANNER_INSTRUCTIONS = (
    "You are the planner for a coding agent. Produce a short ordered plan of "
    "concrete steps to accomplish the goal. Respond with a JSON array of "
    'step strings, e.g. ["step one", "step two"]. Keep it minimal — the '
    "fewest steps that fully accomplish the goal. You have no tool access and "
    "cannot see the real repository: state each step as an OUTCOME (what "
    "should be true when it is done), never as a literal absolute path or a "
    "specific shell command — a path or command you have not verified exists "
    "may be wrong, and the worker that carries out the step can see the tree "
    "and choose one correctly."
)


class Refusal(Exception):
    """The plugin cannot answer. Exits non-zero; the host runs without it."""


def refuse(reason):
    raise Refusal(reason)


def deny_unknown(table, allowed, subject="the request"):
    """Refuse a table carrying a key the contract does not declare.

    One message shape for every table, matching `stella-research/main.py`'s
    own `deny_unknown` and the reference plugins in `stella-examples`.
    """
    unknown = sorted(set(table) - allowed)
    if unknown:
        refuse(
            "{} denies unknown fields; got {}".format(subject, ", ".join(unknown))
        )
    return table


def read_json(stream):
    """The next JSON document on `stream`, or `None` when there is not one.

    Line-oriented *and* whole-document tolerant — see
    `stella-research/main.py::read_json` for why both hosts this program
    meets are legitimate. Identical implementation, kept local so this
    program has no import beyond the standard library and no dependency on
    its sibling plugin.
    """
    decoder = json.JSONDecoder()
    buffered = ""
    while True:
        text = buffered.strip()
        if text:
            try:
                document, end = decoder.raw_decode(text)
            except ValueError:
                pass  # Not yet complete, or never will be. EOF decides which.
            else:
                if not text[end:].strip():
                    return document
        line = stream.readline()
        if not line:
            return None
        buffered += line


def write_json(stream, document):
    """One JSON document, on one line, flushed — the flush is load-bearing,
    see `stella-research/main.py::write_json`."""
    stream.write(json.dumps(document) + "\n")
    stream.flush()


def report(reason):
    """Say on stderr what this stage degraded to, and why — never silent."""
    sys.stderr.write("stella-plan: {}\n".format(reason))


def read_request(stdin):
    """Decode `{"point": ..., "body": ...}` and return the `before_turn` body."""
    envelope = read_json(stdin)
    if not isinstance(envelope, dict):
        refuse("stdin was not a single JSON object")
    if set(envelope) - {"point", "body"}:
        refuse("the request envelope carried a field outside {point, body}")

    point = envelope.get("point")
    if point != POINT:
        refuse(
            "this plugin answers {} only; the host asked for {}".format(
                POINT, json.dumps(point)
            )
        )

    body = envelope.get("body")
    if not isinstance(body, dict):
        refuse("the request envelope carried no object body")
    deny_unknown(body, BEFORE_TURN_REQUEST_FIELDS)

    version = body.get("protocol_version")
    # `isinstance(True, int)` is True in Python, so a JSON `true` would
    # otherwise compare equal to version 1. It does not, and this says so.
    if isinstance(version, bool) or version != PROTOCOL_VERSION:
        refuse(
            "this plugin speaks wrapper protocol_version {}; the host asked for {}".format(
                PROTOCOL_VERSION, json.dumps(version)
            )
        )

    stage = body.get("stage")
    if stage not in STAGE_NAMES:
        refuse(
            "StageName is a closed set {{{}}}; got {}".format(
                ", ".join(STAGE_NAMES), json.dumps(stage)
            )
        )

    candidate = body.get("candidate")
    if candidate is not None:
        if not isinstance(candidate, dict):
            refuse("candidate must be a CandidateGrant object")
        deny_unknown(candidate, CANDIDATE_GRANT_FIELDS)
        # Never read further: this plugin opens no file under any root the
        # host grants it. Validated for shape and nothing else.

    for published in body.get("published", []) or []:
        if not isinstance(published, dict):
            refuse("published must be an array of PublishedSignal objects")
        deny_unknown(published, PUBLISHED_SIGNAL_FIELDS)

    goal = body.get("goal")
    if not isinstance(goal, str):
        refuse("the request carried no goal")
    return body


class HostCalls:
    """This plugin's end of the host-call conversation (§6b).

    Identical shape to `stella-research/main.py::HostCalls` — see that file's
    docstring for the outcome-vs-shape distinction this class enforces. Kept
    as a separate copy rather than a shared import for the same reason
    `read_json`/`write_json` are: each plugin is a single self-contained file,
    which is the property that lets a conformance harness diff one against
    the wire types without following an import graph.
    """

    def __init__(self, stdin, stdout):
        self.stdin = stdin
        self.stdout = stdout
        self.asked = 0

    def ask(self, call, args):
        """Make one host call and return its `ok` payload, or `None`.

        `None` covers every way a call can fail to produce one: refused for
        any of the six closed reasons, answered for a different call, or not
        answered at all. It does not branch on which refusal it was — every
        one of them leaves this stage with nothing to contribute, and the
        *reason* is only ever reported, on stderr, for a human to read.
        """
        self.asked += 1
        call_id = self.asked
        write_json(self.stdout, {"call": call, "id": call_id, "args": args})

        answer = read_json(self.stdin)
        if not isinstance(answer, dict):
            report("the host did not answer the {} call".format(call))
            return None
        answered = answer.get("result")
        if isinstance(answered, bool) or answered != call_id:
            report(
                "the host answered call {} while this plugin asked {}".format(
                    json.dumps(answered), call_id
                )
            )
            return None
        deny_unknown(answer, CALL_ANSWER_FIELDS, CHILD_TURN_ANSWER)
        ok = answer.get("ok")
        failure = answer.get("err")
        if (ok is None) == (failure is None):
            refuse("a host-call answer carries either ok or err, never both")
        if failure is not None:
            report(
                "the host did not serve the {} call: {}".format(call, refusal(failure))
            )
            return None
        if not isinstance(ok, dict):
            refuse("the child turn answer's ok must be an object")
        return ok


def refusal(failure):
    """A failed call's `err`, as the one line this plugin logs about it.

    Identical to `stella-research/main.py::refusal` — see that file for why
    the shape is read leniently here alone.
    """
    if isinstance(failure, dict):
        code = failure.get("refusal")
        detail = failure.get("detail")
        if isinstance(code, str) and isinstance(detail, str):
            return "{}: {}".format(code, detail)
    return json.dumps(failure, sort_keys=True)


def planner_instruction(goal):
    """The `child_turn` instruction this plugin sends — the narrowed planner
    prompt described in this module's docstring.

    `ChildTurnArgs` carries one `instruction: String`, not a separate
    system/user split the way `ManagementPrompt` does — so the fixed
    instructions and the per-call payload are concatenated here, in the same
    order `ManagementPrompt::rendered` would render them, rather than kept
    apart the way the host's own cache-marking wants. There is no cache to
    mark on this side of the call: the host, not this plugin, owns the
    provider request the child turn eventually becomes.
    """
    payload = "## Goal\n{}\n\n## Plan (JSON array of step strings)\n".format(
        goal.strip()
    )
    return PLANNER_INSTRUCTIONS + "\n\n" + payload


def child_turn(host_calls, goal):
    """Ask for one planner-role child turn; return its result or `None`.

    `None` is every degradation §6b names, already reported by `HostCalls.ask`.
    A malformed `ok` payload — the right keys but a value of the wrong shape —
    is not a degradation, and refuses instead: this plugin knows exactly what
    a `ChildTurnResult` looks like, and a host that sends something else has
    not met that contract.
    """
    ok = host_calls.ask(
        CALL_CHILD_TURN,
        {"role": ROLE_INTENT, "instruction": planner_instruction(goal)},
    )
    if ok is None:
        return None
    deny_unknown(ok, CHILD_TURN_OK_FIELDS, CHILD_TURN_ANSWER)
    role = ok.get("role")
    seat = ok.get("seat")
    report_text = ok.get("report")
    completed = ok.get("completed")
    if not isinstance(role, str):
        refuse("the child turn answer carried no role")
    if not isinstance(seat, str):
        refuse("the child turn answer carried no seat")
    if not isinstance(report_text, str):
        refuse("the child turn answer carried no report")
    if not isinstance(completed, bool):
        refuse("the child turn answer's completed must be a boolean")
    return {"role": role, "seat": seat, "report": report_text, "completed": completed}


# ── Plan parsing, ported from crates/stella-pipeline/src/plan.rs ────────────
#
# `parse_plan` is mirrored deliberately, the way `stella-research/main.py`
# mirrors `receipts::render_recall_line`: the built-in stage's own
# `PLANNER_INSTRUCTIONS` promises the model a JSON array (with a plain-list
# fallback), and a plugin that read the answer differently would be a second,
# silently diverging definition of what counts as a plan. There is no golden
# corpus shared between the two languages here (unlike the recall line
# format, which `stella-research`'s goldens pin against Rust's own renderer)
# — `plan.rs`'s own unit tests are the closest thing to one, and this port's
# vectors exercise the same shapes those tests do (bare array, `{"steps":
# [...]}`, prose-wrapped JSON, numbered list, bulleted list).


def strip_list_marker(line):
    """Strip a leading list marker (`1.`, `2)`, `-`, `*`, `•`) from a line,
    returning the remaining text, or `None` if the line is not a list item.
    Ported from `plan.rs::strip_list_marker`."""
    t = line.lstrip()
    for prefix in ("- ", "* ", "• "):
        if t.startswith(prefix):
            return t[len(prefix) :]
    i = 0
    n = len(t)
    saw_digit = False
    # ASCII only, mirroring `plan.rs`'s `char::is_ascii_digit` — Python's
    # `str.isdigit` also accepts non-ASCII numerals, which the Rust parser
    # deliberately does not treat as a list marker.
    while i < n and t[i] in "0123456789":
        saw_digit = True
        i += 1
    if saw_digit and i < n and t[i] in ".)":
        rest = t[i + 1 :]
        if rest[:1] in (" ", "\t"):
            return rest[1:].lstrip()
    return None


def parse_plan_list(text):
    """Fallback plain-text plan parse: numbered or bulleted list items, in
    order. `None` if none are found. Ported from `plan.rs::parse_plan_list`."""
    steps = []
    for line in text.splitlines():
        desc = strip_list_marker(line)
        if desc is not None:
            desc = desc.strip()
            if desc:
                steps.append(desc)
    return steps if steps else None


def balanced_span(text, open_ch, close_ch):
    """The first balanced `open_ch`…`close_ch` span in `text`, string-literal
    aware so a bracket inside a JSON string cannot close it early. Ported
    from `plan.rs::balanced_span`."""
    start = text.find(open_ch)
    if start == -1:
        return None
    depth = 0
    in_string = False
    escaped = False
    for i in range(start, len(text)):
        c = text[i]
        if in_string:
            if escaped:
                escaped = False
            elif c == "\\":
                escaped = True
            elif c == '"':
                in_string = False
            continue
        if c == '"':
            in_string = True
        elif c == open_ch:
            depth += 1
        elif c == close_ch:
            depth -= 1
            if depth == 0:
                return text[start : i + 1]
    return None


def plan_json_steps(parsed, open_ch):
    """The step descriptions `parsed` names, under `PlanJson`'s untagged
    rule, or `None` if `parsed` does not match either shape. Ported from
    `plan.rs`'s `PlanJson`/`StepJson` untagged enums — permissive of extra
    object keys, exactly as serde's derive is for a struct with no
    `deny_unknown_fields`."""
    if open_ch == "[":
        if not isinstance(parsed, list):
            return None
        items = parsed
    else:
        if not isinstance(parsed, dict) or not isinstance(parsed.get("steps"), list):
            return None
        items = parsed["steps"]
    out = []
    for item in items:
        if isinstance(item, str):
            out.append(item)
        elif isinstance(item, dict) and isinstance(item.get("description"), str):
            out.append(item["description"])
        else:
            return None
    return out


def parse_plan_json(text):
    """Strict-JSON plan parse: scans for the first `[`/`{` balanced span and
    tries to decode it as a bare array or a `{"steps": [...]}` object.
    Ported from `plan.rs::parse_plan_json`."""
    for open_ch, close_ch in (("[", "]"), ("{", "}")):
        span = balanced_span(text, open_ch, close_ch)
        if span is None:
            continue
        try:
            parsed = json.loads(span)
        except ValueError:
            continue
        steps = plan_json_steps(parsed, open_ch)
        if steps is None:
            continue
        cleaned = [d.strip() for d in steps if isinstance(d, str)]
        cleaned = [d for d in cleaned if d]
        if cleaned:
            return cleaned
    return None


def parse_plan(text):
    """Parse a plan from a child turn's report. Strict JSON first, then a
    numbered/bulleted list fallback. `None` only when neither yields a
    non-empty ordered list. Ported from `plan.rs::parse_plan`."""
    steps = parse_plan_json(text)
    if steps is not None:
        return steps
    return parse_plan_list(text)


def plan_context(steps, completed):
    """The one contribution this stage makes: the parsed steps, as prose a
    worker reads before its own turn — not a host-executed step loop (see
    module docstring)."""
    lines = ["{}. {}".format(i + 1, step) for i, step in enumerate(steps)]
    header = (
        "## Plan\n\nA planner turn asked to accomplish this goal proposed "
        "these steps, in order. They are sequencing hints, not specs — "
        "earlier steps may already cover a later one, and the goal is the "
        "authority where they disagree:\n"
    )
    text = header + "\n".join(lines)
    if not completed:
        text += (
            "\n\n[the planning turn did not reach a final answer — its "
            "budget or step cap was reached, so this plan may be partial]"
        )
    return {"label": "plan:steps", "text": text}


def contribute(body, host_calls):
    """The context this stage contributes, which may be nothing.

    Nothing is a complete answer and the common case outside `plan`: every
    other declared stage returns `[]`, byte-identical to a host with no
    plugin installed. At `plan`, nothing is also correct whenever the child
    turn could not be run, or ran and produced no parseable plan — a plugin
    with one bounded call cannot afford the built-in stage's own JSON-repair
    retry, so an unparseable answer is reported and dropped rather than
    guessed at.
    """
    if body["stage"] != PLAN_STAGE:
        return []
    result = child_turn(host_calls, body["goal"])
    if result is None:
        return []
    steps = parse_plan(result["report"])
    if not steps:
        report(
            "the planner turn's report contained no parseable plan; "
            "contributing nothing"
        )
        return []
    return [plan_context(steps, result["completed"])]


def main():
    try:
        body = read_request(sys.stdin)
        context = contribute(body, HostCalls(sys.stdin, sys.stdout))
    except Refusal as refusal_reason:
        sys.stderr.write("stella-plan: {}\n".format(refusal_reason))
        return 1

    response = {"protocol_version": PROTOCOL_VERSION}
    if context:
        # Absent rather than empty when there is nothing to say: the host's
        # own `BeforeTurnResponse` skips empty collections when it
        # serializes, so an empty contribution is the same bytes on both
        # sides of the wire.
        response["context"] = context
    write_json(sys.stdout, {"point": POINT, "body": response})
    return 0


if __name__ == "__main__":
    sys.exit(main())
