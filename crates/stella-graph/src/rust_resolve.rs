//! Pure-Rust `use`→file resolution over the module tree (#443, option B2 of
//! `docs/design/semantic-resolution-bridge.md`).
//!
//! File-level edges only, by design: a `use crate::a::b::Item` resolves to
//! the **file defining the deepest path segment that names a module file**
//! (`b.rs`, or `a.rs` when `b` is an item or inline module inside it). Item
//! identity, trait dispatch, and `pub use` re-export chasing are explicit
//! non-goals — an edge to the re-exporting file is still a true dependency
//! edge. External crates (`std`, `serde`, anything not in the workspace
//! layout) stay unresolved, which is the pre-#443 behavior for every Rust
//! edge and remains visible in the stored `kind`/NULL `to_path` shape.
//!
//! Documented gaps, resolved to whichever file exists or left unresolved:
//! `#[path]` overrides, macro-generated modules, and `cfg`-divergent module
//! sets.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::import::rel_to_slash;

/// How deep below the workspace root the manifest scan descends. Cargo
/// workspaces nest members a level or two down (`bench/loop-bench`); five
/// covers real layouts without walking a monorepo's unrelated depths.
const MANIFEST_SCAN_DEPTH: usize = 5;

/// The workspace's Rust crate layout: package name (underscore-normalized,
/// as it appears in `use` paths) → the crate root source file. Loaded once
/// per index pass, like the storage manifest.
pub(crate) struct RustLayout {
    roots: HashMap<String, PathBuf>,
}

impl RustLayout {
    /// Scan for `Cargo.toml` manifests under `root` (bounded depth, heavy
    /// build/VCS directories skipped) and record each `[package]`'s crate
    /// root. `src/lib.rs` wins over `src/main.rs` for the name mapping —
    /// a `use <crate>::…` path always refers to the library target.
    pub(crate) fn load(root: &Path) -> Self {
        let mut roots = HashMap::new();
        let mut stack = vec![(root.to_path_buf(), 0usize)];
        while let Some((dir, depth)) = stack.pop() {
            let manifest = dir.join("Cargo.toml");
            if manifest.is_file()
                && let Some(name) = package_name(&manifest)
                && let Some(crate_root) = crate_root_file(&dir)
            {
                roots.insert(name.replace('-', "_"), crate_root);
            }
            if depth >= MANIFEST_SCAN_DEPTH {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let skip = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_none_or(|n| n.starts_with('.') || n == "target" || n == "node_modules");
                if !skip {
                    stack.push((path, depth + 1));
                }
            }
        }
        RustLayout { roots }
    }

    fn crate_root(&self, name: &str) -> Option<&PathBuf> {
        self.roots.get(&name.replace('-', "_"))
    }
}

/// The `name = "…"` under `[package]`, read without a full toml parse — the
/// only two facts needed are "is there a `[package]` table" and its name,
/// and a manifest broken enough to defeat this scan should simply not
/// contribute a crate root (never abort the index pass).
fn package_name(manifest: &Path) -> Option<String> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let mut in_package = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(section) = line.strip_prefix('[') {
            in_package = section.trim_end_matches(']').trim() == "package";
            continue;
        }
        if in_package
            && let Some(rest) = line.strip_prefix("name")
            && let Some(value) = rest.trim_start().strip_prefix('=')
        {
            return Some(value.trim().trim_matches('"').to_string());
        }
    }
    None
}

/// A crate directory's root source file: `src/lib.rs`, else `src/main.rs`.
fn crate_root_file(crate_dir: &Path) -> Option<PathBuf> {
    let lib = crate_dir.join("src/lib.rs");
    if lib.is_file() {
        return Some(lib);
    }
    let main = crate_dir.join("src/main.rs");
    main.is_file().then_some(main)
}

