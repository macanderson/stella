//! Tests for [`crate::rules`].
//!
//! Extracted verbatim from the inline `mod tests` in `rules.rs` — a pure
//! move, no behavior change. `rules.rs` sat exactly at the 1500-line file-size
//! ceiling with no headroom, and Phase 3 (#714) adds to it; splitting the tests
//! to a sibling module is the sanctioned way to make room without raising a
//! baseline.

use super::*;

// ---- frontmatter parsing ----

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
    let fm =
        parse_frontmatter("---\ndescription: \"quoted value\"\nother: 'single quoted'\n---\nbody");
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

// ---- rule_from_file ----

#[test]
fn rule_from_file_uses_frontmatter_name_over_filename() {
    let r = rule_from_file(".stella/rules/style.md", "---\nname: custom-id\n---\nbody").unwrap();
    assert_eq!(r.id, "custom-id");
}

#[test]
fn rule_from_file_falls_back_to_filename_stem() {
    let r = rule_from_file(".stella/rules/no-force-push.md", "Never force-push.").unwrap();
    assert_eq!(r.id, "no-force-push");
}

#[test]
fn rule_from_file_returns_none_for_empty_body() {
    assert!(rule_from_file(".stella/rules/empty.md", "---\ndescription: d\n---\n").is_none());
}

#[test]
fn rule_from_file_parses_a_bash_command_guard() {
    let r = rule_from_file(
        ".stella/rules/no-force-push.md",
        "---\nguard-tool: Bash\nguard-deny-command: git push --force*\n---\nNever force-push.",
    )
    .unwrap();
    assert_eq!(
        r.guard,
        Some(RuleGuard {
            tool: Some("Bash".to_string()),
            deny_path_glob: None,
            deny_command_glob: Some("git push --force*".to_string()),
            allow_command_glob: None,
        })
    );
}

#[test]
fn rule_with_no_guard_frontmatter_is_prompt_only() {
    let r = rule_from_file(
        ".stella/rules/style.md",
        "---\ndescription: d\n---\nMatch the surrounding code style.",
    )
    .unwrap();
    assert!(r.guard.is_none());
    assert_eq!(r.tier(), RuleTier::Prompt);
}

#[test]
fn blank_guard_frontmatter_values_do_not_manufacture_a_guard() {
    // `guard-tool:` with nothing after it used to parse as `Some("")`: the
    // rule claimed Tier 2 and rendered as "[enforced]" while nothing could
    // ever match it, and the external gate got an empty deny entry.
    let r = rule_from_file(
        ".stella/rules/blank.md",
        "---\nguard-tool:\nguard-deny-path:\n---\nPrefer small diffs.",
    )
    .unwrap();
    assert!(r.guard.is_none());
    assert_eq!(r.tier(), RuleTier::Prompt);
    assert!(guards_to_deny(std::slice::from_ref(&r)).deny.is_empty());
}

#[test]
fn a_guarded_rule_is_tier_guarded() {
    let r = rule_from_file(
        ".stella/rules/x.md",
        "---\nguard-tool: Edit\n---\nNever edit generated files.",
    )
    .unwrap();
    assert_eq!(r.tier(), RuleTier::Guarded);
}

// ---- discovery + precedence merge, against a fake RuleSource ----

struct FakeRuleSource {
    by_dir: HashMap<String, Vec<RuleFile>>,
}

impl RuleSource for FakeRuleSource {
    fn read_rule_files(&self, dirs: &[String]) -> Vec<RuleFile> {
        let mut out = Vec::new();
        for dir in dirs {
            if let Some(files) = self.by_dir.get(dir) {
                out.extend(files.iter().cloned());
            }
        }
        out
    }
}

fn rule_file(dir: &str, name: &str, contents: &str) -> RuleFile {
    RuleFile {
        path: format!("{dir}/{name}"),
        contents: contents.to_string(),
        contributed_by: None,
    }
}

fn opts() -> LoadRulesOptions {
    LoadRulesOptions {
        cwd: "/proj".to_string(),
        user_rules_dir: "/home/u/.stella/rules".to_string(),
    }
}

