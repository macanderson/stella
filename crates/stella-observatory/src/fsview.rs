//! Filesystem-derived views: skills, memories, rule files, distilled
//! lessons (`reflections.jsonl`), configured MCP servers (`mcp.toml`) and
//! the `settings.json` scope chain.
//!
//! Everything here is plain reads of files stella (or the user) already
//! wrote. Two invariants:
//!
//! 1. **Nothing is created or modified** — absent files and directories are
//!    states, rendered as empty sections.
//! 2. **Secrets never reach the browser.** `settings.json` and `mcp.toml`
//!    may carry credentials; [`redact`] scrubs any value under a
//!    sensitive-looking key (and every value under `env`/`headers` maps)
//!    before the JSON is serialized into a response.

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

/// The user-scope stella config dir (`~/.stella`) — `HOME` only, mirroring
/// `stella_cli::settings::user_settings_path` (the source of truth, kept as a
/// copy because the observatory deliberately links no `stella-*` crate), so
/// the config tab names the file the CLI actually loads and the skills tab the
/// directory it actually scans.
///
/// Deliberately `HOME` and nothing else: the CLI's settings loader reads
/// neither `STELLA_CONFIG_DIR` nor `STELLA_HOME`, so honouring either here
/// (as this once did for `STELLA_CONFIG_DIR`) made the tab claim a
/// `settings.json` the CLI never opens. If the loader ever grows an override,
/// mirror it here in the same order.
pub fn user_config_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".stella"))
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
fn skill_entry(path: &Path, scope: &str, in_learned_dir: bool) -> Option<Value> {
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
    Some(json!({
        "name": name.unwrap_or(fallback),
        "description": description.unwrap_or_default(),
        "scope": scope,
        "learned": learned,
        "path": path.display().to_string(),
        "modified_unix": mtime_unix(path),
    }))
}

/// Scan one skills root: directories holding `SKILL.md`, flat `*.md` files,
/// and the `learned/` subdirectory. Note that a self-extracted skill is
/// recognized by its `origin: auto` frontmatter rather than by landing in
/// `learned/` — see [`skill_entry`] — so the flat-file branch below yields
/// learned skills too, which is where the miner actually writes them.
fn scan_skills_dir(dir: &Path, scope: &str, out: &mut Vec<Value>) {
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
                            out.extend(skill_entry(&p, scope, true));
                        }
                    }
                }
            } else {
                let manifest = path.join("SKILL.md");
                if manifest.is_file() {
                    out.extend(skill_entry(&manifest, scope, false));
                }
            }
        } else if path.extension().is_some_and(|e| e == "md") {
            // `extend`, not `push(…unwrap_or_default())`: an unreadable file is
            // a skipped row, never a JSON `null` the dashboard would try to
            // render as a skill.
            out.extend(skill_entry(&path, scope, false));
        }
    }
}

/// Every skill visible to this workspace: project `.stella/skills` plus the
/// user-scope skills dir, with learned (self-extracted) skills flagged.
pub fn skills(workspace_root: &Path) -> Value {
    let mut rows = Vec::new();
    scan_skills_dir(&workspace_root.join(".stella/skills"), "project", &mut rows);
    if let Some(user) = user_config_dir() {
        scan_skills_dir(&user.join("skills"), "user", &mut rows);
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

/// Does this manifest key name a path *inside* the workspace? An absolute path
/// makes [`Path::join`] drop the workspace root entirely and `..` walks out of
/// it; a leading `./` is how a model-authored evidence path often arrives and
/// is harmless, so it stays allowed.
///
/// Lexical on purpose — the observatory does not resolve or `stat` a path it is
/// refusing, so unlike `stella_tools::resolve_within_root` (the writer-side
/// twin, which canonicalises) this does not follow a symlink that points out of
/// the tree. That is the narrower guarantee: no path *spelled* outside the
/// workspace is read.
fn is_workspace_relative(rel: &str) -> bool {
    use std::path::Component;
    let path = Path::new(rel);
    if !path.is_relative() {
        return false;
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_) | Component::CurDir) {
            return false;
        }
    }
    true
}

