//! Filesystem-derived views: skills, memories, rule files, distilled
//! lessons (`reflections.jsonl`), configured MCP servers (`mcp.toml`) and
//! the `settings.json` scope chain.
//!
//! Everything here is plain reads of files stella (or the user) already
//! wrote — [`skills`] additionally joins in a read-only peek at
//! `context.db`'s ledger to name the evidence grade behind a learned skill
//! (#4871), never opening it as a store. Two invariants:
//!
//! 1. **Nothing is created or modified** — absent files, directories, and
//!    ledger rows are states, rendered as empty sections.
//! 2. **Secrets never reach the browser.** `settings.json` and `mcp.toml`
//!    may carry credentials; [`redact`] scrubs any value under a
//!    sensitive-looking key (and every value under `env`/`headers` maps)
//!    before the JSON is serialized into a response.

use std::collections::HashMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};

/// Entries read from any one directory before the scan stops.
///
/// These directories hold hand-written skills, memories and rule files, so a
/// real one holds tens. The bound exists because a request from the page
/// triggers the walk synchronously, and a directory that has grown pathological
/// (or been pointed somewhere unexpected) should degrade the dashboard rather
/// than stall it.
const MAX_DIR_ENTRIES: usize = 512;

/// Bytes read from any one file whose content is only used to build a snippet
/// or read frontmatter.
///
/// Both uses look at the head of the file, so slurping a whole one to render
/// 200 characters is pure waste — and unbounded waste, on the request path.
/// The reported `bytes` is deliberately taken from the file's metadata rather
/// than from what was read, so the number the dashboard shows stays the true
/// file size.
const MAX_SNIPPET_READ_BYTES: u64 = 64 * 1024;

/// Read at most [`MAX_SNIPPET_READ_BYTES`] of a file as text.
///
/// Truncation can land mid-codepoint; `from_utf8_lossy` resolves that to a
/// replacement character, which is correct here because every caller is
/// building a preview rather than parsing.
fn read_head(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.take(MAX_SNIPPET_READ_BYTES)
        .read_to_end(&mut buf)
        .ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// The file's real size, for display alongside a bounded read.
fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// The user-scope stella config dir — `$STELLA_HOME`, else `~/.stella`,
/// mirroring `stella_cli::paths::stella_root` (the source of truth), so the
/// config tab names the file the CLI actually loads and the skills tab the
/// directory it actually scans.
///
/// Shared through `stella-home` rather than copied: this is the one resolver
/// both sides must agree on, and the observatory may not link `stella-cli`.
/// It honours `STELLA_HOME` because the loader does since #2178 — and only
/// `STELLA_HOME`, which is the whole of what can move this path. This
/// resolver used to honour `STELLA_CONFIG_DIR` too, which made the tab claim
/// a `settings.json` the CLI never opens; that variable reached no resolver
/// on either side and was retired in #2442. If the loader grows another
/// override, mirror it here in the same order.
pub fn user_config_dir() -> Option<PathBuf> {
    stella_home::stella_home()
}

/// The org-managed settings path — mirrors `stella-cli`'s
/// `managed_settings_path` (override: `STELLA_MANAGED_SETTINGS`).
fn managed_settings_path() -> PathBuf {
    if let Some(p) = std::env::var_os("STELLA_MANAGED_SETTINGS") {
        return PathBuf::from(p);
    }
    if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/stella/settings.json")
    } else {
        PathBuf::from("/etc/stella/settings.json")
    }
}

/// Seconds since the epoch for a file's mtime, when the OS will say.
fn mtime_unix(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let secs = modified.duration_since(UNIX_EPOCH).ok()?.as_secs();
    i64::try_from(secs).ok()
}

/// Pull `name:` and `description:` out of a `---` YAML frontmatter block
/// without a YAML dependency — stella's skill frontmatter is flat
/// `key: value` lines, which is all this reads.
fn frontmatter_fields(text: &str) -> (Option<String>, Option<String>, Option<String>) {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, None, None);
    }
    let mut name = None;
    let mut description = None;
    let mut origin = None;
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim().trim_matches('"').trim_matches('\'');
            match key.trim() {
                "name" if !value.is_empty() => name = Some(value.to_string()),
                "description" if !value.is_empty() => description = Some(value.to_string()),
                "origin" if !value.is_empty() => origin = Some(value.to_string()),
                _ => {}
            }
        }
    }
    (name, description, origin)
}