#[test]
fn loads_a_rule_with_description_guard_and_body_end_to_end() {
    let o = opts();
    let dirs = rule_search_dirs(&o);
    let mut by_dir = HashMap::new();
    by_dir.insert(
        dirs[2].clone(),
        vec![rule_file(
            &dirs[2],
            "no-applied-migration.md",
            "---\ndescription: Never edit an applied migration\nguard-tool: Edit\nguard-deny-path: packages/database/migrations/*-applied/**\n---\nAdd a new forward migration instead of editing an applied one.",
        )],
    );
    let source = FakeRuleSource { by_dir };
    let rules = load_rules(&source, &o);
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].id, "no-applied-migration");
    assert_eq!(rules[0].description, "Never edit an applied migration");
    assert!(rules[0].text.contains("Add a new forward migration"));
}

#[test]
fn stella_overrides_claude_overrides_user_by_id() {
    let o = opts();
    let dirs = rule_search_dirs(&o);
    let mut by_dir = HashMap::new();
    by_dir.insert(
        dirs[0].clone(),
        vec![rule_file(&dirs[0], "r.md", "user text")],
    );
    by_dir.insert(
        dirs[1].clone(),
        vec![rule_file(&dirs[1], "r.md", "claude text")],
    );
    let source_no_stella = FakeRuleSource {
        by_dir: by_dir.clone(),
    };
    let rules = load_rules(&source_no_stella, &o);
    assert_eq!(
        rules.iter().find(|r| r.id == "r").unwrap().text,
        "claude text"
    );

    by_dir.insert(
        dirs[2].clone(),
        vec![rule_file(&dirs[2], "r.md", "stella text")],
    );
    let source_with_stella = FakeRuleSource { by_dir };
    let rules = load_rules(&source_with_stella, &o);
    assert_eq!(
        rules.iter().find(|r| r.id == "r").unwrap().text,
        "stella text"
    );
}

#[test]
fn ignores_files_with_an_empty_body() {
    let o = opts();
    let dirs = rule_search_dirs(&o);
    let mut by_dir = HashMap::new();
    by_dir.insert(
        dirs[2].clone(),
        vec![rule_file(
            &dirs[2],
            "empty.md",
            "---\ndescription: d\n---\n",
        )],
    );
    let source = FakeRuleSource { by_dir };
    assert!(load_rules(&source, &o).is_empty());
}

#[test]
fn returns_empty_list_when_no_rules_exist() {
    let o = opts();
    let source = FakeRuleSource {
        by_dir: HashMap::new(),
    };
    assert_eq!(load_rules(&source, &o), Vec::new());
}

// ---- enforce: Tier 1 rendering ----

fn rule(id: &str, text: &str, guard: Option<RuleGuard>) -> Rule {
    Rule {
        id: id.to_string(),
        description: String::new(),
        text: text.to_string(),
        guard,
        source: "test".to_string(),
    }
}

#[test]
fn render_rules_section_is_empty_with_no_rules() {
    assert_eq!(render_rules_section(&[]), "");
}

#[test]
fn render_rules_section_lists_rules_and_marks_enforced_ones() {
    let rules = vec![
        rule("a", "Always read before editing.", None),
        rule(
            "b",
            "Never edit applied migrations.",
            Some(RuleGuard {
                tool: Some("Edit".to_string()),
                deny_path_glob: Some("m/**".to_string()),
                deny_command_glob: None,
                allow_command_glob: None,
            }),
        ),
    ];
    let out = render_rules_section(&rules);
    assert!(out.contains("Workspace rules"));
    assert!(out.contains("Always read before editing."));
    assert!(out.contains("Never edit applied migrations.  [enforced]"));
}

// ---- enforce: guard_deny_entry / guards_to_deny ----

#[test]
fn guard_deny_entry_builds_tool_glob_for_a_path_guard() {
    let r = rule(
        "x",
        "t",
        Some(RuleGuard {
            tool: Some("Edit".to_string()),
            deny_path_glob: Some("m/**".to_string()),
            deny_command_glob: None,
            allow_command_glob: None,
        }),
    );
    assert_eq!(guard_deny_entry(&r).unwrap(), "Edit(m/**)");
}

