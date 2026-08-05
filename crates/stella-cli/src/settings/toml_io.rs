//! Comment-preserving read-modify-write for `stella.toml`.
//!
//! # Why this module exists at all
//!
//! The JSON settings writers ([`super::ToolsSettings::save_to`] and friends)
//! read the file into a `serde_json::Value`, replace one key, and re-render.
//! That preserves every OTHER key byte-for-byte at the value level, which is
//! all JSON has to preserve — a settings editor that rewrote the whole file
//! would silently drop `providers`, `hooks`, and every forward-compat key it
//! has never heard of.
//!
//! TOML has more to lose. Parsing with `toml` and re-rendering discards every
//! comment, all key ordering, and all whitespace — so the first `/theme` save
//! would delete the user's entire annotated config and hand back a machine
//! dump. That would remove the single biggest reason to move to TOML in the
//! first place, and it would do it silently, on a write the user did not think
//! was destructive.
//!
//! So every write goes through `toml_edit`, which models the document as a
//! syntax tree rather than a value tree: replacing one key leaves every byte
//! that is not that key exactly where it was. The tests below pin this — the
//! round-trip test is the whole contract, and it is why this module was built
//! before any key was ported.

use std::path::Path;

use serde::Serialize;
use toml_edit::{DocumentMut, Item};

use super::private;