/// Expand one raw `use` argument into simple `::`-separated paths: brace
/// groups (nested), `as` renames dropped, a trailing `::*` wildcard reduced
/// to its module path, and a group's `self` leaf reduced to the group prefix
/// itself (`a::{self, b}` → `a`, `a::b`).
pub(crate) fn expand_use_groups(spec: &str) -> Vec<String> {
    let mut out = Vec::new();
    expand(spec, "", &mut out);
    out
}

fn expand(spec: &str, prefix: &str, out: &mut Vec<String>) {
    let spec = spec.trim();
    if let Some(open) = spec.find('{') {
        let Some(inner) = spec[open..].strip_prefix('{').and_then(strip_matched) else {
            return; // unbalanced braces: not a shape tree-sitter emits
        };
        let head = spec[..open].trim().trim_end_matches("::");
        let prefix = join_path(prefix, head);
        for part in split_top_level(inner) {
            expand(part, &prefix, out);
        }
        return;
    }
    // Leaf: drop an `as` rename, reduce a wildcard to its module path.
    let leaf = spec.split(" as ").next().unwrap_or(spec).trim();
    let leaf = leaf
        .strip_suffix("::*")
        .or(leaf.strip_suffix('*'))
        .unwrap_or(leaf);
    let leaf = leaf.trim_end_matches("::");
    let path = if leaf == "self" {
        prefix.to_string()
    } else {
        join_path(prefix, leaf)
    };
    if !path.is_empty() {
        out.push(path);
    }
}