#[test]
fn guard_deny_entry_builds_tool_glob_for_a_command_guard() {
    let r = rule(
        "x",
        "t",
        Some(RuleGuard {
            tool: Some("Bash".to_string()),
            deny_path_glob: None,
            deny_command_glob: Some("rm -rf*".to_string()),
            allow_command_glob: None,
        }),
    );
    assert_eq!(guard_deny_entry(&r).unwrap(), "Bash(rm -rf*)");
}

#[test]
fn guard_deny_entry_builds_a_bare_tool_when_no_pattern() {
    let r = rule(
        "x",
        "t",
        Some(RuleGuard {
            tool: Some("Bash".to_string()),
            deny_path_glob: None,
            deny_command_glob: None,
            allow_command_glob: None,
        }),
    );
    assert_eq!(guard_deny_entry(&r).unwrap(), "Bash");
}

#[test]
fn guard_deny_entry_is_none_without_a_guard() {
    assert!(guard_deny_entry(&rule("x", "t", None)).is_none());
}

#[test]
fn guards_to_deny_produces_entries_and_a_reason_map() {
    let rules = vec![
        rule(
            "no-mig",
            "Add a forward migration.",
            Some(RuleGuard {
                tool: Some("Edit".to_string()),
                deny_path_glob: Some("mig/*-applied/**".to_string()),
                deny_command_glob: None,
                allow_command_glob: None,
            }),
        ),
        rule("style", "prompt only", None),
    ];
    let denies = guards_to_deny(&rules);
    assert_eq!(denies.deny, vec!["Edit(mig/*-applied/**)".to_string()]);
    let reason = denies.reasons.get("Edit(mig/*-applied/**)").unwrap();
    assert!(reason.contains("rule \"no-mig\""));
    assert!(reason.contains("Add a forward migration."));
}

#[test]
fn guards_to_deny_emits_both_globs_when_a_guard_sets_both() {
    // A guard with BOTH a path and a command condition must surface BOTH to
    // the external gate — dropping either lets the gate disagree with
    // `evaluate_guards`.
    let rules = vec![rule(
        "locked",
        "do not touch",
        Some(RuleGuard {
            tool: Some("Bash".to_string()),
            deny_path_glob: Some("secrets/**".to_string()),
            deny_command_glob: Some("rm -rf*".to_string()),
            allow_command_glob: None,
        }),
    )];
    let denies = guards_to_deny(&rules);
    assert!(denies.deny.contains(&"Bash(secrets/**)".to_string()));
    assert!(denies.deny.contains(&"Bash(rm -rf*)".to_string()));
    assert_eq!(denies.deny.len(), 2);
}

#[test]
fn common_dir_prefix_stops_on_a_segment_boundary_not_mid_segment() {
    // `app/api2` is NOT under `app/api`; the common prefix must be `app`,
    // and the result must not depend on input order.
    let forward = common_dir_prefix(&["app/api/x.ts".into(), "app/api2/y.ts".into()]);
    let reverse = common_dir_prefix(&["app/api2/y.ts".into(), "app/api/x.ts".into()]);
    assert_eq!(forward.as_deref(), Some("app"));
    assert_eq!(reverse.as_deref(), Some("app"));
}

// ---- enforce: evaluate_guards (Tier 2, the actual block decision) ----

#[test]
fn evaluate_guards_blocks_a_matching_path() {
    let rules = vec![rule(
        "no-mig",
        "Add a forward migration.",
        Some(RuleGuard {
            tool: Some("Edit".to_string()),
            deny_path_glob: Some("packages/database/migrations/*-applied/**".to_string()),
            deny_command_glob: None,
            allow_command_glob: None,
        }),
    )];
    let blocked = evaluate_guards(
        &rules,
        &ProposedAction {
            tool: "Edit",
            path: Some("packages/database/migrations/0001-applied/up.sql"),
            command: None,
        },
    );
    assert!(blocked.is_blocked());
    assert_eq!(blocked.primary().unwrap().rule_id, "no-mig");

    let allowed = evaluate_guards(
        &rules,
        &ProposedAction {
            tool: "Edit",
            path: Some("src/app.ts"),
            command: None,
        },
    );
    assert!(!allowed.is_blocked());
}