/// One skill entry from a markdown file.
///
/// `in_learned_dir` is position-derived provenance — true for a file under a
/// `learned/` subdirectory. It is an OR, not the whole answer: the miner names
/// its output `{skills_dir}/{slug}-{hash8}.md`, flat, and has never written a
/// `learned/` path segment (`stella_core::skills::decide_auto_creation` builds
/// `{target_dir}/{name}.md`). Reading provenance from position alone therefore
/// reported every auto-created skill as hand-authored and pinned the
/// dashboard's "learned skills" count to 0 no matter how many the loop
/// promoted. `origin: auto` is the marker the writer itself stamps into the
/// frontmatter (`render_skill_markdown`), so it is the authority here — and it
/// keeps traveling with the file if the user moves or renames it.
fn skill_entry(
    path: &Path,
    scope: &str,
    in_learned_dir: bool,
    grades: &HashMap<String, &'static str>,
) -> Option<Value> {
    // Only the frontmatter block is read out of this, so a bounded head read is
    // equivalent for any file whose frontmatter is not itself 64 KiB.
    let text = read_head(path)?;
    let (name, description, origin) = frontmatter_fields(&text);
    let learned = in_learned_dir || origin.as_deref() == Some("auto");
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    // A directory skill's stem is `SKILL`; prefer the directory name then.
    let fallback = if stem.eq_ignore_ascii_case("skill") {
        path.parent()
            .and_then(Path::file_name)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or(stem)
    } else {
        stem
    };
    let name = name.unwrap_or(fallback);
    // The candidate id the miner named the file for is the skill's own `name`
    // (`stella_core::skills::decide_auto_creation` builds `{name}.md`), so
    // that is the join key back to the proposal that promoted it (#4871). A
    // hand-authored skill was never a candidate and has no entry to find.
    let evidence_grade = grades.get(&name).copied();
    Some(json!({
        "name": name,
        "description": description.unwrap_or_default(),
        "scope": scope,
        "learned": learned,
        "evidence_grade": evidence_grade,
        "path": path.display().to_string(),
        "modified_unix": mtime_unix(path),
    }))
}

/// Scan one skills root: directories holding `SKILL.md`, flat `*.md` files,
/// and the `learned/` subdirectory. Note that a self-extracted skill is
/// recognized by its `origin: auto` frontmatter rather than by landing in
/// `learned/` — see [`skill_entry`] — so the flat-file branch below yields
/// learned skills too, which is where the miner actually writes them.
fn scan_skills_dir(
    dir: &Path,
    scope: &str,
    grades: &HashMap<String, &'static str>,
    out: &mut Vec<Value>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten().take(MAX_DIR_ENTRIES) {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if name == "learned" {
                if let Ok(learned) = std::fs::read_dir(&path) {
                    for file in learned.flatten().take(MAX_DIR_ENTRIES) {
                        let p = file.path();
                        if p.extension().is_some_and(|e| e == "md") {
                            out.extend(skill_entry(&p, scope, true, grades));
                        }
                    }
                }
            } else {
                let manifest = path.join("SKILL.md");
                if manifest.is_file() {
                    out.extend(skill_entry(&manifest, scope, false, grades));
                }
            }
        } else if path.extension().is_some_and(|e| e == "md") {
            // `extend`, not `push(…unwrap_or_default())`: an unreadable file is
            // a skipped row, never a JSON `null` the dashboard would try to
            // render as a skill.
            out.extend(skill_entry(&path, scope, false, grades));
        }
    }
}

/// The evidence grade recorded against each skill candidate id, read straight
/// off `.stella/private/context.db`'s `record_proposal` rows (#4871).
///
/// Read-only and best-effort, the same posture as everything else in this
/// module (module doc point 1): a missing or unreadable ledger — the common
/// case, since the shipped lexical loop never writes one — answers an empty
/// map rather than failing the dashboard. Folds every `Knowledge` proposal
/// recorded for a candidate with the weakest grade recorded
/// (`stella_protocol::provenance::ProvenanceGrade::weakest`) — #2782's rule
/// that combining evidence can only weaken it, applied across ledger
/// revisions the same way `EvidencePool` applies it across observations
/// within one. Named only through `ProposalRecord`'s own field and method,
/// never by importing `stella_protocol` itself: this crate's isolation note
/// (see the dev-dependency comments in `Cargo.toml`) keeps every write-path
/// crate a dev-only dependency, `ProvenanceGrade` included.
fn skill_evidence_grades(workspace_root: &Path) -> HashMap<String, &'static str> {
    use stella_core::context_record::{ProposalRecord, RecordProposalKind};

    let db = workspace_root
        .join(".stella")
        .join("private")
        .join("context.db");
    let Some(conn) = crate::db::open_read_only(&db) else {
        return HashMap::new();
    };
    let Ok(mut stmt) =
        conn.prepare("SELECT body FROM context_records WHERE record_kind = 'record_proposal'")
    else {
        return HashMap::new();
    };
    let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) else {
        return HashMap::new();
    };

    let mut grades = HashMap::new();
    for body in rows.flatten() {
        let Ok(proposal) = serde_json::from_str::<ProposalRecord>(&body) else {
            continue;
        };
        if proposal.proposal_kind != RecordProposalKind::Knowledge {
            continue;
        }
        let Some(grade) = proposal.provenance else {
            continue;
        };
        let weaker = match grades.get(&proposal.candidate_id) {
            Some(&existing) => grade < existing,
            None => true,
        };
        if weaker {
            grades.insert(proposal.candidate_id, grade);
        }
    }
    grades
        .into_iter()
        .map(|(id, grade)| (id, grade.as_str()))
        .collect()
}

