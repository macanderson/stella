//! Allocator-churn + wall-clock A/B for the per-attempt model-request build
//! (#921).
//!
//! # Why this harness exists at all
//!
//! `bench/loop-bench` measures whether the turn loop is *correct* — it distills
//! `STUCK-LOOP` / `ZERO-WORK` verdicts out of Terminal-Bench trial streams and
//! needs a live flash-tier model to produce a single row. It carries no
//! allocator or per-step timing instrumentation, so it cannot answer "how many
//! megabytes does a turn churn building requests". This harness does exactly
//! that, deterministically, offline, with no API key and no spend.
//!
//! # What it measures
//!
//! The work `Engine::run_model_call` does to hand one `CompletionRequest` to a
//! provider adapter, at a realistic transcript size, repeated over a realistic
//! turn. Two arms:
//!
//! * `owned-clone` — the shape the driver used before #921. The retry
//!   `attempt_fn` is `FnMut`, so it cannot move its inputs: every attempt paid
//!   `messages.to_vec()` (a deep copy of every message `String`, every tool
//!   call's `serde_json::Value`, every tool result payload) plus
//!   `tools_schema.clone()` (every `ToolSchema` including its full JSON
//!   parameter document).
//! * `borrowed` — the shape after #921. `CompletionRequestRef<'a>` holds
//!   `&[CompletionMessage]` / `&[ToolSchema]` and is `Copy`, so an `FnMut`
//!   closure hands out a fresh view per attempt without allocating.
//!
//! Both arms stay expressible forever — the owned arm is just `Vec::clone` —
//! so this is a permanent A/B rather than a number that rots the moment the
//! branch it was taken on is merged.
//!
//! # Reading the output
//!
//! Numbers are cumulative allocator traffic, not peak residency: the copies are
//! discarded immediately after serialization, so peak RSS understates the cost
//! while churn (and the `memcpy` + `free` it implies) is what the step actually
//! pays. Run with `--release`; a debug build measures the allocator honestly
//! but the wall-clock column is meaningless.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use stella_protocol::{
    CompletionMessage, CompletionRequest, CompletionRequestRef, MessageRole, ReasoningEffort,
    ToolCall, ToolOutput, ToolResult, ToolSchema,
};

/// Cumulative bytes handed out by [`Counting`] since the last reset.
static BYTES: AtomicUsize = AtomicUsize::new(0);
/// Cumulative successful allocations served by [`Counting`] since the last reset.
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

/// A pass-through global allocator that counts what it hands out.
///
/// `Relaxed` ordering throughout: this harness is single-threaded and the
/// counters are read only after the measured section has finished, so the
/// stronger orderings would buy nothing but overhead inside the very hot path
/// being timed.
struct Counting;

// SAFETY: every method forwards verbatim to `System`, which is a valid
// `GlobalAlloc`. The counters are the only added behavior and they neither
// allocate nor observe the returned pointers.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            BYTES.fetch_add(layout.size(), Ordering::Relaxed);
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let out = unsafe { System.realloc(ptr, layout, new_size) };
        // A grow is fresh bytes the program did not have before; a shrink is
        // not charged. Counting the whole `new_size` would double-count the
        // bytes already charged at `alloc`.
        if !out.is_null() && new_size > layout.size() {
            BYTES.fetch_add(new_size - layout.size(), Ordering::Relaxed);
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        out
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Zero the counters and return what one measured section cost.
fn measure<T>(body: impl FnOnce() -> T) -> (usize, usize) {
    BYTES.store(0, Ordering::Relaxed);
    ALLOCS.store(0, Ordering::Relaxed);
    let out = body();
    let cost = (
        BYTES.load(Ordering::Relaxed),
        ALLOCS.load(Ordering::Relaxed),
    );
    drop(black_box(out));
    cost
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// `stella_core::estimator::CHARS_PER_TOKEN` — duplicated rather than depended
/// on, because pulling `stella-core` in would make this harness link the whole
/// engine to size a `String`.
const CHARS_PER_TOKEN: f64 = 3.5;

/// Transcript size the issue names as the realistic deep-session case.
const TARGET_TOKENS: usize = 150_000;
/// MCP-registered tool count the issue names.
const TOOLS: usize = 60;
/// Model calls in the turn the issue names.
const STEPS: usize = 200;

/// A transcript shaped like a real coding turn: a system prefix, then repeating
/// user → assistant-with-tool-call → tool-result triples, grown until it
/// estimates at [`TARGET_TOKENS`].
fn transcript() -> Vec<CompletionMessage> {
    let target_chars = (TARGET_TOKENS as f64 * CHARS_PER_TOKEN) as usize;
    let mut messages = vec![
        CompletionMessage::system(
            "You are Stella, an autonomous coding agent. Prefer small diffs. \
             Read before you write. Never fabricate a citation.",
        ),
        CompletionMessage::user("Fix the failing integration test in the store crate."),
    ];
    let mut chars = 0usize;
    let mut turn = 0usize;
    while chars < target_chars {
        let call_id = format!("call_{turn:04}");
        let input = serde_json::json!({
            "command": format!("cargo test -p stella-store --test integration_{turn}"),
            "timeout_ms": 120_000,
            "description": "Run the store integration suite",
        });
        let output = format!(
            "running 42 tests\n{}\ntest result: FAILED. 41 passed; 1 failed\n",
            "test store::roundtrip::case ... ok\n".repeat(48)
        );
        chars += output.len() + 256;
        messages.push(CompletionMessage {
            role: MessageRole::Assistant,
            content: format!("Checking suite {turn} before touching the fold."),
            tool_calls: vec![ToolCall {
                call_id: call_id.clone(),
                name: "bash".into(),
                input,
            }],
            tool_results: Vec::new(),
            attachments: Vec::new(),
        });
        messages.push(CompletionMessage {
            role: MessageRole::Tool,
            content: String::new(),
            tool_calls: Vec::new(),
            tool_results: vec![ToolResult {
                call_id,
                output: ToolOutput::Ok { content: output },
            }],
            attachments: Vec::new(),
        });
        turn += 1;
    }
    messages
}

/// [`TOOLS`] schemas with parameter documents the size a real MCP server ships.
fn tool_schemas() -> Vec<ToolSchema> {
    (0..TOOLS)
        .map(|i| ToolSchema {
            name: format!("mcp__server_{}__operation_{i}", i % 7),
            description: format!(
                "Operation {i}. Performs the documented action against the remote \
                 service and returns a structured result. Prefer this over shelling \
                 out. Fails closed when the workspace is not trusted."
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Absolute path to the target resource."},
                    "query": {"type": "string", "description": "Search expression, in the server's own dialect."},
                    "limit": {"type": "integer", "description": "Maximum rows to return.", "default": 50},
                    "recursive": {"type": "boolean", "description": "Descend into child containers."},
                    "fields": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Projection: which fields to include in each row.",
                    },
                },
                "required": ["path"],
                "additionalProperties": false,
            }),
            read_only: i % 3 == 0,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Arms
// ---------------------------------------------------------------------------

/// The pre-#921 driver arm: one deep copy of the transcript and one of the
/// schema list, per attempt, because `CompletionRequest` owns both and the
/// retry closure is `FnMut`.
fn owned_clone(messages: &[CompletionMessage], tools: &[ToolSchema]) -> CompletionRequest {
    CompletionRequest {
        messages: messages.to_vec(),
        max_output_tokens: Some(32_000),
        temperature: None,
        effort: Some(ReasoningEffort::High),
        reasoning: Some(true),
        params: None,
        tools: tools.to_vec(),
    }
}

/// The post-#921 driver arm: a `Copy` view over the caller's slices.
fn borrowed<'a>(
    messages: &'a [CompletionMessage],
    tools: &'a [ToolSchema],
) -> CompletionRequestRef<'a> {
    CompletionRequestRef {
        messages,
        max_output_tokens: Some(32_000),
        temperature: None,
        effort: Some(ReasoningEffort::High),
        reasoning: Some(true),
        params: None,
        tools,
    }
}