/// The content inside an already-opened brace, requiring the close to be the
/// final character (tree-sitter's `use_list` shape).
fn strip_matched(after_open: &str) -> Option<&str> {
    let mut depth = 1usize;
    for (i, c) in after_open.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return after_open[i + 1..]
                        .trim()
                        .is_empty()
                        .then(|| &after_open[..i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Split a brace group's content on commas at depth zero.
fn split_top_level(inner: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (i, c) in inner.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&inner[start..]);
    parts
        .into_iter()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect()
}

fn join_path(prefix: &str, tail: &str) -> String {
    match (prefix.is_empty(), tail.is_empty()) {
        (true, _) => tail.to_string(),
        (_, true) => prefix.to_string(),
        _ => format!("{prefix}::{tail}"),
    }
}

/// Resolve one expanded `use` path to a workspace-relative file, or `None`
/// (external crate, unresolvable prefix, or a walk that never leaves the
/// importing file — a self-edge says nothing).
pub(crate) fn resolve_use(
    layout: &RustLayout,
    path: &str,
    root: &Path,
    file_abs: &Path,
) -> Option<String> {
    let mut segments = path.split("::").map(str::trim).peekable();
    let first = segments.next()?;
    let mut current: PathBuf = match first {
        "crate" => owning_crate_root(file_abs)?,
        "self" => file_abs.to_path_buf(),
        "super" => {
            let mut cur = parent_module_file(file_abs)?;
            while segments.peek() == Some(&"super") {
                segments.next();
                cur = parent_module_file(&cur)?;
            }
            cur
        }
        // `use ::x::…` — an explicit extern prefix; the crate name follows.
        "" => layout.crate_root(segments.next()?)?.clone(),
        name => layout.crate_root(name)?.clone(),
    };
    for segment in segments {
        if segment == "self" || segment.is_empty() {
            continue;
        }
        match child_module_file(&current, segment) {
            Some(child) => current = child,
            // An item, inline module, or `#[path]` child: the deepest module
            // FILE reached is the honest file-level edge.
            None => break,
        }
    }
    if current == file_abs {
        return None;
    }
    let canonical = current.canonicalize().ok()?;
    Some(rel_to_slash(canonical.strip_prefix(root).ok()?))
}

/// Resolve a `mod name;` declaration to the child module file it brings into
/// the tree — `name.rs` or `name/mod.rs` in the declaring file's child dir.
pub(crate) fn resolve_mod_decl(name: &str, root: &Path, file_abs: &Path) -> Option<String> {
    let child = child_module_file(file_abs, name)?;
    let canonical = child.canonicalize().ok()?;
    Some(rel_to_slash(canonical.strip_prefix(root).ok()?))
}

/// The directory a module file's children live in: the file's own directory
/// for the roots (`lib.rs` / `main.rs`) and `mod.rs`, else a directory named
/// after the file (`foo.rs` → `foo/`, the 2018-edition layout).
fn child_dir(file: &Path) -> Option<PathBuf> {
    let name = file.file_name()?.to_str()?;
    let dir = file.parent()?;
    if matches!(name, "lib.rs" | "main.rs" | "mod.rs") {
        return Some(dir.to_path_buf());
    }
    Some(dir.join(file.file_stem()?.to_str()?))
}

/// The child module `name`'s file under `parent`'s child dir, if it exists.
fn child_module_file(parent: &Path, name: &str) -> Option<PathBuf> {
    let dir = child_dir(parent)?;
    let flat = dir.join(format!("{name}.rs"));
    if flat.is_file() {
        return Some(flat);
    }
    let nested = dir.join(name).join("mod.rs");
    nested.is_file().then_some(nested)
}

/// The crate root file owning `file`: the nearest ancestor directory holding
/// a `Cargo.toml`, then its `src/lib.rs` / `src/main.rs`. A file under
/// `src/bin/` belongs to its own binary target, whose root is itself.
fn owning_crate_root(file: &Path) -> Option<PathBuf> {
    if file.parent().and_then(|d| d.file_name()) == Some(std::ffi::OsStr::new("bin")) {
        return Some(file.to_path_buf());
    }
    let mut dir = file.parent()?;
    loop {
        if dir.join("Cargo.toml").is_file() {
            return crate_root_file(dir);
        }
        dir = dir.parent()?;
    }
}

/// The file defining the module that DECLARES `file`'s module — `None` at a
/// crate root, which has no parent. `d/mod.rs` is the module named `d`,
/// declared by whatever owns `d`'s parent directory; any other `d/f.rs` is
/// declared by whatever owns `d` itself.
fn parent_module_file(file: &Path) -> Option<PathBuf> {
    let name = file.file_name()?.to_str()?;
    if matches!(name, "lib.rs" | "main.rs") {
        return None;
    }
    let dir = file.parent()?;
    if name == "mod.rs" {
        module_for_dir(dir.parent()?)
    } else {
        module_for_dir(dir)
    }
}

/// The module file whose children live in `dir`: a crate `src` root's own
/// root file, else the sibling `<dir>.rs`, else `dir/mod.rs`.
fn module_for_dir(dir: &Path) -> Option<PathBuf> {
    for root_name in ["lib.rs", "main.rs"] {
        let candidate = dir.join(root_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let name = dir.file_name()?.to_str()?;
    if let Some(parent) = dir.parent() {
        let flat = parent.join(format!("{name}.rs"));
        if flat.is_file() {
            return Some(flat);
        }
    }
    let nested = dir.join("mod.rs");
    nested.is_file().then_some(nested)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// A two-crate workspace exercising every layout rule: lib root, flat
    /// module, dir module (`mod.rs`), nested child, and a binary crate.
    fn fixture() -> tempfile::TempDir {
        let ws = tempfile::tempdir().unwrap();
        let r = ws.path();
        write(
            r,
            "Cargo.toml",
            "[workspace]\nmembers = [\"alpha\", \"beta-core\"]\n",
        );
        write(
            r,
            "alpha/Cargo.toml",
            "[package]\nname = \"alpha\"\nversion = \"0.0.0\"\n",
        );
        write(r, "alpha/src/lib.rs", "pub mod util;\npub mod deep;\n");
        write(r, "alpha/src/util.rs", "pub fn helper() {}\n");
        write(r, "alpha/src/deep/mod.rs", "pub mod inner;\n");
        write(r, "alpha/src/deep/inner.rs", "pub struct Inner;\n");
        write(
            r,
            "beta-core/Cargo.toml",
            "[package]\nname = \"beta-core\"\nversion = \"0.0.0\"\n",
        );
        write(r, "beta-core/src/main.rs", "mod local;\nfn main() {}\n");
        write(r, "beta-core/src/local.rs", "pub fn here() {}\n");
        ws
    }

    #[test]
    fn group_expansion_covers_braces_wildcards_renames_and_self() {
        assert_eq!(
            expand_use_groups("std::collections::HashMap"),
            vec!["std::collections::HashMap"]
        );
        assert_eq!(
            expand_use_groups("crate::a::{b, c::d}"),
            vec!["crate::a::b", "crate::a::c::d"]
        );
        assert_eq!(expand_use_groups("a::{self, b}"), vec!["a", "a::b"]);
        assert_eq!(expand_use_groups("super::util::*"), vec!["super::util"]);
        assert_eq!(expand_use_groups("crate::x as y"), vec!["crate::x"]);
        assert_eq!(
            expand_use_groups("a::{b::{c, d}, e}"),
            vec!["a::b::c", "a::b::d", "a::e"]
        );
    }

    #[test]
    fn use_paths_resolve_across_the_module_tree_and_workspace() {
        let ws = fixture();
        let root = ws.path().canonicalize().unwrap();
        let layout = RustLayout::load(&root);
        let lib = root.join("alpha/src/lib.rs");
        let inner = root.join("alpha/src/deep/inner.rs");
        let beta = root.join("beta-core/src/main.rs");

        // crate:: from the lib root, through both module layouts.
        assert_eq!(
            resolve_use(&layout, "crate::util::helper", &root, &inner).as_deref(),
            Some("alpha/src/util.rs")
        );
        assert_eq!(
            resolve_use(&layout, "crate::deep::inner::Inner", &root, &lib).as_deref(),
            Some("alpha/src/deep/inner.rs")
        );
        // The deepest MODULE file wins when the tail is an item.
        assert_eq!(
            resolve_use(&layout, "crate::util", &root, &lib).as_deref(),
            Some("alpha/src/util.rs")
        );
        // super:: climbs from a dir-module child to its mod.rs, then out.
        assert_eq!(
            resolve_use(&layout, "super::super::util::helper", &root, &inner).as_deref(),
            Some("alpha/src/util.rs")
        );
        // A bare workspace crate name resolves cross-crate, hyphen-normalized.
        assert_eq!(
            resolve_use(&layout, "alpha::util::helper", &root, &beta).as_deref(),
            Some("alpha/src/util.rs")
        );
        assert_eq!(
            resolve_use(&layout, "beta_core::local", &root, &lib).as_deref(),
            Some("beta-core/src/local.rs")
        );
        // External crates stay unresolved — the pre-#443 shape.
        assert_eq!(
            resolve_use(&layout, "std::collections::HashMap", &root, &lib),
            None
        );
        assert_eq!(resolve_use(&layout, "serde::Serialize", &root, &lib), None);
        // A walk that never leaves the importing file is no edge at all.
        assert_eq!(resolve_use(&layout, "crate::SomeItem", &root, &lib), None);
    }

    #[test]
    fn mod_declarations_resolve_to_both_child_layouts() {
        let ws = fixture();
        let root = ws.path().canonicalize().unwrap();
        let lib = root.join("alpha/src/lib.rs");
        let mod_rs = root.join("alpha/src/deep/mod.rs");
        assert_eq!(
            resolve_mod_decl("util", &root, &lib).as_deref(),
            Some("alpha/src/util.rs")
        );
        assert_eq!(
            resolve_mod_decl("deep", &root, &lib).as_deref(),
            Some("alpha/src/deep/mod.rs")
        );
        assert_eq!(
            resolve_mod_decl("inner", &root, &mod_rs).as_deref(),
            Some("alpha/src/deep/inner.rs")
        );
        assert_eq!(resolve_mod_decl("phantom", &root, &lib), None);
    }
}