#[test]
fn evaluate_guards_ignores_a_mismatched_tool() {
    let rules = vec![rule(
        "no-mig",
        "t",
        Some(RuleGuard {
            tool: Some("Edit".to_string()),
            deny_path_glob: Some("mig/**".to_string()),
            deny_command_glob: None,
            allow_command_glob: None,
        }),
    )];
    let check = evaluate_guards(
        &rules,
        &ProposedAction {
            tool: "Write",
            path: Some("mig/0001.sql"),
            command: None,
        },
    );
    assert!(!check.is_blocked());
}

#[test]
fn evaluate_guards_wildcard_tool_applies_to_any_tool() {
    let rules = vec![rule(
        "no-force-push",
        "Never force-push.",
        Some(RuleGuard {
            tool: None,
            deny_path_glob: None,
            deny_command_glob: Some("git push --force*".to_string()),
            allow_command_glob: None,
        }),
    )];
    let check = evaluate_guards(
        &rules,
        &ProposedAction {
            tool: "Bash",
            path: None,
            command: Some("git push --force origin main"),
        },
    );
    assert!(check.is_blocked());
}

#[test]
fn evaluate_guards_bare_tool_guard_blocks_the_whole_tool() {
    let rules = vec![rule(
        "no-bash",
        "No shell access.",
        Some(RuleGuard {
            tool: Some("Bash".to_string()),
            deny_path_glob: None,
            deny_command_glob: None,
            allow_command_glob: None,
        }),
    )];
    let check = evaluate_guards(
        &rules,
        &ProposedAction {
            tool: "Bash",
            path: None,
            command: Some("ls"),
        },
    );
    assert!(check.is_blocked());
}

#[test]
fn evaluate_guards_collects_every_violation_not_just_the_first() {
    let rules = vec![
        rule(
            "r1",
            "t1",
            Some(RuleGuard {
                tool: Some("Bash".to_string()),
                deny_path_glob: None,
                deny_command_glob: Some("rm*".to_string()),
                allow_command_glob: None,
            }),
        ),
        rule(
            "r2",
            "t2",
            Some(RuleGuard {
                tool: None,
                deny_path_glob: None,
                deny_command_glob: Some("rm*".to_string()),
                allow_command_glob: None,
            }),
        ),
    ];
    let check = evaluate_guards(
        &rules,
        &ProposedAction {
            tool: "Bash",
            path: None,
            command: Some("rm -rf /"),
        },
    );
    assert_eq!(check.violations.len(), 2);
}

#[test]
fn tier_1_only_rules_never_block_anything() {
    let rules = vec![rule("style", "Match the surrounding code style.", None)];
    let check = evaluate_guards(
        &rules,
        &ProposedAction {
            tool: "Edit",
            path: Some("anything.ts"),
            command: None,
        },
    );
    assert!(!check.is_blocked());
}

// ---- promotion mining ----

fn observation(text: &str, occurred_at: u64) -> RawObservation {
    RawObservation {
        text: text.to_string(),
        source: EvidenceSource::TraceFinding,
        reference: format!("trace:t#{occurred_at}"),
        occurred_at,
        files: Vec::new(),
        salient: false,
        memory_kind: None,
    }
}

fn salient_observation(text: &str, id: &str) -> RawObservation {
    RawObservation {
        text: text.to_string(),
        source: EvidenceSource::Memory,
        reference: format!("memory:{id}"),
        occurred_at: 1,
        files: Vec::new(),
        salient: true,
        memory_kind: None,
    }
}

#[test]
fn detects_a_recurring_lesson_as_a_candidate_with_evidence() {
    let obs = vec![
        observation("forgot to add a test for the new route", 1),
        observation("forgot to add a test for the new route", 2),
        observation("forgot to add a test for the new route", 3),
    ];
    let candidates = mine_candidates(obs, &[], &MineConfig::default());
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].text, "forgot to add a test for the new route");
    assert_eq!(candidates[0].occurrences, 3);
    assert_eq!(candidates[0].evidence.len(), 3);
    assert!(
        candidates[0]
            .evidence
            .iter()
            .all(|e| e.reference.starts_with("trace:"))
    );
}