/// Every skill visible to this workspace: project `.stella/skills` plus the
/// user-scope skills dir, with learned (self-extracted) skills flagged.
pub fn skills(workspace_root: &Path) -> Value {
    let grades = skill_evidence_grades(workspace_root);
    let mut rows = Vec::new();
    scan_skills_dir(
        &workspace_root.join(".stella/skills"),
        "project",
        &grades,
        &mut rows,
    );
    if let Some(user) = user_config_dir() {
        scan_skills_dir(&user.join("skills"), "user", &grades, &mut rows);
    }
    rows.sort_by(|a, b| {
        let key = |v: &Value| {
            (
                !v["learned"].as_bool().unwrap_or(false),
                v["name"].as_str().unwrap_or_default().to_lowercase(),
            )
        };
        key(a).cmp(&key(b))
    });
    json!(rows)
}

/// Collapse whitespace and cap for a card snippet, skipping any leading
/// `---` frontmatter block.
fn snippet(text: &str, max: usize) -> String {
    let body = match text.strip_prefix("---") {
        Some(rest) => rest.split_once("\n---").map(|(_, b)| b).unwrap_or(text),
        None => text,
    };
    let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = collapsed.chars().take(max).collect();
    if collapsed.chars().count() > max {
        out.push('…');
    }
    out
}

/// Markdown files under one directory as `{name, snippet, bytes, modified}`.
fn markdown_cards(dir: &Path, snippet_len: usize) -> Vec<Value> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut rows: Vec<Value> = entries
        .flatten()
        .take(MAX_DIR_ENTRIES)
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "md") {
                return None;
            }
            let text = read_head(&path)?;
            Some(json!({
                "name": path.file_stem().map(|s| s.to_string_lossy().into_owned())?,
                "snippet": snippet(&text, snippet_len),
                // From metadata, not from `text.len()`: the read is capped but
                // the size the dashboard displays must stay the real one.
                "bytes": file_len(&path),
                "modified_unix": mtime_unix(&path),
            }))
        })
        .collect();
    rows.sort_by_key(|r| -r["modified_unix"].as_i64().unwrap_or(0));
    rows
}

/// Workspace memories: `.stella/memories/*.md`.
pub fn memories(workspace_root: &Path) -> Value {
    json!(markdown_cards(
        &workspace_root.join(".stella/memories"),
        400
    ))
}

/// The handle `stella context list` prints beside a record, derived from its
/// lineage: `ctx.stella.rust-toolchain-pin` under set `stella` reads back as
/// `rust-toolchain-pin`. A lineage that does not carry the set's prefix is its
/// own handle, so an externally-authored record still names itself.
fn record_handle(lineage_id: &str, set_id: &str) -> String {
    let prefix = format!("ctx.{set_id}.");
    lineage_id
        .strip_prefix(&prefix)
        .unwrap_or(lineage_id)
        .to_string()
}