/// Read `path` as an editable document, or start an empty one when the file
/// does not exist yet.
///
/// A malformed file is a named error, never a silent reset: an editor that
/// quietly discarded a config it could not parse would be worse than one that
/// refuses to save.
fn load_document(path: &Path) -> Result<DocumentMut, String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => contents
            .parse::<DocumentMut>()
            .map_err(|e| format!("invalid config file {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// Serialize `value` into the `Item` a top-level section takes.
///
/// Goes through `toml_edit`'s own serializer rather than `toml::to_string` +
/// re-parse so nested tables keep their structure instead of being flattened
/// into inline tables.
fn section_item<T: Serialize>(value: &T, key: &str) -> Result<Item, String> {
    let rendered =
        toml_edit::ser::to_document(value).map_err(|e| format!("cannot serialize {key}: {e}"))?;
    let mut table = rendered.as_table().clone();
    // A freshly serialized document's root table is "implicit" (it has no
    // header of its own). Assigning it as a named section needs it explicit, or
    // it renders as a headerless table and its keys leak to the document root —
    // exactly the bare-root-key hazard `[run]` exists to avoid.
    table.set_implicit(false);
    let mut item = Item::Table(table);
    expand_inline_tables(&mut item);
    narrow_widened_floats(&mut item);
    hide_empty_parents(&mut item);
    Ok(item)
}

/// Suppress the header of a table that holds nothing but other tables.
///
/// Without this the migration emits a bare `[providers]` immediately followed
/// by `[providers.anthropic]`, and a bare `[context]` above five sub-tables.
/// They are harmless to the parser and pure noise to the reader — and they
/// invite exactly the mistake `[run]` exists to prevent, because the next
/// person to add a key naturally puts it under the empty header where it will
/// silently belong to the wrong table.
///
/// A table with any direct key-value stays explicit, since its keys need a
/// header to belong to.
fn hide_empty_parents(item: &mut Item) {
    match item {
        Item::Table(table) => {
            let has_direct_values = table.iter().any(|(_, child)| child.is_value());
            table.set_implicit(!has_direct_values);
            for (_, child) in table.iter_mut() {
                hide_empty_parents(child);
            }
        }
        Item::ArrayOfTables(aot) => {
            for table in aot.iter_mut() {
                // An array-of-tables entry always renders its own `[[...]]`
                // header, so it is never implicit — only its children are.
                for (_, child) in table.iter_mut() {
                    hide_empty_parents(child);
                }
            }
        }
        Item::None | Item::Value(_) => {}
    }
}

/// Rewrite serde's inline tables (`verifier = { effort = "high" }`) as real
/// headered tables (`[agents.verifier]`).
///
/// Both are valid TOML and both round-trip, so this is purely about the file a
/// person opens. serde's serializer emits every nested struct inline, which
/// collapses the whole config onto a handful of very long lines — the exact
/// unreadability the move away from JSON was meant to fix. A generated file
/// that looks nothing like the documented reference also teaches the wrong
/// shape to whoever edits it next.
fn expand_inline_tables(item: &mut Item) {
    match item {
        Item::Table(table) => {
            for (_, child) in table.iter_mut() {
                expand_inline_tables(child);
            }
        }
        Item::Value(toml_edit::Value::InlineTable(inline)) => {
            let mut table = std::mem::replace(inline, toml_edit::InlineTable::new()).into_table();
            table.set_implicit(false);
            let mut promoted = Item::Table(table);
            expand_inline_tables(&mut promoted);
            *item = promoted;
        }
        // An array whose every element is a table becomes an array-of-tables
        // (`[[hooks.PreToolUse]]`). A mixed or scalar array is left alone.
        Item::Value(toml_edit::Value::Array(array)) => {
            if array.is_empty()
                || !array
                    .iter()
                    .all(|v| matches!(v, toml_edit::Value::InlineTable(_)))
            {
                return;
            }
            let mut aot = toml_edit::ArrayOfTables::new();
            for value in array.iter() {
                let toml_edit::Value::InlineTable(inline) = value else {
                    unreachable!("checked above");
                };
                let mut child = Item::Value(toml_edit::Value::InlineTable(inline.clone()));
                expand_inline_tables(&mut child);
                if let Item::Table(table) = child {
                    aot.push(table);
                }
            }
            *item = Item::ArrayOfTables(aot);
        }
        Item::ArrayOfTables(aot) => {
            for table in aot.iter_mut() {
                for (_, child) in table.iter_mut() {
                    expand_inline_tables(child);
                }
            }
        }
        Item::None | Item::Value(_) => {}
    }
}

/// Re-render a float that is really a widened `f32`.
///
/// `temperature: Option<f32>` reaches the TOML serializer as `f64`, so `0.2`
/// is written `0.20000000298023224`. That is the same number, and it round-trips
/// — but it appears in a file a person reads and edits, where it looks like
/// corruption and invites someone to "fix" it by hand.
///
/// A value is treated as a widened `f32` only when narrowing and re-widening is
/// EXACTLY lossless, so a genuine `f64` knob is never silently rounded. The
/// trailing `.0` is mandatory: without it `60.0` renders as `60`, which TOML
/// reads back as an integer and serde then rejects as the wrong type.
fn narrow_widened_floats(item: &mut Item) {
    match item {
        Item::Table(table) => {
            for (_, child) in table.iter_mut() {
                narrow_widened_floats(child);
            }
        }
        Item::ArrayOfTables(aot) => {
            for table in aot.iter_mut() {
                for (_, child) in table.iter_mut() {
                    narrow_widened_floats(child);
                }
            }
        }
        Item::Value(toml_edit::Value::Float(f)) => {
            let wide = *f.value();
            let narrow = wide as f32;
            if f64::from(narrow) != wide {
                return;
            }
            // Round-trip through the f32's own shortest representation. The
            // resulting f64 differs from `wide` in the last bits, which is
            // exactly the point: the destination field is an f32, so reading it
            // back yields the identical f32 the user wrote.
            let Ok(parsed) = narrow.to_string().parse::<f64>() else {
                return;
            };
            let mut reformatted = toml_edit::Formatted::new(parsed);
            reformatted.fmt();
            *item = Item::Value(toml_edit::Value::Float(reformatted));
        }
        Item::Value(toml_edit::Value::Array(array)) => {
            for value in array.iter_mut() {
                let mut child = Item::Value(value.clone());
                narrow_widened_floats(&mut child);
                if let Item::Value(v) = child {
                    *value = v;
                }
            }
        }
        Item::None | Item::Value(_) => {}
    }
}

/// Write `value` as the top-level `key` of the TOML config at `path`,
/// preserving every comment, key order, and whitespace elsewhere in the file.
///
/// `None` REMOVES the key rather than writing an empty table: "no switches" and
/// "no theme" are the shipped postures, and the file should read as though the
/// editor had never been opened.
///
/// `user_private` reserves owner-only atomic writes for the user scope, which
/// is the tier that can hold credentials — the same split the JSON writers make.
pub fn save_section<T: Serialize>(
    path: &Path,
    key: &str,
    value: Option<&T>,
    user_private: bool,
) -> Result<(), String> {
    private::reject_symlink(path)?;
    let mut doc = load_document(path)?;
    match value {
        Some(value) => doc[key] = section_item(value, key)?,
        None => {
            doc.remove(key);
        }
    }
    private::write_settings(path, doc.to_string().as_bytes(), user_private)
}

/// Write several sections in ONE read-modify-write.
///
/// Needed because a single JSON key can lower to more than one TOML section:
/// `agent_engine_config` splits into `[agents]` and `[models].allowed`. Saving
/// them with two `save_section` calls would parse, render, and rewrite the file
/// twice — doubling the window in which a crash leaves a half-written config,
/// and doubling the chance a bug in one write clobbers the other.
pub fn save_sections(
    path: &Path,
    sections: &[(&str, Option<Item>)],
    user_private: bool,
) -> Result<(), String> {
    private::reject_symlink(path)?;
    let mut doc = load_document(path)?;
    for (key, item) in sections {
        match item {
            Some(item) => doc[*key] = item.clone(),
            None => {
                doc.remove(key);
            }
        }
    }
    private::write_settings(path, doc.to_string().as_bytes(), user_private)
}

/// Build the `Item` for one section without writing it — the input to
/// [`save_sections`].
pub fn item_for<T: Serialize>(value: &T, key: &str) -> Result<Item, String> {
    section_item(value, key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::UiSettings;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// THE contract of this module. If this test ever fails, the migration has
    /// lost its justification: a config whose comments do not survive a write
    /// is a JSON file with a different syntax.
    #[test]
    fn a_write_preserves_every_comment_and_the_order_of_untouched_keys() {
        let dir = tmp();
        let path = dir.path().join("stella.toml");
        let original = r#"# Top-of-file banner explaining the whole config.
[meta]
schema_version = 1

# Why this provider is pinned to an internal endpoint.
[providers.anthropic]
api_key_env = "ANTHROPIC_API_KEY"  # trailing comment
default_model = "claude-fable-5"

[ui]
theme = "stella-dark"

# A section AFTER the one being edited, to prove ordering survives.
[tools]
bash = "off"
"#;
        std::fs::write(&path, original).unwrap();

        let ui = UiSettings {
            theme: Some("stella-light".to_string()),
        };
        save_section(&path, "ui", Some(&ui), false).unwrap();

        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            after.contains("# Top-of-file banner explaining the whole config."),
            "banner comment survives:\n{after}"
        );
        assert!(
            after.contains("# Why this provider is pinned to an internal endpoint."),
            "section comment survives:\n{after}"
        );
        assert!(
            after.contains("# trailing comment"),
            "trailing comment survives:\n{after}"
        );
        assert!(
            after.contains("# A section AFTER the one being edited"),
            "comment after the edit survives:\n{after}"
        );
        assert!(
            after.contains(r#"theme = "stella-light""#),
            "the edit landed:\n{after}"
        );
        assert!(
            !after.contains("stella-dark"),
            "the old value is gone:\n{after}"
        );
        assert!(
            after.find("[providers.anthropic]").unwrap() < after.find("[tools]").unwrap(),
            "key order survives:\n{after}"
        );
    }

    #[test]
    fn writing_a_section_into_a_file_that_does_not_exist_yet_creates_it() {
        let dir = tmp();
        let path = dir.path().join("nested").join("stella.toml");
        let ui = UiSettings {
            theme: Some("stella-light".to_string()),
        };
        save_section(&path, "ui", Some(&ui), false).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains(r#"theme = "stella-light""#), "{after}");
    }

    #[test]
    fn a_none_value_removes_the_section_rather_than_writing_an_empty_table() {
        let dir = tmp();
        let path = dir.path().join("stella.toml");
        std::fs::write(
            &path,
            "[ui]\ntheme = \"stella-dark\"\n\n[tools]\nbash = \"off\"\n",
        )
        .unwrap();

        save_section::<UiSettings>(&path, "ui", None, false).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(!after.contains("[ui]"), "section removed:\n{after}");
        assert!(!after.contains("theme"), "its keys went with it:\n{after}");
        assert!(after.contains("[tools]"), "siblings untouched:\n{after}");
    }

    #[test]
    fn a_malformed_file_is_a_named_error_not_a_silent_reset() {
        let dir = tmp();
        let path = dir.path().join("stella.toml");
        std::fs::write(&path, "[ui\ntheme = broken").unwrap();
        let ui = UiSettings {
            theme: Some("stella-light".to_string()),
        };
        let err = save_section(&path, "ui", Some(&ui), false).unwrap_err();
        assert!(err.contains("invalid config file"), "{err}");
        assert!(err.contains("stella.toml"), "names the path: {err}");
        // The original bytes are still there — nothing was overwritten.
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, "[ui\ntheme = broken");
    }

    #[test]
    fn a_nested_section_renders_as_a_table_not_an_inline_dump() {
        let dir = tmp();
        let path = dir.path().join("stella.toml");
        let cfg = crate::settings::AgentEngineConfig {
            default_model: Some("anthropic/claude-fable-5".to_string()),
            ..Default::default()
        };
        save_section(&path, "agents", Some(&cfg), false).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("[agents]"), "has a real header:\n{after}");
        assert!(
            after.contains(r#"default_model = "anthropic/claude-fable-5""#),
            "{after}"
        );
    }
}