#[test]
fn does_not_surface_a_one_off_observation_below_the_threshold() {
    let obs = vec![observation("one-off issue nobody else hit", 1)];
    assert!(mine_candidates(obs, &[], &MineConfig::default()).is_empty());
}

#[test]
fn promotes_a_single_salient_observation_regardless_of_occurrence_count() {
    let obs = vec![salient_observation(
        "database credentials leaked in a log line",
        "mem_1",
    )];
    let candidates = mine_candidates(obs, &[], &MineConfig::default());
    assert_eq!(candidates.len(), 1);
    assert!(candidates[0].salient);
    assert_eq!(candidates[0].occurrences, 1);
    assert_eq!(candidates[0].evidence[0].reference, "memory:mem_1");
}

#[test]
fn ranks_a_salient_single_observation_above_a_merely_recurring_one() {
    let mut obs = vec![
        observation("forgot to add a test for the new route", 1),
        observation("forgot to add a test for the new route", 2),
        observation("forgot to add a test for the new route", 3),
    ];
    obs.push(salient_observation(
        "database credentials leaked in a log line",
        "mem_1",
    ));
    let candidates = mine_candidates(obs, &[], &MineConfig::default());
    assert_eq!(candidates.len(), 2);
    assert!(candidates[0].salient);
}

#[test]
fn does_not_surface_a_candidate_that_duplicates_an_existing_rule() {
    let obs = vec![
        observation("never force-push to a shared branch", 1),
        observation("never force-push to a shared branch", 2),
        observation("never force-push to a shared branch", 3),
    ];
    let existing = vec![rule(
        "no-force-push",
        "Never force-push to a shared branch.",
        None,
    )];
    assert!(mine_candidates(obs, &existing, &MineConfig::default()).is_empty());
}

#[test]
fn infers_a_deny_path_guard_from_consistent_file_evidence() {
    let obs = vec![
        RawObservation {
            text: "never edit an applied migration file directly".to_string(),
            source: EvidenceSource::Memory,
            reference: "memory:m1".to_string(),
            occurred_at: 1,
            files: vec!["packages/database/migrations/0001-applied/up.sql".to_string()],
            salient: true,
            memory_kind: Some("gotcha".to_string()),
        },
        RawObservation {
            text: "never edit an applied migration file directly".to_string(),
            source: EvidenceSource::Memory,
            reference: "memory:m2".to_string(),
            occurred_at: 2,
            files: vec!["packages/database/migrations/0002-applied/up.sql".to_string()],
            salient: true,
            memory_kind: Some("gotcha".to_string()),
        },
    ];
    let candidates = mine_candidates(obs, &[], &MineConfig::default());
    assert_eq!(
        candidates[0].guard,
        Some(RuleGuard {
            tool: None,
            deny_path_glob: Some("packages/database/migrations/**".to_string()),
            deny_command_glob: None,
            allow_command_glob: None,
        })
    );
}

#[test]
fn leaves_candidate_prompt_only_when_files_share_no_common_directory() {
    let obs = vec![
        RawObservation {
            text: "shared lesson text here".to_string(),
            source: EvidenceSource::Memory,
            reference: "memory:m1".to_string(),
            occurred_at: 1,
            files: vec!["a/one.ts".to_string()],
            salient: true,
            memory_kind: Some("gotcha".to_string()),
        },
        RawObservation {
            text: "shared lesson text here".to_string(),
            source: EvidenceSource::Memory,
            reference: "memory:m2".to_string(),
            occurred_at: 2,
            files: vec!["b/two.ts".to_string()],
            salient: true,
            memory_kind: Some("gotcha".to_string()),
        },
    ];
    let candidates = mine_candidates(obs, &[], &MineConfig::default());
    assert!(candidates[0].guard.is_none());
}