/// One card per `[[record]]` entry in a published context record TOML file
/// (`ctx.stella.*.toml`), carrying the fields the Rules panel shows.
///
/// Cards carry the same `name`/`snippet`/`modified_unix` shape a markdown card
/// does, because the page renders one list from both and reads those three
/// keys; the record-only fields ride alongside so the panel can show what
/// actually steers (force, enforcement) rather than just a title.
///
/// Malformed files, and files without a `[[record]]` array, contribute no
/// cards rather than erroring — this is a best-effort dashboard view, not a
/// validator.
fn toml_record_cards(path: &Path) -> Vec<Value> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(parsed) = text.parse::<toml::Table>() else {
        return Vec::new();
    };
    let Some(records) = parsed.get("record").and_then(|r| r.as_array()) else {
        return Vec::new();
    };
    let set_id = parsed.get("set_id").and_then(|v| v.as_str()).unwrap_or("");
    let modified_unix = mtime_unix(path);
    let bytes = file_len(path);
    records
        .iter()
        .filter_map(|record| {
            let table = record.as_table()?;
            let lineage_id = table.get("lineage_id")?.as_str()?.to_string();
            let kind = table.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            let statement = table
                .get("statement")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let tags: Vec<&str> = table
                .get("tags")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|t| t.as_str()).collect())
                .unwrap_or_default();
            let steering_force = table
                .get("steering")
                .and_then(|s| s.as_table())
                .and_then(|s| s.get("force"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let enforcement_mode = table
                .get("enforcement")
                .and_then(|e| e.as_table())
                .and_then(|e| e.get("mode"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            Some(json!({
                "name": record_handle(&lineage_id, set_id),
                "snippet": snippet(statement, 400),
                "bytes": bytes,
                "modified_unix": modified_unix,
                "lineage_id": lineage_id,
                "kind": kind,
                "statement": statement,
                "tags": tags,
                "steering_force": steering_force,
                "enforcement_mode": enforcement_mode,
            }))
        })
        .collect()
}

/// Filenames under `.stella/rules/` that carry a rule extension but are not
/// rules. Mirrors `RESERVED_RULE_FILENAMES` in `stella-cli`'s rule loader
/// (`crates/stella-cli/src/rules.rs`) — `governance.toml` sets the governance
/// mode, and reading it as a record would show the dashboard a rule the engine
/// never loads. `promotions.jsonl` needs no entry: its extension is not one we
/// read.
const RESERVED_RULE_FILENAMES: &[&str] = &["governance.toml"];

/// Rule files: `.stella/rules/*.md` and the published context records at
/// `.stella/rules/ctx.stella.*.toml` (the db-promoted rules live in
/// [`crate::db::Observatory::memory`]'s payload).
///
/// Both extensions are read because the engine reads both — `RULE_EXTENSIONS`
/// in `stella-cli`'s loader is `["md", "toml"]`. Serving only markdown made
/// the panel under-report what actually steers the session, which for a
/// workspace whose whole steering policy is published as TOML records meant an
/// empty panel beside two enforced rules.
pub fn rules_files(workspace_root: &Path) -> Value {
    let dir = workspace_root.join(".stella/rules");
    let mut rows = markdown_cards(&dir, 400);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return json!(rows);
    };
    for entry in entries.flatten().take(MAX_DIR_ENTRIES) {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        let reserved = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| RESERVED_RULE_FILENAMES.contains(&n));
        if reserved {
            continue;
        }
        rows.extend(toml_record_cards(&path));
    }
    rows.sort_by_key(|r| -r["modified_unix"].as_i64().unwrap_or(0));
    json!(rows)
}

/// The governance mode from `.stella/rules/governance.toml` (`solo` / `team` /
/// `regulated`), or `None` when the file is absent or unreadable.
pub fn governance_mode(workspace_root: &Path) -> Option<String> {
    let text =
        std::fs::read_to_string(workspace_root.join(".stella/rules/governance.toml")).ok()?;
    let parsed = text.parse::<toml::Table>().ok()?;
    Some(parsed.get("mode")?.as_str()?.to_string())
}

/// The enforcement-promotion ledger, `.stella/rules/promotions.jsonl` — the
/// hash-chained record of every accountable change to what a published context
/// record is allowed to enforce.
///
/// This is a **different ledger** from the `promotion_event` records
/// [`crate::context_db`] folds out of `context.db`. Those are the adaptive
/// loop's own keep/ignore/retire decisions over induced proposals; this one is
/// the human governance act that moves a *published* record between advisory
/// and blocking, and it travels with the repository because a record only
/// steers a teammate if its authority does too. Serving only the first left
/// the dashboard claiming "no governance decisions recorded" for a workspace
/// whose steering policy had been promoted by a named approver.
///
/// Fields are emitted by allowlist rather than passed through: the file is
/// operator-authored text, and only the chain's own vocabulary belongs in the
/// browser.
pub fn promotions(workspace_root: &Path) -> Value {
    let path = workspace_root.join(".stella/rules/promotions.jsonl");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return json!([]);
    };
    let mut rows: Vec<Value> = text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|v| v["lineage_id"].is_string())
        .map(|v| {
            json!({
                "seq": v["seq"].as_i64(),
                "at": v["at"].as_str().unwrap_or(""),
                "lineage_id": v["lineage_id"].as_str().unwrap_or(""),
                "from": v["from"].as_str().unwrap_or(""),
                "to": v["to"].as_str().unwrap_or(""),
                "approver": v["approver"].as_str().unwrap_or(""),
                "mode": v["mode"].as_str().unwrap_or(""),
                "reason": snippet(v["reason"].as_str().unwrap_or(""), 400),
            })
        })
        .collect();
    // Newest first, matching every other feed on the page. `seq` is the chain's
    // own order and is authoritative; `at` is operator-supplied text.
    rows.sort_by_key(|r| -r["seq"].as_i64().unwrap_or(0));
    json!(rows)
}

/// Distilled lessons from `.stella/private/reflections.jsonl` — one object per line,
/// `{lesson, domains, occurred_at}`. Unparseable lines are skipped.
pub fn lessons(workspace_root: &Path) -> Value {
    let path = workspace_root.join(".stella/private/reflections.jsonl");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return json!([]);
    };
    let mut rows: Vec<Value> = text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|v| v["lesson"].is_string())
        .collect();
    rows.sort_by_key(|r| -r["occurred_at"].as_i64().unwrap_or(0));
    json!(rows)
}

