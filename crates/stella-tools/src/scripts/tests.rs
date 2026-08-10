use super::*;

fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

#[test]
fn node_scripts_and_lockfile_pm_bind_verbs() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "package.json",
        r#"{"scripts": {"build": "next build", "dev": "next dev", "test": "vitest",
            "test:watch": "vitest --watch", "deploy": "vercel deploy"}}"#,
    );
    write(dir.path(), "pnpm-lock.yaml", "");
    let index = ScriptIndex::detect_blocking(dir.path());

    assert_eq!(index.verbs.get("build").unwrap(), "pnpm:build");
    assert_eq!(index.verbs.get("test").unwrap(), "pnpm:test");
    assert_eq!(index.verbs.get("start").unwrap(), "pnpm:dev");
    assert_eq!(index.verbs.get("install").unwrap(), "pnpm:install");
    let build = index.verb_entry("build").unwrap();
    assert_eq!(build.command, "pnpm run build");
    assert_eq!(build.raw.as_deref(), Some("next build"));
    assert_eq!(index.verb_entry("install").unwrap().command, "pnpm install",);
    // watch/deploy names are listed but never verb-bound.
    for entry in &index.scripts {
        if entry.name == "test:watch" || entry.name == "deploy" {
            assert!(entry.verb.is_none(), "{} must not bind a verb", entry.id);
        }
    }
}

#[test]
fn cargo_root_synthesizes_verbs_and_reads_aliases() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "Cargo.toml",
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
    );
    write(
        dir.path(),
        ".cargo/config.toml",
        "[alias]\nxt = \"test --workspace\"\n",
    );
    let index = ScriptIndex::detect_blocking(dir.path());

    assert_eq!(index.verbs.get("install").unwrap(), "cargo:install");
    assert_eq!(index.verb_entry("install").unwrap().command, "cargo fetch");
    assert_eq!(
        index.verb_entry("build").unwrap().command,
        "cargo build --workspace"
    );
    // No default-run bin and no src/main.rs → no start verb.
    assert!(!index.verbs.contains_key("start"));
    let alias = index.scripts.iter().find(|e| e.id == "cargo:xt").unwrap();
    assert_eq!(alias.command, "cargo xt");
    assert_eq!(alias.raw.as_deref(), Some("test --workspace"));
    assert_eq!(alias.source, ".cargo/config.toml");
}

#[test]
fn check_verb_synthesizes_for_cargo_and_binds_explicit_typecheck() {
    // Cargo: the synthesized fast typecheck binds `check`.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "Cargo.toml",
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
    );
    let index = ScriptIndex::detect_blocking(dir.path());
    assert_eq!(index.verbs.get("check").unwrap(), "cargo:check");
    assert_eq!(
        index.verb_entry("check").unwrap().command,
        "cargo check --workspace"
    );

    // Node: an explicit `typecheck` script binds the verb via its alias,
    // and `run_script {"script": "check"}` resolves to it.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "package.json",
        r#"{"scripts": {"typecheck": "tsc --noEmit"}}"#,
    );
    let index = ScriptIndex::detect_blocking(dir.path());
    assert_eq!(index.verbs.get("check").unwrap(), "npm:typecheck");
    assert_eq!(index.resolve("check", None).unwrap().id, "npm:typecheck");
}

#[test]
fn make_targets_parse_and_skip_variables_and_patterns() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "Makefile",
        ".PHONY: build test\nVAR := x\nOTHER = y\n\nbuild:\n\tcargo build\n\
         test: build\n\techo test\n%.o: %.c\n\tcc\n.hidden:\n\ttrue\n",
    );
    let index = ScriptIndex::detect_blocking(dir.path());
    let ids: Vec<&str> = index.scripts.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, vec!["make:build", "make:test"]);
    // make is the only ecosystem → its explicit targets bind the verbs.
    assert_eq!(index.verbs.get("build").unwrap(), "make:build");
    assert_eq!(index.verbs.get("test").unwrap(), "make:test");
    assert!(!index.verbs.contains_key("install"));
}

#[test]
fn justfile_recipes_skip_hidden_and_assignments() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "justfile",
        "version := \"1\"\n\n# comment\nbuild target=\"debug\":\n    cargo build\n\
         _helper:\n    true\nfmt:\n    cargo fmt\n",
    );
    let index = ScriptIndex::detect_blocking(dir.path());
    let ids: Vec<&str> = index.scripts.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, vec!["just:build", "just:fmt"]);
    assert_eq!(index.verbs.get("format").unwrap(), "just:fmt");
}

