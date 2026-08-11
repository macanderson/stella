//! A tiny, self-contained MCP server for the `stella-mcp` integration tests.
//! It speaks newline-delimited JSON-RPC on stdin/stdout with canned answers to
//! `initialize`, `tools/list`, `tools/call`, `resources/list`, and
//! `resources/read`, plus fault-injection flags for the resilience tests. It
//! is **not** part of the shipping surface — only `tests/` build and launch it
//! (via `env!("CARGO_BIN_EXE_mcp-fixture-server")`).
//!
//! Flags:
//! - `--protocol-version <v>`: counter-offer `<v>` in the initialize result.
//! - `--paginate`: return `tools/list` across two cursor pages.
//! - `--hang`: never answer `tools/call` (exercises the call timeout).
//! - `--delay-call-ms <n>`: answer `tools/call` after sleeping `<n>` ms — a
//!   slow-but-working tool (exercises the connect/call timeout split).
//! - `--die-after <n>`: exit(0) upon the `(n+1)`-th request, before answering
//!   it (exercises mid-call server death).
//! - `--garbage`: answer `tools/call` with an unparseable `result`.
//! - `--stderr <text>`: write `<text>` to stderr at startup and again before
//!   every response — a server that logs (and, with `--die-after`, one whose
//!   log is the only clue why it died).
//! - `--no-resources`: omit the `resources` capability from `initialize` and
//!   answer `resources/*` with method-not-found — a tools-only server (#2678).
//!
//! Tools it advertises: `echo`, `env_probe`, `make_image`, `make_resource`,
//! `fail` (`isError:true`), `jsonrpc_error` (a JSON-RPC error response).
//! Resources it serves (unless `--no-resources`): `file:///r.txt` (text) and
//! `file:///data.bin` (a base64 blob); any other URI is a JSON-RPC error.

use std::io::{self, BufRead, Write};
use std::time::Duration;

use serde_json::{Value, json};

struct Flags {
    hang: bool,
    delay_call: Option<Duration>,
    die_after: Option<u64>,
    garbage: bool,
    paginate: bool,
    protocol_version: String,
    stderr: Option<String>,
    no_resources: bool,
}

impl Flags {
    fn parse() -> Self {
        let mut flags = Flags {
            hang: false,
            delay_call: None,
            die_after: None,
            garbage: false,
            paginate: false,
            protocol_version: "2025-06-18".to_string(),
            stderr: None,
            no_resources: false,
        };
        let args: Vec<String> = std::env::args().skip(1).collect();
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--hang" => flags.hang = true,
                "--garbage" => flags.garbage = true,
                "--paginate" => flags.paginate = true,
                "--no-resources" => flags.no_resources = true,
                "--die-after" => {
                    i += 1;
                    if i < args.len() {
                        flags.die_after = args[i].parse().ok();
                    }
                }
                "--delay-call-ms" => {
                    i += 1;
                    if i < args.len() {
                        flags.delay_call = args[i].parse().ok().map(Duration::from_millis);
                    }
                }
                "--protocol-version" => {
                    i += 1;
                    if i < args.len() {
                        flags.protocol_version = args[i].clone();
                    }
                }
                "--stderr" => {
                    i += 1;
                    if i < args.len() {
                        flags.stderr = Some(args[i].clone());
                    }
                }
                _ => {}
            }
            i += 1;
        }
        flags
    }
}

fn main() {
    let flags = Flags::parse();
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut served: u64 = 0;

    log_stderr(&flags);

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(_) => continue,
        };

        // Notifications (no id) get no response.
        let id = match message.get("id") {
            Some(id) if !id.is_null() => id.clone(),
            _ => continue,
        };

        // `--die-after`: exit before answering once we've served enough.
        if let Some(limit) = flags.die_after
            && served >= limit
        {
            std::process::exit(0);
        }

        // Interleave the log line with real traffic: proves stderr noise
        // cannot corrupt the JSON-RPC framing on stdout.
        log_stderr(&flags);

        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let params = message.get("params").cloned().unwrap_or(Value::Null);

        let response = match method {
            "initialize" => {
                let capabilities = if flags.no_resources {
                    json!({ "tools": {} })
                } else {
                    json!({ "tools": {}, "resources": {} })
                };
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": flags.protocol_version,
                        "capabilities": capabilities,
                        "serverInfo": { "name": "mcp-fixture-server", "version": "0.1.0" }
                    }
                })
            }
            "tools/list" => tools_list_response(&id, &params, &flags),
            "resources/list" if !flags.no_resources => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "resources": [
                    {
                        "uri": "file:///r.txt",
                        "name": "r.txt",
                        "mimeType": "text/plain",
                        "description": "a small text resource"
                    },
                    {
                        "uri": "file:///data.bin",
                        "name": "data.bin",
                        "mimeType": "application/octet-stream"
                    }
                ] }
            }),
            "resources/read" if !flags.no_resources => resources_read_response(&id, &params),
            "tools/call" => {
                if flags.hang {
                    loop {
                        std::thread::sleep(Duration::from_secs(3600));
                    }
                }
                // A slow-but-working tool, unlike `--hang`'s never-answering one.
                if let Some(delay) = flags.delay_call {
                    std::thread::sleep(delay);
                }
                tools_call_response(&id, &params, &flags)
            }
            _ => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": "method not found" }
            }),
        };

        write_line(&mut stdout, &response);
        served += 1;
    }
}