/// Configured MCP servers from the project's `.stella/mcp.toml`. Env var
/// values and header values are never included — only their names — and the
/// `target` field is likewise served without its value-bearing parts: a stdio
/// server shows its command name and argument *count* (args routinely carry
/// credentials — `--token=…`, an API key as a positional), an http server its
/// URL through [`redacted_url`].
pub fn mcp_servers(workspace_root: &Path) -> Value {
    let path = workspace_root.join(".stella/mcp.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return json!({ "path": path.display().to_string(), "servers": [] });
    };
    let parsed: toml::Table = match toml::from_str(&text) {
        Ok(v) => v,
        Err(_) => {
            return json!({
                "path": path.display().to_string(),
                "servers": [],
                "parse_error": true,
            });
        }
    };
    let mut servers = Vec::new();
    if let Some(table) = parsed.get("servers").and_then(|s| s.as_table()) {
        for (name, def) in table {
            let transport = def
                .get("transport")
                .and_then(|t| t.as_str())
                .unwrap_or("unknown");
            let target = match transport {
                "stdio" => {
                    let cmd = def.get("cmd").and_then(|c| c.as_str()).unwrap_or("");
                    // The command name and the argument *count*, never the
                    // argument values: the count keeps the row legible ("did
                    // my args reach the config?") without serving what they
                    // hold. Same fail-closed posture as [`redact`].
                    let args = def
                        .get("args")
                        .and_then(|a| a.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    match args {
                        0 => cmd.to_string(),
                        1 => format!("{cmd} (1 arg)").trim().to_string(),
                        n => format!("{cmd} ({n} args)").trim().to_string(),
                    }
                }
                "http" => def
                    .get("url")
                    .and_then(|u| u.as_str())
                    .map(redacted_url)
                    .unwrap_or_default(),
                _ => String::new(),
            };
            let key_names = |field: &str| -> Vec<String> {
                def.get(field)
                    .and_then(|e| e.as_table())
                    .map(|t| t.keys().cloned().collect())
                    .unwrap_or_default()
            };
            servers.push(json!({
                "name": name,
                "transport": transport,
                "target": target,
                "env_keys": key_names("env"),
                "header_keys": key_names("headers"),
            }));
        }
    }
    json!({ "path": path.display().to_string(), "servers": servers })
}

/// An `mcp.toml` URL with everything credential-shaped removed: the query and
/// fragment (`?api_key=…`) and any userinfo (`user:secret@host`). Scheme, host
/// and path are what the panel needs to tell servers apart; the rest is where
/// a URL carries credentials, so it never reaches the browser.
fn redacted_url(url: &str) -> String {
    let base = url.split(['?', '#']).next().unwrap_or(url);
    match base.split_once("://") {
        Some((scheme, rest)) => {
            let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
            let host = authority
                .rsplit_once('@')
                .map_or(authority, |(_, host)| host);
            format!("{scheme}://{host}{path}")
        }
        None => base.to_string(),
    }
}

/// Does this (lowercased) key look like it holds a credential? Keys ending
/// `_env` are exempt — they name an environment variable, they don't hold
/// its value.
fn sensitive_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    if k.ends_with("_env") {
        return false;
    }
    ["key", "token", "secret", "password", "credential", "bearer"]
        .iter()
        .any(|marker| k.contains(marker))
        || k == "authorization"
}

/// Recursively scrub credentials from parsed settings before serving them.
/// A sensitive key — or an `env`/`headers` map, whose values are credentials
/// by position — opens a *credential scope*, and every string at or below it
/// is replaced.
///
/// The scope has to be inherited, not recomputed per level: settings are
/// arbitrary user JSON, so a secret can sit one container deeper than the
/// key that names it (`{"api_keys": ["sk-live-…"]}`,
/// `{"env": {"TOKEN": {"value": "…"}}}`). Redacting only the direct string
/// child leaked both. Fail closed — an over-redacted `<redacted>` on the
/// config tab costs nothing; a leaked key costs everything.
fn redact(value: &mut Value, in_credential_scope: bool) {
    match value {
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                let scoped = in_credential_scope
                    || sensitive_key(key)
                    || matches!(key.as_str(), "env" | "headers");
                if scoped && v.is_string() {
                    *v = Value::String("<redacted>".into());
                } else {
                    redact(v, scoped);
                }
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                if in_credential_scope && item.is_string() {
                    *item = Value::String("<redacted>".into());
                } else {
                    redact(item, in_credential_scope);
                }
            }
        }
        _ => {}
    }
}

/// One scope of the settings chain: its path, whether it exists, and its
/// redacted contents.
fn settings_scope(scope: &str, path: PathBuf) -> Value {
    let exists = path.is_file();
    let mut body = Value::Null;
    let mut parse_error = false;
    if exists {
        match std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        {
            Some(mut parsed) => {
                redact(&mut parsed, false);
                body = parsed;
            }
            None => parse_error = true,
        }
    }
    json!({
        "scope": scope,
        "path": path.display().to_string(),
        "exists": exists,
        "parse_error": parse_error,
        "settings": body,
    })
}

