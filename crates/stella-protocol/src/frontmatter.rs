// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The one header parser for markdown files.
//!
//! A skill, a rule, an agent and a slash command are all one markdown file
//! with a `---` header. [`parse_frontmatter`] splits that header off the
//! body. It reads the header as one `key: value` pair per line. It is not a
//! YAML parser. When it meets shape it cannot hold, it says so rather than
//! guess.
//!
//! It sits here because four modules read one of those files, and they no
//! longer share a crate: `rules` and `skills` in `stella-learn`, plus
//! `extensions` and `skill_invocation` in `stella-core`. One parser means a
//! `SKILL.md` and a rule file cannot read their own headers two ways.

use std::collections::HashMap;

/// Frontmatter split from a markdown file's body (TS: `Frontmatter`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontmatter {
    pub data: HashMap<String, String>,
    /// Keys **indented under another key**. That is a nested map, and one
    /// line at a time cannot hold it.
    ///
    /// Noted here, acted on elsewhere: skills, rules and extensions all read
    /// this, and a nested key means a different thing to each of them.
    /// `stella_learn::rules::rule_from_file` refuses a rule that has any.
    ///
    /// Why it matters (ADR 0011, Consequences): the parser drops the
    /// indent. So this example from `docs/spec/adaptive-context/context-pr.md` §6.1
    ///
    /// ```text
    /// scope:
    ///   repository_id: repo_stella
    /// ```
    ///
    /// would raise `repository_id` up beside `record_id`, leave `scope`
    /// empty, and say nothing. The record loads wearing a scope it does not
    /// have. That is the shape of a guard script that prints OK and skips
    /// most of its input.
    pub nested_keys: Vec<String>,
    /// The top-level keys those nested children sat under, first parent
    /// first.
    ///
    /// A caller that decides per field needs the parent, not the child. ADR
    /// 0025 refuses a nested value where reading it wrong would widen what the
    /// file may do. `tools:` is such a key. `description:` is not.
    /// `nested_keys` cannot tell them apart, so a caller reading it alone
    /// refuses an agent for nesting under any key at all.
    ///
    /// A set of parents, not pairs. Every caller so far asks whether one named
    /// key was nested. Pairs answer that no better, and they make the common
    /// `is_empty` read longer.
    pub nested_parents: Vec<String>,
    pub body: String,
}

