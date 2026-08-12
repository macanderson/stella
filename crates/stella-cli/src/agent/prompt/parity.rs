//! Structural parity for the shared prompt contracts (#2231).
//!
//! `prompt.rs` declares its cross-cutting contracts — tool steering, scope,
//! measurement — as `macro_rules!` string literals so that **both** static
//! prompts can embed the same bytes: one literal, two `concat!` sites, no
//! second copy to drift from. The catalogue this pattern replaced had drifted
//! in exactly that way (`verify_done` and `ask_user` took comma splices in the
//! pipeline copy where the base copy took an em dash, #450).
//!
//! Each contract was then pinned by its own hand-written test. That covers the
//! contracts that exist and says nothing about the next one: a fifth contract
//! embedded in `SYSTEM_PROMPT` alone, with no test written for it, failed
//! nothing. Confirmed on `main` before this module existed — a throwaway
//! contract added to `SYSTEM_PROMPT` only left `cargo test -p stella-cli --bin
//! stella prompt` at "45 passed; 0 failed". The coupling held by convention.
//!
//! `PIPELINE_SYSTEM_PROMPT` is the half that would go missing quietly: it is
//! what `stella run` sends and what the bench harness exercises, so a contract
//! landing in `SYSTEM_PROMPT` alone is invisible to every measurement the
//! project makes decisions from.
//!
//! # The set is derived, never listed
//!
//! A hardcoded roster of contracts would reproduce the defect one level up —
//! the fifth contract simply joins the set without joining the list. So the
//! authority here is `prompt.rs`'s own source text: [`scan`] reads the
//! `macro_rules!` declarations and the `const … = concat!(` prompt
//! declarations out of it, and the two runtime registries below exist only
//! because a macro cannot be invoked by a name computed at runtime. Both are
//! cross-checked against the scan in both directions, so a registry that falls
//! behind the source is a named failure rather than a check that quietly
//! covers less.
//!
//! # Byte stability is untouched
//!
//! Every value here is a `&'static str` fixed at compile time: the contracts
//! stay `macro_rules!` literals, the prompts stay `concat!` compositions, and
//! this module only reads them. Nothing becomes runtime-computed, so the
//! byte-stable prefix the prompt cache keys on (invariant 7 / L-E8) is exactly
//! what it was. The scan in fact *enforces* one half of that: a prompt
//! rewritten as a runtime `format!` stops parsing as a static prompt and the
//! registry cross-check fails naming it.
//!
//! # What deliberately stayed
//!
//! The per-contract `both_prompts_*` tests in `prompt.rs` are not deleted here.
//! Their embedding assertion is now redundant with this module, but each one's
//! doc comment is the provenance record for its contract and their *claim*
//! assertions guard the literal's content rather than its presence — a
//! different property. Consolidating them is #2237, deliberately sequenced
//! after #2232 lands rather than merged into a `mod tests` block another PR is
//! adding a test to.
//!
//! # Anti-vacuity
//!
//! A check that silently examines nothing reads as a passing gate, which is
//! strictly worse than no check — the same shape as #2182, where a parity
//! check could not run where it mattered. So every way this scan could come up
//! empty is an error with a name: [`ScanError::NoContractsDeclared`],
//! [`ScanError::NoStaticPromptsParsed`], a registry that no longer matches the
//! source, and a contract literal too short for `contains` to prove anything.
//! `the_scan_fails_loudly_rather_than_finding_nothing` drives each one.

use super::{PIPELINE_SYSTEM_PROMPT, SYSTEM_PROMPT};

/// `prompt.rs` read as text, at compile time.
///
/// `include_str!` resolves relative to this file, so renaming or moving either
/// file is a compile error rather than a scan that quietly reads nothing — the
/// strongest available form of the anti-vacuity guard.
const PROMPT_SOURCE: &str = include_str!("../prompt.rs");

/// A line every reader of `prompt.rs` will recognise, asserted before anything
/// else so "the scan parsed nothing" can never be confused with "the scan is
/// pointed at the wrong text".
const SOURCE_ANCHOR: &str = "//! System-prompt assembly and file-tree rendering.";

/// A shared contract shorter than this is empty or a placeholder, and
/// `prompt.contains(it)` proves nothing. Not a style rule — the floor exists
/// only so the verbatim assertion cannot pass vacuously against a hollowed-out
/// literal. The shortest real contract today is several hundred characters.
const MIN_CONTRACT_CHARS: usize = 80;