#[test]
fn mine_candidates_respects_the_limit() {
    // Five lessons with disjoint vocabulary (a shared
    // boilerplate template — e.g. "lesson about {word} handling" —
    // would keep 3 of 4 terms identical across "different" lessons,
    // pushing Jaccard similarity above the default clustering threshold
    // and collapsing them into one cluster instead of five), each
    // recurring three times so all five clear the occurrence
    // threshold and become candidates before truncation.
    let lessons = [
        "forgot to add a test for the new route",
        "database credentials appeared in application logs",
        "team members must not force push shared branches",
        "response handler skipped a required null check",
        "queries used the wrong tenant scope entirely",
    ];
    let mut obs = Vec::new();
    for lesson in lessons {
        for occurrence in 0..3u64 {
            obs.push(observation(lesson, occurrence));
        }
    }
    let config = MineConfig {
        limit: 2,
        ..MineConfig::default()
    };
    let candidates = mine_candidates(obs, &[], &config);
    assert_eq!(candidates.len(), 2);
}

#[test]
fn mine_candidates_is_deterministic_across_reruns() {
    let obs = vec![
        observation("forgot to add a test for the new route", 1),
        observation("forgot to add a test for the new route", 2),
        observation("forgot to add a test for the new route", 3),
    ];
    let first = mine_candidates(obs.clone(), &[], &MineConfig::default());
    let second = mine_candidates(obs, &[], &MineConfig::default());
    assert_eq!(first[0].id, second[0].id);
}

// ---- decide_promotion ----

#[test]
fn decide_promotion_declines_without_approval() {
    assert_eq!(decide_promotion(false, false), PromoteStatus::Declined);
    assert_eq!(decide_promotion(false, true), PromoteStatus::Declined);
}

#[test]
fn decide_promotion_refuses_to_clobber_an_existing_file() {
    assert_eq!(decide_promotion(true, true), PromoteStatus::AlreadyExists);
}

#[test]
fn decide_promotion_writes_when_approved_and_absent() {
    assert_eq!(decide_promotion(true, false), PromoteStatus::Written);
}

// ---- enforce: guard-allow-command (the deny exception, stella #5128) ----
//
// The exception exists because every other guard field is a positive match,
// which cannot express "this family is forbidden except in its scoped form" —
// the shape SCR-001 has, and the shape most operational command rules have.

/// A guard denying a command family with one scoped form excepted.
fn scoped_only(deny: &str, allow: &str) -> Option<RuleGuard> {
    Some(RuleGuard {
        tool: Some("Bash".to_string()),
        deny_path_glob: None,
        deny_command_glob: Some(deny.to_string()),
        allow_command_glob: Some(allow.to_string()),
    })
}

fn bash(command: &str) -> ProposedAction<'_> {
    ProposedAction {
        tool: "Bash",
        path: None,
        command: Some(command),
    }
}

#[test]
fn guard_allow_command_exempts_the_scoped_form_and_blocks_the_rest() {
    // The SCR-001 case exactly: workspace-wide test compiles are denied, the
    // per-crate form is not. Neither half is expressible as a single positive
    // glob, which is why the field exists.
    let rules = vec![rule(
        "scr-001",
        "Scope test builds to the touched crate.",
        scoped_only("*cargo test*", "*cargo test*-p *"),
    )];

    assert!(evaluate_guards(&rules, &bash("cargo test")).is_blocked());
    assert!(evaluate_guards(&rules, &bash("cargo test --workspace")).is_blocked());
    // The spelling a list of deny globs would have missed: scoped to nothing,
    // but not spelled with any of the flags someone thought to enumerate.
    assert!(evaluate_guards(&rules, &bash("cargo test some_filter")).is_blocked());

    assert!(!evaluate_guards(&rules, &bash("cargo test -p stella-core")).is_blocked());
    assert!(!evaluate_guards(&rules, &bash("cargo test -p stella-core rules::")).is_blocked());
}