#[test]
fn pyproject_uv_synthesizes_only_declared_tools() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "pyproject.toml",
        "[project]\nname = \"x\"\ndependencies = [\"requests>=2\"]\n\n\
         [dependency-groups]\ndev = [\"pytest>=8\", \"ruff\"]\n\n\
         [project.scripts]\nserve = \"x.app:main\"\n",
    );
    write(dir.path(), "uv.lock", "");
    let index = ScriptIndex::detect_blocking(dir.path());

    assert_eq!(index.verb_entry("install").unwrap().command, "uv sync");
    assert_eq!(index.verb_entry("test").unwrap().command, "uv run pytest");
    assert_eq!(
        index.verb_entry("lint").unwrap().command,
        "uv run ruff check"
    );
    assert_eq!(
        index.verb_entry("format").unwrap().command,
        "uv run ruff format"
    );
    // The [project.scripts] entry point is explicit and binds `start`
    // via its `serve` alias.
    assert_eq!(index.verbs.get("start").unwrap(), "uv:serve");
    assert_eq!(index.verb_entry("start").unwrap().command, "uv run serve");

    // Without pytest/ruff declared, none of test/lint/format synthesize.
    let bare = tempfile::tempdir().unwrap();
    write(bare.path(), "pyproject.toml", "[project]\nname = \"y\"\n");
    let bare_index = ScriptIndex::detect_blocking(bare.path());
    assert!(!bare_index.verbs.contains_key("test"));
    assert!(!bare_index.verbs.contains_key("lint"));
}

#[test]
fn poetry_marker_selects_poetry_runner() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "pyproject.toml",
        "[tool.poetry]\nname = \"x\"\n\n[tool.poetry.dependencies]\npytest = \"^8\"\n",
    );
    let index = ScriptIndex::detect_blocking(dir.path());
    assert_eq!(
        index.verb_entry("install").unwrap().command,
        "poetry install"
    );
    assert_eq!(
        index.verb_entry("test").unwrap().command,
        "poetry run pytest"
    );
}

#[test]
fn multi_ecosystem_rank_one_wins_then_later_explicit_fills_gaps() {
    // Node (rank 2) is first: its scripts win; the Makefile (rank 6)
    // fills verbs node doesn't define.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "package.json",
        r#"{"scripts": {"test": "vitest"}}"#,
    );
    write(dir.path(), "Makefile", "build:\n\ttrue\ntest:\n\ttrue\n");
    let index = ScriptIndex::detect_blocking(dir.path());
    assert_eq!(index.verbs.get("test").unwrap(), "npm:test");
    assert_eq!(index.verbs.get("build").unwrap(), "make:build");
    // Synthesized install of the first ecosystem still binds.
    assert_eq!(index.verbs.get("install").unwrap(), "npm:install");
}

#[test]
fn workspace_members_are_indexed_with_their_dir() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "package.json",
        r#"{"workspaces": ["packages/*"], "scripts": {"build": "true"}}"#,
    );
    write(dir.path(), "pnpm-lock.yaml", "");
    write(
        dir.path(),
        "packages/app/package.json",
        r#"{"scripts": {"dev": "vite"}}"#,
    );
    let index = ScriptIndex::detect_blocking(dir.path());
    let member = index
        .scripts
        .iter()
        .find(|e| e.dir == "packages/app" && e.name == "dev")
        .expect("member script indexed");
    assert_eq!(member.id, "pnpm:dev");
    assert_eq!(member.source, "packages/app/package.json");
    // Verbs bind at the root only.
    assert_eq!(index.verbs.get("start"), None);
    assert_eq!(index.verbs.get("build").unwrap(), "pnpm:build");
}