/// Every shared contract, paired with the bytes its macro expands to.
///
/// The authority on *which* contracts exist is `prompt.rs` itself;
/// `the_registries_match_the_source` fails when this list and the scan differ
/// in either direction. This list exists only because a `macro_rules!` cannot
/// be invoked by a name computed at runtime, and expanding each one here is
/// what lets the check compare bytes rather than source text.
const SHARED_CONTRACTS: &[(&str, &str)] = &[
    ("tool_steering", tool_steering!()),
    ("skill_use", skill_use!()),
    ("scope_discipline", scope_discipline!()),
    ("measurement_discipline", measurement_discipline!()),
    (
        "verification_proportionality",
        verification_proportionality!(),
    ),
    ("faithful_reporting", faithful_reporting!()),
    ("complexity_discipline", complexity_discipline!()),
    ("action_care", action_care!()),
    ("injection_defense", injection_defense!()),
];

/// Every static prompt, paired with its assembled bytes. Same derivation
/// discipline as [`SHARED_CONTRACTS`], and shared with `prompt.rs`'s own test
/// module so there is one list of prompts to keep current rather than two.
pub(super) const STATIC_PROMPTS: &[(&str, &str)] = &[
    ("SYSTEM_PROMPT", SYSTEM_PROMPT),
    ("PIPELINE_SYSTEM_PROMPT", PIPELINE_SYSTEM_PROMPT),
];

/// Why a parity scan of `prompt.rs` could not be completed.
///
/// Each variant is a way the check would otherwise examine *less* than it
/// must. They are errors rather than "nothing to check" precisely because an
/// empty scan reads exactly like a passing gate.
#[derive(Debug, PartialEq, Eq)]
enum ScanError {
    /// `prompt.rs` declares no `macro_rules!` at all: either the shared-literal
    /// pattern was abandoned, or this scan is reading the wrong text.
    NoContractsDeclared,
    /// No static prompt parsed. A prompt is recognised by its declaration being
    /// a compile-time `concat!` — the shape invariant 7 (L-E8) rests on — so
    /// this also fires when one is rewritten as a runtime composition.
    NoStaticPromptsParsed,
}

/// What `prompt.rs`'s source says about its own shared contracts.
#[derive(Debug)]
struct Scan<'a> {
    /// Contract macro names, in declaration order.
    contracts: Vec<&'a str>,
    /// For every static prompt: its constant name, and the body of the
    /// `concat!` that assembles it.
    prompts: Vec<(&'a str, &'a str)>,
}

/// Read the shared-contract structure out of `source`, or say why not.
fn scan(source: &str) -> Result<Scan<'_>, ScanError> {
    let contracts = declared_contracts(source);
    if contracts.is_empty() {
        return Err(ScanError::NoContractsDeclared);
    }
    let prompts = static_prompt_bodies(source);
    if prompts.is_empty() {
        return Err(ScanError::NoStaticPromptsParsed);
    }
    Ok(Scan { contracts, prompts })
}

/// Every `(contract, prompt)` pair the source leaves uncoupled — the defect
/// this module exists to name. Empty is the healthy answer.
fn unembedded<'a>(scan: &Scan<'a>) -> Vec<(&'a str, &'a str)> {
    let mut missing = Vec::new();
    for contract in &scan.contracts {
        let invocation = format!("{contract}!()");
        for (prompt, body) in &scan.prompts {
            if !body.contains(&invocation) {
                missing.push((*contract, *prompt));
            }
        }
    }
    missing
}

/// The names of every top-level `macro_rules!` declared in `source`.
///
/// Every macro in `prompt.rs` is a shared prompt contract, and this treats them
/// as such: a helper macro added there fails the coupling check until it is
/// either embedded in both prompts or moved somewhere it is not mistaken for a
/// contract. That is the fail-closed direction — the alternative is an opt-in
/// marker, which is the convention this module replaces.
fn declared_contracts(source: &str) -> Vec<&str> {
    source.lines().filter_map(contract_name).collect()
}

/// The name declared by a top-level `macro_rules!` line, if this is one.
///
/// Column 0 only. Every contract is declared at the top level, and requiring it
/// keeps prose that merely mentions the keyword out of the scan.
fn contract_name(line: &str) -> Option<&str> {
    let name = line
        .strip_prefix("macro_rules!")?
        .trim()
        .trim_end_matches('{')
        .trim();
    is_ident(name).then_some(name)
}