fn main() {
    let messages = transcript();
    let tools = tool_schemas();
    let transcript_bytes: usize = messages
        .iter()
        .map(|m| {
            m.content.len()
                + m.tool_calls
                    .iter()
                    .map(|c| c.input.to_string().len())
                    .sum::<usize>()
                + m.tool_results
                    .iter()
                    .map(|r| match &r.output {
                        ToolOutput::Ok { content } => content.len(),
                        ToolOutput::Error { message } => message.len(),
                    })
                    .sum::<usize>()
        })
        .sum();

    println!("request-bench (#921) — per-attempt model-request build");
    println!(
        "  fixture: {} messages, ~{} KiB of transcript text, {} tool schemas",
        messages.len(),
        transcript_bytes / 1024,
        tools.len()
    );
    println!("  turn:    {STEPS} steps, 1 attempt per step (the overwhelmingly common case)\n");

    // Warm the allocator's free lists so the first arm is not charged for
    // arena growth the second arm inherits for free.
    for _ in 0..3 {
        black_box(owned_clone(&messages, &tools));
    }

    let owned_started = Instant::now();
    let (owned_bytes, owned_allocs) = measure(|| {
        for _ in 0..STEPS {
            black_box(owned_clone(black_box(&messages), black_box(&tools)));
        }
    });
    let owned_elapsed = owned_started.elapsed();

    let borrowed_started = Instant::now();
    let (borrowed_bytes, borrowed_allocs) = measure(|| {
        for _ in 0..STEPS {
            black_box(borrowed(black_box(&messages), black_box(&tools)));
        }
    });
    let borrowed_elapsed = borrowed_started.elapsed();

    println!("  arm            per step (bytes)   per step (allocs)   per turn (MiB)   wall/turn");
    for (label, bytes, allocs, elapsed) in [
        ("owned-clone", owned_bytes, owned_allocs, owned_elapsed),
        (
            "borrowed",
            borrowed_bytes,
            borrowed_allocs,
            borrowed_elapsed,
        ),
    ] {
        println!(
            "  {label:<13}  {:>16}   {:>17}   {:>14.1}   {:>9.1?}",
            bytes / STEPS,
            allocs / STEPS,
            bytes as f64 / (1024.0 * 1024.0),
            elapsed,
        );
    }

    // The acceptance criterion, asserted rather than eyeballed: a single-attempt
    // model call performs no deep copy of the transcript or the tool schemas.
    assert_eq!(
        borrowed_bytes, 0,
        "the borrowed arm allocated {borrowed_bytes} bytes — it is supposed to \
         be a `Copy` of slices and scalars"
    );
    // Reported as a difference, not a ratio: the borrowed arm allocates
    // nothing, and "∞x better" is not a number anyone can act on.
    println!(
        "\n  → {} allocations and {:.1} MiB of churn per turn removed; \
         {:?} of build time becomes {:?}.",
        owned_allocs - borrowed_allocs,
        (owned_bytes - borrowed_bytes) as f64 / (1024.0 * 1024.0),
        owned_elapsed,
        borrowed_elapsed,
    );
}