#[test]
fn a_dotdot_member_pattern_is_not_walked_and_the_skip_is_reported() {
    // The manifests are repository-controlled input: a `../` member would
    // otherwise index — and let `run_script` execute in — whatever the
    // checkout happens to sit next to.
    let outer = tempfile::tempdir().unwrap();
    write(
        outer.path(),
        "sibling/package.json",
        r#"{"scripts": {"exfil": "cat ~/.ssh/id_rsa"}}"#,
    );
    let root = outer.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    write(
        &root,
        "package.json",
        r#"{"workspaces": ["../sibling", "../*", "packages/*"], "scripts": {"build": "true"}}"#,
    );
    write(&root, "pnpm-lock.yaml", "");
    write(
        &root,
        "packages/app/package.json",
        r#"{"scripts": {"dev": "vite"}}"#,
    );

    let index = ScriptIndex::detect_blocking(&root);
    assert!(
        index.scripts.iter().all(|e| e.name != "exfil"),
        "an out-of-root member was walked: {:?}",
        index.scripts.iter().map(|e| &e.id).collect::<Vec<_>>()
    );
    assert!(
        index.scripts.iter().all(|e| !e.dir.contains("..")),
        "a member dir escaped the root: {:?}",
        index.scripts.iter().map(|e| &e.dir).collect::<Vec<_>>()
    );
    // In-root members still index normally.
    assert!(
        index
            .scripts
            .iter()
            .any(|e| e.dir == "packages/app" && e.name == "dev"),
        "confinement dropped a legitimate member"
    );
    // The skip is observable, not silent: `../sibling` and the `../*`
    // base both resolve outside the root.
    assert_eq!(index.out_of_root_members, 2);
    assert!(
        index
            .render_list(None)
            .contains("resolve outside the workspace root"),
        "{}",
        index.render_list(None)
    );
}

#[test]
fn pnpm_workspace_yaml_and_taskfile_and_composer_and_deno_parse() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "pnpm-workspace.yaml",
        "# comment\npackages:\n  - docs\n  - '!excluded'\nother:\n  - not-a-package\n",
    );
    write(dir.path(), "package.json", r#"{"scripts": {}}"#);
    write(dir.path(), "pnpm-lock.yaml", "");
    write(
        dir.path(),
        "docs/package.json",
        r#"{"scripts": {"dev": "next dev"}}"#,
    );
    write(
        dir.path(),
        "Taskfile.yml",
        "version: '3'\ntasks:\n  greet:\n    cmds:\n      - echo hi\n  lint:\n    cmds:\n      - true\n",
    );
    write(
        dir.path(),
        "composer.json",
        r#"{"scripts": {"post-install-cmd": ["A\\B::hook"], "check": "phpstan"}}"#,
    );
    write(
        dir.path(),
        "deno.jsonc",
        "{\n  // a comment\n  \"tasks\": { \"bench\": \"deno bench\" }\n}\n",
    );
    let index = ScriptIndex::detect_blocking(dir.path());
    let has = |id: &str| index.scripts.iter().any(|e| e.id == id);
    assert!(has("task:greet"), "{:?}", index.scripts);
    assert!(has("task:lint"));
    assert!(has("composer:check"));
    assert!(has("deno:bench"));
    assert!(
        index
            .scripts
            .iter()
            .any(|e| e.dir == "docs" && e.id == "pnpm:dev"),
        "pnpm-workspace member indexed"
    );
    assert!(
        !index.scripts.iter().any(|e| e.dir == "not-a-package"),
        "keys outside packages: must not enumerate"
    );
}

#[test]
fn taskfile_namespaced_task_names_survive_intact() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "Taskfile.yml",
        "version: '3'\ntasks:\n  build:\n    cmds:\n      - go build\n  ns:build:\n    cmds:\n      - go build ./ns\n",
    );
    let index = ScriptIndex::detect_blocking(dir.path());
    let ns = index
        .scripts
        .iter()
        .find(|e| e.id == "task:ns:build")
        .unwrap_or_else(|| panic!("namespaced task indexed: {:?}", index.scripts));
    assert_eq!(ns.name, "ns:build");
    assert_eq!(ns.command, "task ns:build");
    // The first-colon truncation produced a phantom `ns` task.
    assert!(
        !index.scripts.iter().any(|e| e.id == "task:ns"),
        "{:?}",
        index.scripts
    );
    assert!(index.scripts.iter().any(|e| e.id == "task:build"));
}

#[test]
fn go_synthesizes_the_full_verb_set() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "go.mod", "module example.com/x\n");
    let index = ScriptIndex::detect_blocking(dir.path());
    assert_eq!(
        index.verb_entry("install").unwrap().command,
        "go mod download"
    );
    assert_eq!(index.verb_entry("test").unwrap().command, "go test ./...");
    assert_eq!(index.verb_entry("lint").unwrap().command, "go vet ./...");
}