/// `(name, body)` for every static prompt in `source`, where `body` is the text
/// between the `concat!(` line and the `);` that closes it.
///
/// A body that ends early — were a prompt ever to contain a line opening with
/// `);` — can only *remove* text, so it can turn a coupled contract into a
/// reported failure and never the reverse. This scan is allowed to be wrong
/// only in the direction that shouts.
fn static_prompt_bodies(source: &str) -> Vec<(&str, &str)> {
    let mut prompts = Vec::new();
    let mut offset = 0usize;
    for raw in source.split_inclusive('\n') {
        offset += raw.len();
        let Some(name) = static_prompt_name(raw.trim_end()) else {
            continue;
        };
        let rest = &source[offset..];
        // rustfmt closes a multi-line `concat!` on its own line.
        let Some(end) = rest.find("\n);") else {
            continue;
        };
        prompts.push((name, &rest[..end]));
    }
    prompts
}

/// The constant name declared by a top-level static-prompt line, if this is
/// one: `[pub(crate) ]const NAME: &str = concat!(`.
///
/// The `concat!` is load-bearing rather than incidental syntax. Both prompts
/// are compile-time concatenations precisely so the cached prefix stays
/// byte-stable (invariant 7 / L-E8); a prompt rewritten as a runtime `format!`
/// stops matching here, and the registry cross-check turns that into a named
/// failure rather than a prompt this scan quietly stops covering.
fn static_prompt_name(line: &str) -> Option<&str> {
    let declaration = line
        .strip_prefix("pub(crate) const ")
        .or_else(|| line.strip_prefix("pub const "))
        .or_else(|| line.strip_prefix("const "))?;
    let (name, rest) = declaration.split_once(':')?;
    let name = name.trim();
    (is_ident(name) && rest.trim() == "&str = concat!(").then_some(name)
}

/// Whether `name` is a plain Rust identifier — the shape both a macro name and
/// a constant name take here.
fn is_ident(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The scan of `prompt.rs` itself, or a panic naming what stopped it.
fn source_scan() -> Scan<'static> {
    scan(PROMPT_SOURCE).unwrap_or_else(|error| {
        panic!(
            "the shared-contract scan could not read crates/stella-cli/src/agent/prompt.rs \
             ({error:?}). It examines nothing in this state, which reads as a passing gate — \
             correct the parser in agent/prompt/parity.rs rather than leaving it silent (#2231)"
        )
    })
}

/// Guard the guard: everything below iterates a set this scan produced, so an
/// empty or misdirected scan would pass every one of them vacuously.
///
/// #2182 was exactly a parity check that could not run where it mattered, so
/// these assertions come before the ones that matter.
#[test]
fn the_scan_reads_its_subject() {
    assert!(
        PROMPT_SOURCE.contains(SOURCE_ANCHOR),
        "the text this module scans is not agent/prompt.rs — it does not open with \
         {SOURCE_ANCHOR:?}. Correct the `include_str!` in agent/prompt/parity.rs"
    );
    let scan = source_scan();
    assert!(
        !scan.contracts.is_empty(),
        "no `macro_rules!` contract was found in prompt.rs: either the shared-literal \
         pattern was abandoned (#450) or `contract_name` no longer matches how they are \
         declared"
    );
    assert!(
        scan.prompts.len() >= 2,
        "prompt.rs declares fewer than two static `concat!` prompts ({:?}) — the whole \
         subject of this check is that a contract reaches BOTH, so a scan finding one \
         has stopped being able to see the defect",
        scan.prompts
            .iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>()
    );
}

/// The two runtime registries are the only hardcoded lists in this module, and
/// this is what stops them being hardcoded in the way that matters: the source
/// is the authority and a registry that falls behind it fails by name.
///
/// Checked in both directions. A missing row is the #2231 hole itself — a new
/// contract joining the set without joining the list. A stale row is the
/// mirror: a list that claims to cover something the source no longer has.
#[test]
fn the_registries_match_the_source() {
    let scan = source_scan();
    assert_registry(
        "contract",
        &scan.contracts,
        SHARED_CONTRACTS,
        "prompt/parity.rs SHARED_CONTRACTS",
    );
    let prompts: Vec<&str> = scan.prompts.iter().map(|(name, _)| *name).collect();
    assert_registry(
        "static prompt",
        &prompts,
        STATIC_PROMPTS,
        "prompt/parity.rs STATIC_PROMPTS",
    );
}