/// The full configuration picture: the settings scope chain (ascending
/// precedence: user → org-managed → project) plus where every store this
/// dashboard reads actually lives.
pub fn config(workspace_root: &Path) -> Value {
    let user = user_config_dir()
        .map(|d| d.join("settings.json"))
        .unwrap_or_else(|| PathBuf::from("settings.json"));
    let scopes = vec![
        settings_scope("user", user),
        settings_scope("managed", managed_settings_path()),
        settings_scope(
            "project",
            workspace_root.join(".stella").join("settings.json"),
        ),
    ];
    let dot = workspace_root.join(".stella");
    let store_path = |name: &str| {
        let p = dot.join("private").join(name);
        json!({ "path": p.display().to_string(), "exists": p.exists() })
    };
    let usage = crate::global::usage_db_path();
    json!({
        "scopes": scopes,
        "stores": {
            "store_db": store_path("store.db"),
            "fleet_db": store_path("fleet.db"),
            "codegraph_db": store_path("codegraph.db"),
            "context_db": store_path("context.db"),
            "usage_db": {
                "path": usage.display().to_string(),
                "exists": usage.exists(),
            },
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Rules panel reads `.stella/rules/*`, but the repository's real
    /// context records there are `ctx.stella.*.toml` files, not markdown —
    /// `rules_files` must parse the `[[record]]` entries in those TOML files
    /// (lineage_id, kind, statement, tags, steering force, enforcement mode)
    /// while still returning cards for any plain `.md` file in the directory.
    #[test]
    fn rules_files_parses_toml_context_records_alongside_markdown() {
        let dir = tempfile::tempdir().unwrap();
        let rules_dir = dir.path().join(".stella/rules");
        std::fs::create_dir_all(&rules_dir).unwrap();

        std::fs::write(
            rules_dir.join("ctx.stella.test-rule.toml"),
            r#"
schema = "context-record/v0.1"
set_id = "stella"

[[record]]
lineage_id = "ctx.stella.test-rule"
kind       = "constraint"
statement  = "Tests must pin something."
tags       = ["testing", "pins"]

  [record.steering]
  force      = "must"
  precedence = 90

  [record.enforcement]
  mode = "hard"
"#,
        )
        .unwrap();

        std::fs::write(
            rules_dir.join("hand-written.md"),
            "# A rule\nSome body text.",
        )
        .unwrap();
        // Carries a rule extension but is not a rule; the engine's loader
        // reserves it and so must this view.
        std::fs::write(rules_dir.join("governance.toml"), "mode = \"regulated\"\n").unwrap();

        let rows = rules_files(dir.path());
        let rows = rows.as_array().expect("rules_files returns an array");

        let toml_card = rows
            .iter()
            .find(|r| r["lineage_id"] == "ctx.stella.test-rule")
            .expect("toml record card present");
        assert_eq!(toml_card["kind"], "constraint");
        assert_eq!(toml_card["statement"], "Tests must pin something.");
        assert_eq!(toml_card["tags"], serde_json::json!(["testing", "pins"]));
        assert_eq!(toml_card["steering_force"], "must");
        assert_eq!(toml_card["enforcement_mode"], "hard");

        // The page renders one list from both kinds and reads these three keys
        // off every row, so a record card without them renders blank — the
        // failure no Rust gate can see.
        assert_eq!(
            toml_card["name"], "test-rule",
            "handle, as `stella context list` prints it"
        );
        assert_eq!(toml_card["snippet"], "Tests must pin something.");
        assert!(
            toml_card["modified_unix"].as_i64().is_some(),
            "modified_unix: {:?}",
            toml_card["modified_unix"]
        );

        assert!(
            !rows.iter().any(|r| r["name"] == "governance"),
            "governance.toml is reserved, not a record: {rows:?}"
        );

        let md_card = rows
            .iter()
            .find(|r| r["name"] == "hand-written")
            .expect(".md cards still work");
        assert!(
            md_card["snippet"]
                .as_str()
                .unwrap()
                .contains("Some body text."),
            "md snippet: {:?}",
            md_card["snippet"]
        );
    }

    /// The card read is bounded, but `bytes` must still be the file's real
    /// size — otherwise bounding the read would silently start lying to the
    /// dashboard about how big a memory is, which is the objection that
    /// deferred this row in the first place.
    #[test]
    fn a_huge_card_is_read_bounded_but_still_reports_its_true_size() {
        let dir = tempfile::tempdir().unwrap();
        let big = "x".repeat(300 * 1024);
        std::fs::write(dir.path().join("huge.md"), &big).unwrap();

        let rows = markdown_cards(dir.path(), 40);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0]["bytes"].as_u64(),
            Some(big.len() as u64),
            "reported size comes from metadata, not from the capped read"
        );
        let snippet = rows[0]["snippet"].as_str().unwrap();
        assert!(
            snippet.chars().count() <= 41,
            "snippet stays capped: {} chars",
            snippet.chars().count()
        );
    }

    /// A directory that has grown pathological must degrade the dashboard, not
    /// stall the request that walks it synchronously.
    #[test]
    fn a_pathological_directory_scan_stops_at_the_entry_bound() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(MAX_DIR_ENTRIES + 50) {
            std::fs::write(dir.path().join(format!("m{i:04}.md")), "body").unwrap();
        }
        let rows = markdown_cards(dir.path(), 40);
        assert!(
            rows.len() <= MAX_DIR_ENTRIES,
            "scan must stop at the bound, got {} rows",
            rows.len()
        );
    }

    #[test]
    fn a_bounded_read_never_exceeds_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.md");
        std::fs::write(&path, "y".repeat(500 * 1024)).unwrap();
        let text = read_head(&path).unwrap();
        assert!(
            text.len() as u64 <= MAX_SNIPPET_READ_BYTES,
            "read {} bytes, cap is {MAX_SNIPPET_READ_BYTES}",
            text.len()
        );
    }

    #[test]
    fn frontmatter_extracts_name_and_description() {
        let text = "---\nname: my-skill\ndescription: \"Does a thing\"\n---\n# Body";
        let (name, desc, origin) = frontmatter_fields(text);
        assert_eq!(name.as_deref(), Some("my-skill"));
        assert_eq!(desc.as_deref(), Some("Does a thing"));
        assert_eq!(origin, None);
        assert_eq!(frontmatter_fields("no frontmatter"), (None, None, None));
    }

    /// A mined skill is flagged `learned` from the `origin: auto` its own
    /// writer stamped, wherever the file happens to sit. The miner writes flat
    /// into `.stella/skills/`, so requiring a `learned/` parent directory —
    /// which nothing in the codebase ever creates — reported every promoted
    /// skill as hand-authored.
    #[test]
    fn mined_skill_is_learned_from_origin_not_directory_position() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("money-is-minor-units-a1b2c3d4.md"),
            "---\nname: money-is-minor-units\ndescription: d\norigin: auto\n---\nbody",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("hand-written.md"),
            "---\nname: hand-written\ndescription: d\n---\nbody",
        )
        .unwrap();
        let mut rows = Vec::new();
        scan_skills_dir(dir.path(), "project", &HashMap::new(), &mut rows);
        let find = |name: &str| {
            rows.iter()
                .find(|r| r["name"] == name)
                .unwrap_or_else(|| panic!("{name} listed"))
                .clone()
        };
        assert_eq!(find("money-is-minor-units")["learned"], true);
        assert_eq!(find("hand-written")["learned"], false);
    }

    /// A learned skill's dashboard row names the grade of the evidence that
    /// promoted it (#4871), joined out of `context.db`'s ledger by candidate
    /// id — never by touching the `SKILL.md` bytes, which the byte-identity
    /// guarantee (`stella-cli`'s `learning::guarantees`) requires stay
    /// silent on it. A hand-authored skill was never a candidate and carries
    /// no grade at all.
    #[test]
    fn skills_names_the_evidence_grade_behind_a_learned_skill() {
        use stella_context::{ContextStore, LedgerAppend};
        use stella_core::context_record::{
            ContextRecordKind, EvidencePool, LIFECYCLE_SCHEMA_VERSION, ObservationRecord,
            ObservationSource, ProposalRecord, ProposalScore, RecordProposalKind,
            RecordProposalStatus, confidence_from_score,
        };

        let dir = tempfile::tempdir().unwrap();
        let workspace_root = dir.path();
        let skills_dir = workspace_root.join(".stella").join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(
            skills_dir.join("money-is-minor-units-a1b2c3d4.md"),
            "---\nname: money-is-minor-units-a1b2c3d4\ndescription: d\norigin: auto\n---\nbody",
        )
        .unwrap();
        std::fs::write(
            skills_dir.join("hand-written.md"),
            "---\nname: hand-written\ndescription: d\n---\nbody",
        )
        .unwrap();

        let private = workspace_root.join(".stella").join("private");
        std::fs::create_dir_all(&private).unwrap();
        let store = ContextStore::open(private.join("context.db")).unwrap();

        let observation = ObservationRecord::new(
            ObservationSource::ToolOutcome,
            "tool:cargo_test#1",
            "turn:1",
            "money amounts must be stored as minor units",
            Vec::new(),
            false,
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        let score = ProposalScore {
            occurrences: 3,
            distinct_tasks: 3,
            salient: false,
            rank: 30.0,
        };
        let confidence = confidence_from_score(&score).unwrap();
        let proposal = ProposalRecord::new(
            RecordProposalKind::Knowledge,
            RecordProposalStatus::Eligible,
            "money-is-minor-units-a1b2c3d4",
            "money is minor units",
            "money amounts must be stored as minor units",
            Vec::new(),
            EvidencePool::from_observations([&observation]),
            score,
            confidence,
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        let body = serde_json::to_string(&proposal).unwrap();
        store
            .append_record(LedgerAppend {
                record_id: &proposal.record_id,
                lineage_id: &proposal.lineage_id,
                record_kind: ContextRecordKind::RecordProposal.as_str(),
                record_hash: &proposal.record_hash,
                schema_version: LIFECYCLE_SCHEMA_VERSION,
                body: &body,
                observed_at: &proposal.observed_at,
                supersedes: None,
            })
            .unwrap();
        drop(store);

        let rows = skills(workspace_root);
        let rows = rows.as_array().expect("skills returns an array");
        let find = |name: &str| {
            rows.iter()
                .find(|r| r["name"] == name)
                .unwrap_or_else(|| panic!("{name} listed"))
                .clone()
        };
        assert_eq!(
            find("money-is-minor-units-a1b2c3d4")["evidence_grade"],
            "environment_observation"
        );
        assert_eq!(
            find("hand-written")["evidence_grade"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn redact_scrubs_credentials_but_keeps_env_var_names() {
        let mut v = serde_json::json!({
            "providers": { "zai": { "api_key": "sk-live-123", "api_key_env": "ZAI_KEY" } },
            "hooks": [ { "env": { "GITHUB_TOKEN": "ghp_abc" } } ],
            "mcp": { "headers": { "Authorization": "Bearer xyz" } },
            "model": "glm-5.2",
        });
        redact(&mut v, false);
        let s = v.to_string();
        assert!(!s.contains("sk-live-123"));
        assert!(!s.contains("ghp_abc"));
        assert!(!s.contains("Bearer xyz"));
        assert!(s.contains("ZAI_KEY"), "env var *names* survive");
        assert!(s.contains("glm-5.2"), "non-sensitive values survive");
    }

    /// The `target` field is served to the browser, so the value-bearing
    /// parts of a URL — query, fragment, userinfo — must never survive.
    #[test]
    fn redacted_url_strips_query_fragment_and_userinfo() {
        assert_eq!(
            redacted_url("https://mcp.example/v1?api_key=sk-live-123"),
            "https://mcp.example/v1"
        );
        assert_eq!(
            redacted_url("https://user:hunter2@mcp.example/v1#frag"),
            "https://mcp.example/v1"
        );
        assert_eq!(redacted_url("https://mcp.example"), "https://mcp.example");
        // No scheme at all: still nothing past a `?` is served.
        assert_eq!(redacted_url("mcp.example/v1?t=s3cret"), "mcp.example/v1");
    }

    /// The config tab must name the settings file the CLI actually loads
    /// (`stella_cli::paths::stella_root`). Since #2178 that is `$STELLA_HOME`
    /// when set, else `$HOME/.stella` — and since #2442 there is no second
    /// override to mirror: this test used to set `STELLA_CONFIG_DIR` and
    /// assert it was ignored, which stopped being a claim about anything once
    /// the variable named no resolver on either side and was retired.
    #[test]
    fn user_config_dir_mirrors_the_cli_settings_loader() {
        // The hazard is the shared environment, not any one variable: this
        // test reads HOME, which other crates' tests override. Take the
        // crate-wide lock like every other env-mutating test here (#1137).
        let _env = crate::test_env::lock();
        let _restore = crate::test_env::EnvRestore::capture(&["STELLA_HOME"]);
        // SAFETY: the lock is held for the whole test, and `_restore` undoes
        // this on drop — including if `user_config_dir` panics.
        unsafe {
            std::env::remove_var("STELLA_HOME");
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("HOME set");
        assert_eq!(
            user_config_dir(),
            Some(home.join(".stella")),
            "with nothing overriding it, the tab names the loader's default"
        );

        // SAFETY: same lock, same guard.
        unsafe { std::env::set_var("STELLA_HOME", "/tmp/stella-2178-observatory") };
        assert_eq!(
            user_config_dir(),
            Some(PathBuf::from("/tmp/stella-2178-observatory")),
            "STELLA_HOME does move the loader's root (#2178), so a dashboard \
             that ignored it would name a settings file the CLI never opens"
        );
    }

    /// A secret one container below the key that names it used to survive:
    /// only the *direct* string child of a sensitive key was replaced.
    #[test]
    fn redact_follows_credentials_into_arrays_and_nested_tables() {
        let mut v = serde_json::json!({
            "api_keys": ["sk-live-one", "sk-live-two"],
            "env": { "TOKEN": { "value": "ghp_nested" } },
            "models": ["glm-5.2"],
        });
        redact(&mut v, false);
        let s = v.to_string();
        assert!(!s.contains("sk-live-one"), "arrays under a sensitive key");
        assert!(
            !s.contains("sk-live-two"),
            "every element, not just the first"
        );
        assert!(!s.contains("ghp_nested"), "nested tables under env");
        assert!(s.contains("glm-5.2"), "ordinary lists are untouched");
    }
}