/// Strip one pair of matching surrounding quotes (`"…"` or `'…'`).
pub(crate) fn strip_matched_quotes(value: &str) -> &str {
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

/// Split `---\n…\n---\nbody` into header pairs plus body text.
///
/// With no `---` fence, the whole trimmed input is the body and the header
/// is empty. It ports `parseFrontmatter` from `markdown-registry.ts`: strip
/// a leading BOM, strip one pair of quotes off a value. It adds one thing.
/// A key with no value, followed by `- item` lines, folds onto that key as
/// a comma-joined string. So a list field reads the same way no matter how
/// the author wrote it.
///
/// A key **indented under another key** goes to
/// [`Frontmatter::nested_keys`], and the key it sat under goes to
/// [`Frontmatter::nested_parents`]. Neither is raised to the top level. The
/// first field's own docs say why raising it was a bug.
pub fn parse_frontmatter(raw: &str) -> Frontmatter {
    let text = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    if !text.starts_with("---") {
        return Frontmatter {
            body: text.trim().to_string(),
            ..Frontmatter::default()
        };
    }
    let Some(rel_end) = text.get(3..).and_then(|rest| rest.find("\n---")) else {
        return Frontmatter {
            body: text.trim().to_string(),
            ..Frontmatter::default()
        };
    };
    let end = 3 + rel_end;
    let header = text[3..end].trim();
    let after_fence = &text[end + 4..];
    let body = after_fence
        .strip_prefix("\r\n")
        .or_else(|| after_fence.strip_prefix('\n'))
        .unwrap_or(after_fence)
        .trim()
        .to_string();

    let mut data = HashMap::new();
    let mut nested_keys = Vec::new();
    let mut nested_parents = Vec::new();
    // The key whose scalar value was empty on its own line — the head of a
    // possible YAML block sequence (`tools:` followed by `- Read` lines).
    let mut pending_list_key: Option<String> = None;
    // The indentation the block's own keys sit at, taken from the first key seen.
    // Anything deeper is a nested mapping. Read from the file rather than assumed
    // to be zero so a frontmatter block someone indented wholesale still parses.
    let mut base_indent: Option<usize> = None;
    // The most recent key at the base indent — the parent any deeper key
    // hangs under.
    let mut last_base_key: Option<String> = None;
    for line in header.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        // A `- item` line under an empty-valued key is a block-sequence
        // element: flatten it onto that key. Without a pending key the
        // line falls through to the scalar path (and is skipped when it
        // has no colon), exactly as before.
        if let Some(item) = trimmed.strip_prefix("- ")
            && let Some(key) = &pending_list_key
        {
            let item = strip_matched_quotes(item.trim());
            if !item.is_empty() {
                let entry: &mut String = data.entry(key.clone()).or_default();
                if !entry.is_empty() {
                    entry.push_str(", ");
                }
                entry.push_str(item);
            }
            continue;
        }
        let Some(colon) = trimmed.find(':') else {
            continue;
        };
        let key = trimmed[..colon].trim();
        let value = strip_matched_quotes(trimmed[colon + 1..].trim());
        if key.is_empty() {
            continue;
        }
        let base = *base_indent.get_or_insert(indent);
        if indent > base {
            // A nested mapping. Record it and DO NOT promote it: writing it to
            // `data` is what made a mangled record look like a valid one.
            if !nested_keys.iter().any(|seen| seen == key) {
                nested_keys.push(key.to_string());
            }
            if let Some(parent) = &last_base_key
                && !nested_parents.iter().any(|seen| seen == parent)
            {
                nested_parents.push(parent.clone());
            }
            continue;
        }
        data.insert(key.to_string(), value.to_string());
        pending_list_key = value.is_empty().then(|| key.to_string());
        last_base_key = Some(key.to_string());
    }
    Frontmatter {
        data,
        nested_keys,
        nested_parents,
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_description_guard_and_body() {
        let raw = "---\ndescription: Never edit an applied migration\nguard-tool: Edit\nguard-deny-path: packages/database/migrations/*-applied/**\n---\nAdd a new forward migration instead of editing an applied one.";
        let fm = parse_frontmatter(raw);
        assert_eq!(
            fm.data.get("description").unwrap(),
            "Never edit an applied migration"
        );
        assert_eq!(fm.data.get("guard-tool").unwrap(), "Edit");
        assert!(fm.body.contains("Add a new forward migration"));
    }

    #[test]
    fn no_fence_means_whole_trimmed_text_is_the_body() {
        let fm = parse_frontmatter("  just a plain rule, no frontmatter  ");
        assert!(fm.data.is_empty());
        assert_eq!(fm.body, "just a plain rule, no frontmatter");
    }

    #[test]
    fn strips_a_leading_bom() {
        let fm = parse_frontmatter("\u{feff}---\ndescription: d\n---\nbody text");
        assert_eq!(fm.data.get("description").unwrap(), "d");
        assert_eq!(fm.body, "body text");
    }

    #[test]
    fn strips_matching_quotes_from_values() {
        let fm = parse_frontmatter(
            "---\ndescription: \"quoted value\"\nother: 'single quoted'\n---\nbody",
        );
        assert_eq!(fm.data.get("description").unwrap(), "quoted value");
        assert_eq!(fm.data.get("other").unwrap(), "single quoted");
    }

    #[test]
    fn ignores_comment_and_blank_frontmatter_lines() {
        let fm = parse_frontmatter("---\n# a comment\n\ndescription: d\n---\nbody");
        assert_eq!(fm.data.len(), 1);
        assert_eq!(fm.data.get("description").unwrap(), "d");
    }

    #[test]
    fn flattens_block_sequences_onto_their_key() {
        let fm = parse_frontmatter(
            "---\ntools:\n  - Read\n  - 'Grep'\n  - \"Web Search\"\ndescription: d\n---\nbody",
        );
        assert_eq!(fm.data.get("tools").unwrap(), "Read, Grep, Web Search");
        assert_eq!(
            fm.data.get("description").unwrap(),
            "d",
            "the key after the sequence parses normally"
        );
    }

    #[test]
    fn dash_lines_without_a_pending_list_key_stay_ignored() {
        let fm = parse_frontmatter("---\ndescription: d\n- stray item\n---\nbody");
        assert_eq!(fm.data.len(), 1);
        assert_eq!(fm.data.get("description").unwrap(), "d");
    }

    /// A nested mapping records both halves: the child names, and the key the
    /// nesting hung under. Without the parent, a caller cannot ask whether
    /// the nested key was one that grants a capability, which is the question
    /// ADR 0025 turns on.
    #[test]
    fn a_nested_mapping_records_the_key_it_sat_under() {
        let fm = parse_frontmatter(
            "---\nname: reviewer\ntools:\n  read: true\n  write: false\ndescription: d\n---\nbody",
        );
        assert_eq!(fm.nested_keys, vec!["read", "write"]);
        assert_eq!(fm.nested_parents, vec!["tools"]);
        assert_eq!(
            fm.data.get("description").unwrap(),
            "d",
            "the key after the nesting parses normally"
        );
        assert!(
            !fm.data.contains_key("read"),
            "a nested child is still never raised to the top level"
        );
    }

    /// Two nested mappings keep their own parents, so a caller asking about
    /// one key is not answered by the other.
    #[test]
    fn each_nested_mapping_names_its_own_parent() {
        let fm =
            parse_frontmatter("---\ndescription:\n  short: s\ntools:\n  read: true\n---\nbody");
        assert_eq!(fm.nested_parents, vec!["description", "tools"]);
        assert_eq!(fm.nested_keys, vec!["short", "read"]);
    }
}