/// Both halves of one registry check, so the two calls above read the same.
fn assert_registry(subject: &str, declared: &[&str], registry: &[(&str, &str)], home: &str) {
    let registered: Vec<&str> = registry.iter().map(|(name, _)| *name).collect();
    for name in declared {
        assert!(
            registered.contains(name),
            "prompt.rs declares the {subject} `{name}`, which {home} does not list. \
             Add its row — the list is what lets the check compare bytes, and a {subject} \
             missing from it is checked by nothing (#2231)"
        );
    }
    for name in &registered {
        assert!(
            declared.contains(name),
            "{home} lists the {subject} `{name}`, which prompt.rs no longer declares. \
             Drop the stale row so the registry cannot claim coverage it does not have"
        );
    }
}

/// The coupling itself, at the source level: every shared contract is invoked
/// by every static prompt's `concat!`.
///
/// This is the assertion the four hand-written `both_prompts_*` tests each made
/// for one contract. Read off the source, it holds for the fifth contract too,
/// which is the whole point (#2231).
#[test]
fn every_shared_contract_is_invoked_by_every_static_prompt() {
    let scan = source_scan();
    let missing = unembedded(&scan);
    assert!(
        missing.is_empty(),
        "a shared prompt contract is embedded by only some of the static prompts, so the \
         two prompts have already diverged (#450, #2231). Missing: {}",
        missing
            .iter()
            .map(|(contract, prompt)| format!("`{contract}!()` is not in {prompt}"))
            .collect::<Vec<_>>()
            .join("; ")
    );
}

/// The coupling at the byte level: the contract's expanded text is in the
/// assembled prompt, not merely its invocation in the source.
///
/// Source and bytes are genuinely different questions. A contract could be
/// invoked inside a comment, or the macro could expand to something other than
/// the literal it appears to hold; only the assembled `&'static str` settles
/// what the model is actually sent.
#[test]
fn every_shared_contract_reaches_every_static_prompt_verbatim() {
    for (contract, text) in SHARED_CONTRACTS {
        assert!(
            text.chars().count() >= MIN_CONTRACT_CHARS,
            "the `{contract}` literal is {} characters — too short for `contains` to prove \
             anything, so the assertions below would pass against a hollowed-out contract",
            text.chars().count()
        );
        for (label, prompt) in STATIC_PROMPTS {
            assert!(
                prompt.contains(text),
                "{label} does not embed the shared `{contract}` block verbatim"
            );
        }
    }
}

/// The injection-defense marker clause, checked against the engine's own
/// table rather than prose (#2722).
///
/// The clause turns "be suspicious of instruction-shaped tool output" into a
/// decidable test: engine-injected guidance carries a recognizable marker, so
/// directive text without one is data impersonating the operator. That test is
/// only as sound as the marker list the prompt teaches — and the hand-written
/// prose list had already drifted twice in the quiet direction: the
/// output-limit continuation nudge (itself instruction-shaped) was absent
/// entirely, so the engine's own steering failed the prompt's own test, and
/// the recall marker was described but never spelled. The authority is
/// `stella_core::engine_markers::ENGINE_MARKERS` — the constants the engine
/// actually writes, by definition — so a new marker joining that table fails
/// here by name until both prompts teach it.
#[test]
fn every_engine_marker_is_taught_verbatim_by_every_static_prompt() {
    let markers = stella_core::engine_markers::ENGINE_MARKERS;
    // Anti-vacuity: the table's own tests pin entry shape; this pins that a
    // refactor cannot leave this loop iterating an emptied table.
    assert!(
        markers.len() >= 4,
        "ENGINE_MARKERS lost entries — this check now proves less than it did"
    );
    for marker in markers {
        for (label, prompt) in STATIC_PROMPTS {
            assert!(
                prompt.contains(marker),
                "{label} does not teach the engine marker {marker:?}. The \
                 injection-defense clause tells the model engine guidance is \
                 recognizable by its markers, so an untaught marker makes the \
                 engine's own text fail the prompt's own test (#2722) — name it \
                 in the clause in prompt.rs"
            );
        }
    }
}