#[test]
fn a_guard_without_an_exception_is_unchanged() {
    // The field is additive: every guard written before it behaves exactly as
    // it did, which is the property that makes adding it safe.
    let rules = vec![rule(
        "no-force-push",
        "Never force-push.",
        Some(RuleGuard {
            tool: Some("Bash".to_string()),
            deny_path_glob: None,
            deny_command_glob: Some("git push --force*".to_string()),
            allow_command_glob: None,
        }),
    )];
    assert!(evaluate_guards(&rules, &bash("git push --force")).is_blocked());
    assert!(!evaluate_guards(&rules, &bash("git push")).is_blocked());
}

#[test]
fn an_exception_cannot_unlock_a_path_guard_beside_it() {
    // The exception is command-scoped on purpose. If it suppressed the whole
    // guard, one permissive command glob would quietly disarm a `deny-path`
    // condition it was never written to reason about — and a guard that stops
    // firing looks exactly like a guard that was never violated.
    let rules = vec![rule(
        "locked",
        "Do not touch secrets.",
        Some(RuleGuard {
            tool: Some("*".to_string()),
            deny_path_glob: Some("secrets/**".to_string()),
            deny_command_glob: Some("rm -rf*".to_string()),
            allow_command_glob: Some("rm -rf /tmp/*".to_string()),
        }),
    )];

    // The command exception applies to the command branch...
    assert!(!evaluate_guards(&rules, &bash("rm -rf /tmp/build")).is_blocked());
    assert!(evaluate_guards(&rules, &bash("rm -rf /etc")).is_blocked());

    // ...and leaves the path branch untouched.
    let write = ProposedAction {
        tool: "Write",
        path: Some("secrets/token.txt"),
        command: None,
    };
    assert!(evaluate_guards(&rules, &write).is_blocked());
}

#[test]
fn an_exception_cannot_soften_a_whole_tool_block() {
    // `guard-tool: Bash` with no deny glob blocks the tool outright. Letting
    // an exception through there would turn this into a whole-tool allowlist,
    // which is a different feature wearing this one's name.
    let rules = vec![rule(
        "no-bash",
        "No shell in this workspace.",
        Some(RuleGuard {
            tool: Some("Bash".to_string()),
            deny_path_glob: None,
            deny_command_glob: None,
            allow_command_glob: Some("echo *".to_string()),
        }),
    )];
    assert!(evaluate_guards(&rules, &bash("echo hi")).is_blocked());
}

#[test]
fn guard_allow_command_parses_from_frontmatter_in_both_spellings() {
    for key in ["guard-allow-command", "guard_allow_command"] {
        let parsed = rule_from_file(
            ".stella/rules/scr-001.md",
            &format!(
                "---\nguard-tool: Bash\nguard-deny-command: '*cargo test*'\n\
                 {key}: '*cargo test*-p *'\n---\nScope test builds."
            ),
        )
        .unwrap();
        assert_eq!(
            parsed.guard.as_ref().unwrap().allow_command_glob.as_deref(),
            Some("*cargo test*-p *"),
            "{key} should parse",
        );
    }
}

#[test]
fn a_blank_exception_is_absent_rather_than_an_empty_glob() {
    // Same reason the other guard fields treat blank as absent: an empty glob
    // is not a condition, and one that matched everything would silently
    // disarm the deny it sits beside.
    let parsed = rule_from_file(
        ".stella/rules/x.md",
        "---\nguard-tool: Bash\nguard-deny-command: 'rm *'\nguard-allow-command:\n---\nCareful.",
    )
    .unwrap();
    assert_eq!(parsed.guard.as_ref().unwrap().allow_command_glob, None);
    assert!(evaluate_guards(&[parsed], &bash("rm x")).is_blocked());
}

#[test]
fn an_exception_alone_does_not_manufacture_a_guard() {
    // Nothing to except from means nothing is denied. Treating this as Tier 2
    // would advertise enforcement that structurally cannot fire.
    let parsed = rule_from_file(
        ".stella/rules/x.md",
        "---\nguard-allow-command: 'cargo test*'\n---\nPrefer scoped tests.",
    )
    .unwrap();
    assert!(parsed.guard.is_none());
    assert_eq!(parsed.tier(), RuleTier::Prompt);
}