/// Exploration maps from `.stella/explorations/*.json` with a per-map
/// freshness verdict computed by re-hashing each record's `path → sha256`
/// manifest against the working tree — the human-facing twin of the agents'
/// startup index (`docs/spec/exploration-sharing.md` §4e). Records without
/// a manifest (pre-v2) report `"unknown"`.
pub fn explorations(workspace_root: &Path) -> Value {
    use sha2::{Digest, Sha256};
    let dir = workspace_root.join(".stella/explorations");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return json!([]);
    };
    let mut rows: Vec<Value> = Vec::new();
    // Bounded like every other directory walk in this module (`MAX_DIR_ENTRIES`):
    // each record re-hashes its whole manifest against the working tree, so an
    // unbounded scan here is the most expensive one in the file, not the cheapest.
    for entry in entries.flatten().take(MAX_DIR_ENTRIES) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(record) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        else {
            continue;
        };
        let manifest = record["manifest"].as_object().cloned().unwrap_or_default();
        let (mut changed, mut missing) = (Vec::new(), Vec::new());
        for (rel, saved) in &manifest {
            // An exploration record is a shareable artifact that travels with
            // the tree (docs/spec/exploration-sharing.md §3), so its manifest
            // keys are untrusted text. `Path::join` discards the root when
            // handed an absolute path, and `..` walks out of the workspace
            // either way: both would turn a freshness poll into an
            // arbitrary-file read whose verdict reports whether that file exists
            // and whether its bytes hash to an attacker-chosen digest. The
            // producer already refuses such a key (`stella_tools::staleness`);
            // this side must too. It reads as missing — the same verdict a
            // deleted file gets — without being opened.
            if !is_workspace_relative(rel) {
                missing.push(rel.clone());
                continue;
            }
            match std::fs::read(workspace_root.join(rel)) {
                Ok(bytes) => {
                    let mut hasher = Sha256::new();
                    hasher.update(&bytes);
                    let digest = hasher.finalize();
                    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
                    if Some(hex.as_str()) != saved.as_str() {
                        changed.push(rel.clone());
                    }
                }
                Err(_) => missing.push(rel.clone()),
            }
        }
        let freshness = if manifest.is_empty() {
            "unknown"
        } else if changed.is_empty() && missing.is_empty() {
            "fresh"
        } else {
            "drifted"
        };
        rows.push(json!({
            "slice": record["slice"],
            "title": record["title"],
            "summary": record["summary"],
            "status": record["status"].as_str().unwrap_or("complete"),
            "pid": record["pid"],
            "created_at_ms": record["created_at_ms"],
            "git_head": record["git_head"],
            "manifest_files": manifest.len(),
            "freshness": freshness,
            "changed": changed,
            "missing": missing,
            "content_chars": record["content"].as_str().map(|s| s.chars().count()).unwrap_or(0),
        }));
    }
    rows.sort_by_key(|r| -(r["created_at_ms"].as_i64().unwrap_or(0)));
    json!(rows)
}

/// Rule files: `.stella/rules/*.md` (the db-promoted rules live in
/// [`crate::db::Observatory::memory`]'s payload).
pub fn rules_files(workspace_root: &Path) -> Value {
    json!(markdown_cards(&workspace_root.join(".stella/rules"), 400))
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
        scan_skills_dir(dir.path(), "project", &mut rows);
        let find = |name: &str| {
            rows.iter()
                .find(|r| r["name"] == name)
                .unwrap_or_else(|| panic!("{name} listed"))
                .clone()
        };
        assert_eq!(find("money-is-minor-units")["learned"], true);
        assert_eq!(find("hand-written")["learned"], false);
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

    /// Manifest keys come out of a shareable artifact, so an ingested map can
    /// name a path outside the workspace. Anything but a plain relative path
    /// must be refused before it reaches `fs::read`.
    #[test]
    fn manifest_keys_outside_the_workspace_are_refused() {
        assert!(is_workspace_relative("src/lib.rs"));
        assert!(is_workspace_relative("a/b/c.rs"));
        assert!(!is_workspace_relative("/etc/passwd"));
        assert!(!is_workspace_relative("../../.ssh/id_rsa"));
        assert!(!is_workspace_relative("src/../../secrets"));
        // A leading `./` is how model-authored evidence paths often arrive.
        assert!(is_workspace_relative("./src/lib.rs"));
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
    /// (`stella_cli::settings::user_settings_path`, `$HOME/.stella`). The
    /// loader reads neither `STELLA_CONFIG_DIR` nor `STELLA_HOME`, so this
    /// resolver has to ignore both — honouring `STELLA_CONFIG_DIR` here once
    /// pointed the tab at a `settings.json` the CLI never opens.
    #[test]
    fn user_config_dir_mirrors_the_cli_settings_loader() {
        // The old note here reasoned that no parallel test could observe the
        // transient value because nothing else reads STELLA_CONFIG_DIR. That
        // is an argument about one variable, and the hazard is the shared
        // environment: this test also reads HOME, which other crates' tests
        // override, and stella-store's `any_override_set` reads
        // STELLA_CONFIG_DIR. Take the crate-wide lock like every other
        // env-mutating test here (#1137).
        let _env = crate::test_env::lock();
        let _restore = crate::test_env::EnvRestore::capture(&["STELLA_CONFIG_DIR"]);
        // SAFETY: the lock is held for the whole test, and `_restore` undoes
        // this on drop — including if `user_config_dir` panics.
        unsafe { std::env::set_var("STELLA_CONFIG_DIR", "/tmp/elsewhere") };
        let resolved = user_config_dir();
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("HOME set");
        assert_eq!(resolved, Some(home.join(".stella")));
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