/// Witness for the batching gap: Stella essentially never issued two tool
/// calls in one response, and neither prompt said it could.
///
/// Measured by censusing `tool_start` against `step_usage` (one completed model
/// call) in the bench event logs of four arms: post1 600 tool calls over 595
/// model calls (1.01 per turn, 519 of 595 turns carrying exactly one), s5b2
/// 649/659, dec1 543/578, f89b2 2812/2707. Essentially every tool call bought
/// its own round trip. That is the direct cost lever, because a turn re-reads
/// the whole cached conversation prefix — measured growth on one real trial was
/// 16.7k tokens at turn 0 to 46.6k by turn 55 — so three independent reads
/// issued separately pay for that transcript three times and issued together
/// pay once. `rg -ic parallel` over this file's subject returned 0 before this
/// bullet: the capability was never mentioned at all.
///
/// The dependency test is the half that makes the rule safe, and it is pinned
/// separately for that reason. Without it the instruction reads as "batch your
/// calls" and produces an agent issuing an edit alongside the read of the file
/// it has not seen yet — which is why the negative clauses ("Never batch an
/// edit with the read it depends on", "read a file before editing it") are
/// asserted here rather than left to survive a future trim on their own.
///
/// Lives here rather than beside its siblings in `prompt.rs`'s test module for
/// the same reason `both_prompts_bind_the_model_to_skills` does: that file sits
/// within a few lines of the 1500-line ratchet (#2985), and #2237 makes this
/// module the home for prompt-content pins.
#[test]
fn both_prompts_batch_independent_tool_calls() {
    let shared = tool_steering!();
    for claim in [
        // The rule itself.
        "Independent tool calls belong in ONE response",
        // Why it pays — the cached prefix is re-sent per response, so the
        // saving is per avoided round trip, not per avoided tool call.
        "re-sends the entire conversation so far to the model",
        // The dependency test, which is what makes batching safe rather than
        // merely aggressive.
        "The test is dependency",
        "issue them together in the same response",
        // The negative half: a dependent call is still sequential, and the
        // read-before-edit case is named explicitly because it is the one a
        // blunt batching rule breaks first.
        "only where one genuinely consumes a previous result",
        "read a file before editing it",
        "Never batch an edit with the read it depends on",
    ] {
        assert!(
            shared.contains(claim),
            "the shared literal must keep the batching claim {claim:?} — \
             dropping the dependency half leaves a blunt rule that batches an \
             edit with the read it depends on"
        );
    }
    for (label, prompt) in STATIC_PROMPTS {
        assert!(
            prompt.contains(shared),
            "{label} does not embed the shared tool-steering block verbatim"
        );
    }
}

/// Witness for the shell-routing gap: Stella reached for `bash` when it held a
/// purpose-built tool, and neither prompt said not to.
///
/// Measured in bench run post1: 425 of 600 tool calls were `bash`. In one trial
/// the worker made 8 shell `grep` invocations *on top of* 6 calls to its own
/// `grep` tool, in the same session against the same file. `rg -ic "dedicated
/// tool"` over both prompts returned 0.
///
/// Both stated reasons are pinned because they are different claims. The first
/// is about the record: a shell command declares no path, so
/// `stella_tools::shell_touch::WorkspaceProbe` has to fingerprint the workspace
/// either side of every opaque call and attribute the difference — a walk whose
/// bounds are recorded as saturation rather than guessed at, which is why it
/// can come up short (deletions are dropped outright once either walk
/// saturated). A dedicated tool declares its path and needs none of that. The
/// prompt says "can come up short", deliberately not "invisible": shell
/// mutations ARE attributed here, just reconstructed rather than declared.
/// The second reason is the per-call cost, and it is the weaker of the two.
///
/// The reservation clause is pinned for the same reason the dependency test
/// above is: shell is the right answer for a large share of real work on this
/// benchmark, and an instruction that reads as "never use bash" fights the task
/// and gets ignored wholesale. A trim that keeps the routing and drops the
/// reservation would turn this into exactly that rule.
#[test]
fn both_prompts_route_file_work_to_the_dedicated_tool() {
    let shared = tool_steering!();
    for claim in [
        // The routing itself, enumerated so a schema-level "use the right
        // tool" cannot stand in for it.
        "Reading, editing, and creating files, finding files by name, and \
         searching file contents each have a dedicated tool",
        "use it rather than the shell",
        // Reason one: the record. A shell edit declares no path, so the
        // change has to be reconstructed by a bounded workspace scan.
        "names the file it touches",
        "fingerprinting the whole workspace either side of the call",
        // Reason two, and deliberately the weaker of the two.
        "cheaper per call than shelling out",
        // The negative half — this is routing, never a ban.
        "This is routing, not a ban",
        "running builds and tests, process and service control, git operations",
    ] {
        assert!(
            shared.contains(claim),
            "the shared literal must keep the shell-routing claim {claim:?} — \
             dropping the reservation clause turns routing into a blanket ban \
             on a tool that is the right answer for much of the work"
        );
    }
    for (label, prompt) in STATIC_PROMPTS {
        assert!(
            prompt.contains(shared),
            "{label} does not embed the shared tool-steering block verbatim"
        );
    }
}