#[test]
fn detection_is_byte_stable() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "package.json",
        r#"{"scripts": {"b": "1", "a": "2", "c": "3"}}"#,
    );
    write(dir.path(), "Makefile", "x:\n\ttrue\n");
    let a = ScriptIndex::detect_blocking(dir.path());
    let b = ScriptIndex::detect_blocking(dir.path());
    assert_eq!(a.render_prompt_section(), b.render_prompt_section());
    assert_eq!(a.render_list(None), b.render_list(None));
    assert_eq!(a.to_json().to_string(), b.to_json().to_string());
}

#[test]
fn prompt_section_lists_verbs_and_counts_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "package.json",
        r#"{"scripts": {"build": "next build", "docs:gen": "typedoc"}}"#,
    );
    let index = ScriptIndex::detect_blocking(dir.path());
    let section = index.render_prompt_section().unwrap();
    assert!(section.starts_with("## Project scripts"));
    assert!(section.contains("build → npm run build"), "{section}");
    assert!(section.contains("install → npm install"));
    assert!(section.contains("more scripts"), "{section}");
    assert!(section.contains("npm:docs:gen"), "{section}");
    assert!(section.chars().count() <= PROMPT_SECTION_CHAR_CAP);

    let empty = tempfile::tempdir().unwrap();
    assert!(
        ScriptIndex::detect_blocking(empty.path())
            .render_prompt_section()
            .is_none()
    );
}

#[test]
fn resolve_accepts_verb_id_and_unique_name_and_names_near_misses() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "package.json",
        r#"{"scripts": {"build": "true", "typecheck": "tsc"}}"#,
    );
    let index = ScriptIndex::detect_blocking(dir.path());
    assert_eq!(index.resolve("build", None).unwrap().id, "npm:build");
    assert_eq!(index.resolve("npm:build", None).unwrap().id, "npm:build");
    assert_eq!(
        index.resolve("typecheck", None).unwrap().id,
        "npm:typecheck"
    );
    let err = index.resolve("typechek", None).unwrap_err();
    assert!(err.contains("unknown script"), "{err}");
    let err = index.resolve("lint", None).unwrap_err();
    assert!(err.contains("no `lint` script detected"), "{err}");
}

#[test]
fn compose_command_quotes_args_and_uses_npm_family_separator() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "package.json",
        r#"{"scripts": {"test": "vitest"}}"#,
    );
    write(dir.path(), "Makefile", "lint:\n\ttrue\n");
    let index = ScriptIndex::detect_blocking(dir.path());

    let test = index.resolve("test", None).unwrap();
    assert_eq!(
        compose_command(test, &["--run".into(), "my file".into()]),
        "npm run test -- --run 'my file'"
    );
    let lint = index.resolve("make:lint", None).unwrap();
    assert_eq!(compose_command(lint, &["V=1".into()]), "make lint V=1");
    // Synthesized npm install takes plain args (no `--`).
    let install = index.resolve("install", None).unwrap();
    assert_eq!(
        compose_command(install, &["--frozen-lockfile".into()]),
        "npm install --frozen-lockfile"
    );
}

#[test]
fn strip_jsonc_preserves_strings_and_removes_comments() {
    let src = "{ // c\n \"a\": \"http://x/*y*/\", /* b\n */ \"t\": 1 }";
    let stripped = strip_jsonc_comments(src);
    let doc: Value = serde_json::from_str(&stripped).unwrap();
    assert_eq!(doc.get("a").unwrap(), "http://x/*y*/");
    assert_eq!(doc.get("t").unwrap(), 1);
}

#[tokio::test]
async fn run_script_tool_executes_indexed_entries_only() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "Makefile", "greet:\n\t@echo hello-from-make\n");

    let out = RunScript
        .execute(&serde_json::json!({"script": "make:greet"}), dir.path())
        .await;
    match &out {
        ToolOutput::Ok { content } => {
            assert!(content.contains("PASSED"), "{content}");
            assert!(content.contains("hello-from-make"), "{content}");
        }
        other => panic!("{other:?}"),
    }

    let out = RunScript
        .execute(&serde_json::json!({"script": "rm -rf /"}), dir.path())
        .await;
    assert!(out.is_error(), "non-indexed input must be refused: {out:?}");

    let out = ListScripts
        .execute(&serde_json::json!({}), dir.path())
        .await;
    match &out {
        ToolOutput::Ok { content } => assert!(content.contains("make:greet"), "{content}"),
        other => panic!("{other:?}"),
    }
}