/// Emit `--stderr <text>` (when set) on the *other* stream. Flushed so a
/// `--die-after` exit cannot strand it in a buffer.
fn log_stderr(flags: &Flags) {
    if let Some(text) = &flags.stderr {
        let mut stderr = io::stderr();
        let _ = writeln!(stderr, "{text}");
        let _ = stderr.flush();
    }
}

fn write_line(stdout: &mut io::Stdout, value: &Value) {
    let mut line = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string());
    line.push('\n');
    let _ = stdout.write_all(line.as_bytes());
    let _ = stdout.flush();
}

fn tool_def(name: &str) -> Value {
    json!({
        "name": name,
        "description": format!("fixture tool {name}"),
        "inputSchema": { "type": "object" }
    })
}

fn all_tools() -> Vec<Value> {
    [
        "echo",
        "env_probe",
        "make_image",
        "make_resource",
        "fail",
        "jsonrpc_error",
    ]
    .iter()
    .map(|name| tool_def(name))
    .collect()
}

fn tools_list_response(id: &Value, params: &Value, flags: &Flags) -> Value {
    if flags.paginate {
        let cursor = params.get("cursor").and_then(Value::as_str);
        return match cursor {
            None => json!({
                "jsonrpc": "2.0", "id": id,
                "result": { "tools": [tool_def("alpha")], "nextCursor": "page2" }
            }),
            Some("page2") => json!({
                "jsonrpc": "2.0", "id": id,
                "result": { "tools": [tool_def("beta")] }
            }),
            Some(_) => json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": [] } }),
        };
    }
    json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": all_tools() } })
}

/// `resources/read`: the text resource, the binary one, or a JSON-RPC
/// "resource not found" error for any other URI (#2678).
fn resources_read_response(id: &Value, params: &Value) -> Value {
    let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
    match uri {
        "file:///r.txt" => json!({
            "jsonrpc": "2.0", "id": id,
            "result": { "contents": [{
                "uri": "file:///r.txt",
                "mimeType": "text/plain",
                "text": "hello from the fixture resource"
            }] }
        }),
        "file:///data.bin" => json!({
            "jsonrpc": "2.0", "id": id,
            "result": { "contents": [{
                "uri": "file:///data.bin",
                "mimeType": "application/octet-stream",
                "blob": "QUFBQQ=="
            }] }
        }),
        other => json!({
            "jsonrpc": "2.0", "id": id,
            "error": { "code": -32002, "message": format!("resource not found: {other}") }
        }),
    }
}

fn tools_call_response(id: &Value, params: &Value, flags: &Flags) -> Value {
    // `--garbage`: a well-formed JSON-RPC envelope whose `result` is not a
    // tool-call result — the client must fail to decode it, not crash.
    if flags.garbage {
        return json!({ "jsonrpc": "2.0", "id": id, "result": 42 });
    }

    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
    let tool = params.get("name").and_then(Value::as_str).unwrap_or("");

    let result = match tool {
        "echo" => json!({
            "content": [{ "type": "text", "text": format!("echo: {arguments}") }],
            "isError": false
        }),
        "env_probe" => {
            let var = arguments.get("var").and_then(Value::as_str).unwrap_or("");
            let value = std::env::var(var).unwrap_or_else(|_| "unset".to_string());
            json!({ "content": [{ "type": "text", "text": value }] })
        }
        "make_image" => json!({
            "content": [{ "type": "image", "data": "AAAA", "mimeType": "image/png" }]
        }),
        "make_resource" => json!({
            "content": [{ "type": "resource", "resource": { "uri": "file:///r.txt", "text": "hi" } }]
        }),
        "fail" => json!({
            "content": [{ "type": "text", "text": "the tool failed" }],
            "isError": true
        }),
        "jsonrpc_error" => {
            return json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32000, "message": "boom from server" }
            });
        }
        other => json!({
            "content": [{ "type": "text", "text": format!("unknown fixture tool: {other}") }],
            "isError": true
        }),
    };

    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}