/// Witness for the skill-use gap (#2724): neither persona said the word
/// "skill", so the only skill text the model ever saw was the volatile recall
/// block's section header — a label with no contract. Each clause pinned
/// separately, in the established `both_prompts_*` shape, so a trim cannot
/// hollow the contract while the verbatim embedding assertion still passes.
///
/// Lives here rather than beside its siblings in `prompt.rs`'s test module
/// because that file sits within a few lines of the 1500-line ratchet
/// (`scripts/check-file-size.sh`) — and #2237 already consolidates the
/// per-contract pins into this module, so this is the direction of travel,
/// not an exception to it.
#[test]
fn both_prompts_bind_the_model_to_skills() {
    let shared = skill_use!();
    for claim in [
        // What a skill is, and where the selected ones arrive.
        "durable procedures",
        "arrive in the recalled-context block",
        // The check-before-work rule — conditional on the tool, because
        // `skill_search` is a session-layer tool absent from reduced
        // assemblies and this one static text must stay honest in all of
        // them (never fork the prefix per-assembly).
        "when a skill-search tool is available",
        // An explicit request is an instruction, not a suggestion.
        "an instruction to apply it, not optional context",
        // The honest escape hatch: deviation is stated, never silent —
        // without it the contract degrades into ritual compliance.
        "set it aside and say so with the reason",
    ] {
        assert!(
            shared.contains(claim),
            "the shared literal must keep the skill-use claim {claim:?} — \
             without it the prompt goes back to teaching skills nothing (#2724)"
        );
    }
    for (label, prompt) in STATIC_PROMPTS {
        assert!(
            prompt.contains(shared),
            "{label} does not embed the shared skill-use block verbatim"
        );
    }
}

/// The delta-orientation rule (#2984), pinned clause by clause so a trim
/// cannot leave the half that makes it safe behind.
///
/// Measured on TB2.1 `fix-code-vulnerability`, where the bug is planted as an
/// *uncommitted* working-tree change: Claude Code opened with `git diff
/// bottle.py` at call 1 in both trials and finished in 7 calls; Stella reached
/// the same diff at call 18, 34, 40 and 41 across four trials and took 23-63.
/// Its one early git probe was `git diff HEAD~5` — committed history, which
/// structurally cannot see an uncommitted change — and it then spent dozens of
/// calls grepping for the bug by name.
///
/// The rule is written as a **discriminator, not a recipe**, because the
/// advantage being copied is selectivity rather than a first move: Claude Code
/// probes git in only 6 of 20 trials (0.3 git operations per trial on non-git
/// tasks, against Stella's 3.8). An unconditional "check the diff first" would
/// add a wasted call to the 14 tasks where git is irrelevant, which is why the
/// trigger is a property of the task text alone — a past-tense claim that
/// something was done to this repository — evaluable before any call is spent.
///
/// This is deliberately **not** a shared contract: it steers a first move, and
/// the pipeline persona already prescribes its own first move (the ORIENT →
/// REPRODUCE → LOCALIZE methodology), so embedding it there would put two
/// rules in the same slot. `SHARED_CONTRACTS` therefore does not list it and
/// `every_shared_contract_is_invoked_by_every_static_prompt` does not cover it
/// — hence this pin.
///
/// The two clauses are load-bearing in opposite directions and each fails on
/// its own. Without the working-tree-first clause the rule reproduces the exact
/// measured defect: Stella's one early git probe was `git diff HEAD~5`, which
/// reads committed history and cannot see an uncommitted change. Without the
/// negative clause it becomes "always check the diff first", which would add a
/// wasted call to the 14 of 20 trials where Claude Code — the arm being copied
/// — touches git not at all.
///
/// Lives here rather than beside its siblings in `prompt.rs`'s test module for
/// the same reason `both_prompts_bind_the_model_to_skills` does: that file sits
/// within a few lines of the 1500-line ratchet, and #2237 makes this module the
/// home for prompt-content pins.
#[test]
fn the_default_persona_reads_the_delta_only_when_the_task_claims_one() {
    for claim in [
        // The trigger, stated as a property of the task text alone — the only
        // kind of rule the model can evaluate before spending a call.
        "claims something was DONE to this repository",
        // Working tree before history: the measured failure was a
        // history-first probe against an uncommitted change.
        "read the WORKING TREE first",
        "need never have been committed",
        // The negative half — what stops a discriminator becoming a recipe.
        "a task to BUILD, ADD or IMPLEMENT something",
        "the probe is a call spent on nothing",
    ] {
        assert!(
            SYSTEM_PROMPT.contains(claim),
            "the default persona must keep the delta-orientation claim {claim:?} — \
             without it the rule is either blind to uncommitted changes or fires on \
             every task (#2984)"
        );
    }
    assert!(
        !PIPELINE_SYSTEM_PROMPT.contains("read the WORKING TREE first"),
        "the delta-orientation rule is scoped to the default persona: the pipeline \
         persona prescribes its own first move (ORIENT/REPRODUCE/LOCALIZE), and two \
         rules competing for the same slot is the drift this scoping avoids (#2984)"
    );
}

/// The localization discriminator (#3015), pinned in both personas and clause
/// by clause so a trim cannot drop the half that keeps it from firing
/// everywhere.
///
/// `semantic_code_search` (#3014) exists to remove a measured spiral: on TB2.1
/// `fix-code-vulnerability`, 13 of 58 tool calls went to grepping ONE file for
/// spelling variants of one concept (`_hkey|_hval|HeaderDict|CRLF`) — a
/// semantic question asked of a substring index. The model decides whether to
/// call a tool from context, and until this rule landed the localization bullet
/// named `graph_query` alone and routed "free-text search" to grep by name,
/// which is the wrong answer once a semantic index exists.
///
/// The rule is a **discriminator, not a preference**: the split is a property
/// of the question the model is already holding — can you NAME the symbol, or
/// can you only DESCRIBE what it does — so it is decidable before a call is
/// spent. `graph_query` keeps the named case; grep keeps the genuinely lexical
/// case. "Always try semantic search first" is the version deliberately NOT
/// written: a repository with no index, or a question like "find every TODO",
/// pays a call for nothing.
///
/// Unlike the delta-orientation rule above, this one is pinned in **both**
/// prompts. It steers tool choice inside localization rather than a first move,
/// so the pipeline persona's ORIENT/REPRODUCE/LOCALIZE methodology has a slot
/// for it (step 3) rather than a competing rule — and `PIPELINE_SYSTEM_PROMPT`
/// is what `stella run` and every bench measurement send (see prompt.rs's
/// header note, #2231), so a localization fix reaching `SYSTEM_PROMPT` alone
/// would be invisible to #2995, the measurement that settles whether the model
/// reaches for the tool at all.
///
/// It is not a `macro_rules!` shared contract because the two sites are
/// genuinely different shapes — a Rules bullet and a numbered methodology step
/// — so the wording that survives is asserted here instead of the bytes.
///
/// Lives here rather than beside its siblings in `prompt.rs`'s test module for
/// the reason its two predecessors do: that file sits within a few lines of the
/// 1500-line ratchet (#2985), and #2237 makes this module the home for
/// prompt-content pins.
#[test]
fn both_personas_route_a_described_behaviour_to_semantic_search_and_a_named_one_to_the_graph() {
    for (label, prompt) in STATIC_PROMPTS {
        for claim in [
            // The named half keeps the tool it always had.
            "When you can NAME the",
            "graph_query FIRST",
            // The described half — the whole point, and stated as "before any
            // grep" because the measured failure is grep spent first.
            "semantic_code_search BEFORE any grep",
            // The tell that makes the discriminator decidable in advance: an
            // alternation of spellings of ONE idea is the case.
            "`redact|scrub|sanitize|mask`",
            // The negative half. Without it the rule degrades into "always try
            // semantic search first", which spends a call on every lexical
            // question and on every repository with no index.
            "Grep and glob stay the right answer for a genuinely lexical question",
            "the repository has no index yet",
        ] {
            assert!(
                prompt.contains(claim),
                "{label} must keep the localization claim {claim:?} — without it the rule \
                 either stops naming semantic_code_search at all (the #3015 defect) or \
                 fires on questions grep answers for free (#3015)"
            );
        }
        assert!(
            !prompt.contains("free-text search"),
            "{label} still routes \"free-text search\" to grep by name. That was the wrong \
             answer the moment semantic_code_search shipped (#3014) — a free-text question \
             is precisely the described-behaviour case (#3015)"
        );
    }
}

/// Every way this check could come up empty, driven on hand-built sources.
///
/// The tests above all iterate sets the scan produced; if the scan silently
/// returned nothing they would pass while examining nothing, which is the
/// failure mode the whole module is aimed at. These pin that each of those
/// ways is an error with a name instead.
#[test]
fn the_scan_fails_loudly_rather_than_finding_nothing() {
    assert_eq!(
        scan("").unwrap_err(),
        ScanError::NoContractsDeclared,
        "an empty source must be an error, never an empty set of contracts to check"
    );
    assert_eq!(
        scan("// nothing here declares a contract\nfn main() {}\n").unwrap_err(),
        ScanError::NoContractsDeclared,
        "a source with no `macro_rules!` must be an error"
    );
    assert_eq!(
        scan("macro_rules! only_contract {\n    () => { \"x\" };\n}\n").unwrap_err(),
        ScanError::NoStaticPromptsParsed,
        "contracts with nowhere to be embedded must be an error"
    );
    // The byte-stability half: a prompt rewritten as a runtime composition
    // stops being a static prompt, and that is loud rather than invisible.
    assert_eq!(
        scan(concat!(
            "macro_rules! only_contract {\n    () => { \"x\" };\n}\n",
            "pub(crate) fn system_prompt() -> String {\n    format!(\"{}\", only_contract!())\n}\n"
        ))
        .unwrap_err(),
        ScanError::NoStaticPromptsParsed,
        "a prompt assembled at runtime must not silently drop out of the scan (invariant 7)"
    );
}

/// The catch, demonstrated on a source built to hold the defect.
///
/// The witness the artisanal way — stripping a real embedding from `prompt.rs`
/// and watching the test above fail — proves it once, in a checkout nobody else
/// sees. This proves the same thing every time the suite runs, and pins the
/// half that matters: the omission is reported *naming which contract and which
/// prompt*, because "something is wrong in prompt.rs" is not a handoff.
#[test]
fn an_omission_from_one_prompt_only_is_caught_and_named() {
    let source = concat!(
        "macro_rules! shared_everywhere {\n    () => { \"a\" };\n}\n",
        "macro_rules! shared_by_convention {\n    () => { \"b\" };\n}\n",
        "pub(crate) const SYSTEM_PROMPT: &str = concat!(\n",
        "    shared_everywhere!(),\n",
        "    shared_by_convention!(),\n",
        ");\n",
        "pub(crate) const PIPELINE_SYSTEM_PROMPT: &str = concat!(\n",
        "    shared_everywhere!(),\n",
        ");\n",
    );
    let scan = scan(source).expect("the fixture declares contracts and static prompts");
    assert_eq!(
        scan.contracts,
        ["shared_everywhere", "shared_by_convention"]
    );
    assert_eq!(
        scan.prompts
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>(),
        ["SYSTEM_PROMPT", "PIPELINE_SYSTEM_PROMPT"]
    );
    assert_eq!(
        unembedded(&scan),
        vec![("shared_by_convention", "PIPELINE_SYSTEM_PROMPT")],
        "the exact #2231 shape — a contract in one prompt only — must be reported, and \
         reported with both names"
    );
}

/// **Witness: neither static prompt names a tool the session may not have.**
///
/// `ask_user` is registered only when a human is present to answer it
/// ([`crate::interactive::human_can_answer`]), while both prompts are static
/// bytes shared by attended and unattended runs alike — one `concat!` each,
/// riding the cache prefix, with no per-mode text. A prompt that recommends a
/// tool missing from the schema is worse than one that never mentioned it: it
/// sends the agent looking for something that is not there, which is the
/// state withholding the tool exists to leave.
///
/// Both prompts are checked, and by the same rule, because
/// `PIPELINE_SYSTEM_PROMPT` is what `stella run` sends and what the bench
/// harness measures — the copy an omission goes missing from quietly (see
/// this module's header).
#[test]
fn no_static_prompt_names_a_tool_the_session_may_not_offer() {
    for (name, prompt) in STATIC_PROMPTS {
        assert!(
            !prompt.contains("ask_user"),
            "{name} names `ask_user`, which an unattended session does not register"
        );
    }
}
