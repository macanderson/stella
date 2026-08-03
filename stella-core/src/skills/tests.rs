//! Tests for [`crate::skills`].
//!
//! Extracted verbatim from the inline `mod tests` in `skills.rs` — a pure
//! move, no behavior change. `skills.rs` sat exactly at the 1500-line file-size
//! ceiling with no headroom, and Phase 3 (#714) adds to it; splitting the tests
//! to a sibling module is the sanctioned way to make room without raising a
//! baseline.

use super::*;

// ---- parsing: frontmatter, layouts, diagnostics ----

#[test]
fn parses_name_description_domains_and_body() {
    let raw = "---\nname: sql-style\ndescription: Format SQL with lowercase keywords\ndomains: sql, formatting\n---\nUse lowercase keywords and two-space indentation.";
    let skill = skill_from_file(".stella/skills/sql-style.md", raw).unwrap();
    assert_eq!(skill.name, "sql-style");
    assert_eq!(skill.description, "Format SQL with lowercase keywords");
    assert_eq!(skill.domains, vec!["sql", "formatting"]);
    assert!(skill.body.contains("lowercase keywords"));
}

#[test]
fn parses_bracketed_domains_list_and_strips_quotes() {
    let raw = "---\nname: t\ndescription: \"quoted desc\"\ndomains: [\"sql\", 'graph']\n---\nbody";
    let skill = skill_from_file("t.md", raw).unwrap();
    assert_eq!(skill.description, "quoted desc");
    assert_eq!(skill.domains, vec!["sql", "graph"]);
}

#[test]
fn ignores_unknown_frontmatter_keys() {
    let raw = "---\nname: t\ndescription: d\nunknown-key: whatever\nanother: 123\n---\nbody";
    let skill = skill_from_file("t.md", raw).unwrap();
    assert_eq!(skill.name, "t");
    assert_eq!(skill.description, "d");
}

#[test]
fn missing_description_is_a_typed_diagnostic() {
    let err = skill_from_file("t.md", "---\nname: t\n---\nbody").unwrap_err();
    assert_eq!(err.problem, SkillProblem::MissingDescription);
    assert_eq!(err.path, "t.md");
}

#[test]
fn empty_body_is_a_typed_diagnostic() {
    let err = skill_from_file("t.md", "---\nname: t\ndescription: d\n---\n").unwrap_err();
    assert_eq!(err.problem, SkillProblem::EmptyBody);
}

#[test]
fn missing_name_with_unusable_path_is_a_typed_diagnostic() {
    // Bare `SKILL.md` with no parent dir and no frontmatter name yields
    // no usable slug.
    let err = skill_from_file("SKILL.md", "just a body, no frontmatter").unwrap_err();
    assert_eq!(err.problem, SkillProblem::MissingName);
}

#[test]
fn nested_layout_derives_slug_from_parent_directory() {
    // The `npx skills` ecosystem layout: <dir>/<slug>/SKILL.md.
    let skill = skill_from_file(
        "/ws/.stella/skills/pdf-extract/SKILL.md",
        "---\ndescription: d\n---\nbody",
    )
    .unwrap();
    assert_eq!(skill.name, "pdf-extract");
}

#[test]
fn flat_layout_derives_slug_from_filename_stem() {
    let skill = skill_from_file(
        "/ws/.stella/skills/pdf-extract.md",
        "---\ndescription: d\n---\nbody",
    )
    .unwrap();
    assert_eq!(skill.name, "pdf-extract");
}

#[test]
fn frontmatter_name_overrides_the_path_slug() {
    let skill = skill_from_file(
        "/ws/.stella/skills/whatever/SKILL.md",
        "---\nname: real-name\ndescription: d\n---\nbody",
    )
    .unwrap();
    assert_eq!(skill.name, "real-name");
}

#[test]
fn origin_marker_auto_and_installed_are_read_back() {
    let auto = skill_from_file(
        "a.md",
        "---\nname: a\ndescription: d\norigin: auto\n---\nbody",
    )
    .unwrap();
    assert_eq!(auto.origin, SkillOrigin::AutoCreated);
    let installed = skill_from_file(
        "b.md",
        "---\nname: b\ndescription: d\norigin: installed\n---\nbody",
    )
    .unwrap();
    assert_eq!(installed.origin, SkillOrigin::Installed);
}

// ---- discovery + precedence, against a fake SkillSource ----

struct FakeSkillSource {
    by_dir: HashMap<String, Vec<SkillFile>>,
}

impl SkillSource for FakeSkillSource {
    fn read_skill_files(&self, roots: &[String]) -> Vec<SkillFile> {
        let mut out = Vec::new();
        for root in roots {
            if let Some(files) = self.by_dir.get(root) {
                out.extend(files.iter().cloned());
            }
        }
        out
    }
}

fn opts() -> LoadSkillsOptions {
    LoadSkillsOptions {
        workspace_skills_dir: "/ws/.stella/skills".to_string(),
        user_skills_dir: "/home/u/.stella/skills".to_string(),
    }
}

fn skill_file(dir: &str, name: &str, content: &str) -> SkillFile {
    SkillFile {
        path: format!("{dir}/{name}"),
        content: content.to_string(),
    }
}

#[test]
fn loads_a_skill_end_to_end_with_workspace_origin() {
    let o = opts();
    let dirs = skill_search_dirs(&o);
    let mut by_dir = HashMap::new();
    by_dir.insert(
        dirs[1].clone(), // workspace dir
        vec![skill_file(
            &dirs[1],
            "tables.md",
            "---\nname: prefer-tables\ndescription: Prefer tables over prose for comparisons\n---\nWhen comparing options, render a table.",
        )],
    );
    let source = FakeSkillSource { by_dir };
    let skills = load_skills(&source, &o);
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "prefer-tables");
    assert_eq!(skills[0].origin, SkillOrigin::Workspace);
}

#[test]
fn workspace_beats_user_global_on_a_name_collision() {
    let o = opts();
    let dirs = skill_search_dirs(&o);
    let mut by_dir = HashMap::new();
    by_dir.insert(
        dirs[0].clone(), // user dir
        vec![skill_file(
            &dirs[0],
            "s.md",
            "---\nname: s\ndescription: user version\n---\nuser body",
        )],
    );
    by_dir.insert(
        dirs[1].clone(), // workspace dir
        vec![skill_file(
            &dirs[1],
            "s.md",
            "---\nname: s\ndescription: workspace version\n---\nworkspace body",
        )],
    );
    let source = FakeSkillSource { by_dir };
    let skills = load_skills(&source, &o);
    let s = skills.iter().find(|s| s.name == "s").unwrap();
    assert_eq!(s.description, "workspace version");
    assert_eq!(s.origin, SkillOrigin::Workspace);
}

#[test]
fn load_with_diagnostics_surfaces_skipped_files() {
    let o = opts();
    let dirs = skill_search_dirs(&o);
    let mut by_dir = HashMap::new();
    by_dir.insert(
        dirs[1].clone(),
        vec![
            skill_file(
                &dirs[1],
                "ok.md",
                "---\nname: ok\ndescription: d\n---\nbody",
            ),
            skill_file(&dirs[1], "bad.md", "---\nname: bad\n---\nbody"), // no description
        ],
    );
    let source = FakeSkillSource { by_dir };
    let loaded = load_skills_with_diagnostics(&source, &o);
    assert_eq!(loaded.skills.len(), 1);
    assert_eq!(loaded.diagnostics.len(), 1);
    assert_eq!(
        loaded.diagnostics[0].problem,
        SkillProblem::MissingDescription
    );
}

fn skill(name: &str, description: &str, domains: &[&str], origin: SkillOrigin) -> Skill {
    Skill {
        name: name.to_string(),
        description: description.to_string(),
        domains: domains.iter().map(|d| d.to_string()).collect(),
        body: format!("body of {name}"),
        source_path: format!("{name}.md"),
        origin,
    }
}

// ---- selection ----

#[test]
fn selection_populates_the_why_fields() {
    let skills = vec![skill(
        "sql-style",
        "format sql queries nicely",
        &["sql"],
        SkillOrigin::Workspace,
    )];
    let selected = select_skills(
        &skills,
        "please format the sql for me",
        &["sql".to_string()],
        &SelectionConfig::default(),
    );
    assert_eq!(selected.len(), 1);
    assert!(selected[0].matched_terms.contains(&"sql".to_string()));
    assert!(selected[0].matched_terms.contains(&"format".to_string()));
    assert_eq!(selected[0].matched_domains, vec!["sql"]);
}

#[test]
fn domain_boost_wins_over_a_weak_lexical_match() {
    let skills = vec![
        // No domain, weak lexical overlap with the prompt.
        skill(
            "verbose-logging",
            "add verbose logging everywhere",
            &[],
            SkillOrigin::Workspace,
        ),
        // Domain match, but its wording barely overlaps the prompt.
        skill(
            "migration-safety",
            "irreversible ddl needs review",
            &["database"],
            SkillOrigin::Workspace,
        ),
    ];
    let selected = select_skills(
        &skills,
        "add verbose stuff",
        &["database".to_string()],
        &SelectionConfig::default(),
    );
    // The domain-matched skill should rank first despite weaker wording.
    assert_eq!(selected[0].skill.name, "migration-safety");
}

#[test]
fn below_threshold_skills_are_excluded() {
    let skills = vec![skill(
        "unrelated",
        "something about kubernetes pods",
        &[],
        SkillOrigin::Workspace,
    )];
    let selected = select_skills(
        &skills,
        "help me write a poem about the ocean",
        &[],
        &SelectionConfig::default(),
    );
    assert!(selected.is_empty());
}

#[test]
fn top_k_is_respected() {
    let skills: Vec<Skill> = (0..5)
        .map(|i| {
            skill(
                &format!("sql-skill-{i}"),
                "format sql queries",
                &["sql"],
                SkillOrigin::Workspace,
            )
        })
        .collect();
    let config = SelectionConfig {
        max_skills: 2,
        ..SelectionConfig::default()
    };
    let selected = select_skills(&skills, "format sql", &["sql".to_string()], &config);
    assert_eq!(selected.len(), 2);
}

#[test]
fn auto_created_skill_wins_the_tie_break() {
    // Two skills with identical wording/domain → identical base score; the
    // AutoCreated one (a freshly-learned preference) sorts first.
    let skills = vec![
        skill(
            "z-workspace",
            "prefer tables for comparisons",
            &[],
            SkillOrigin::Workspace,
        ),
        skill(
            "a-auto",
            "prefer tables for comparisons",
            &[],
            SkillOrigin::AutoCreated,
        ),
    ];
    let selected = select_skills(
        &skills,
        "prefer tables for comparisons please",
        &[],
        &SelectionConfig::default(),
    );
    assert_eq!(selected[0].skill.name, "a-auto");
    assert!(selected[0].score > selected[1].score);
}

// ---- rendering ----

#[test]
fn render_section_is_empty_with_no_skills() {
    assert_eq!(render_skills_section(&[]), "");
}

#[test]
fn render_section_includes_name_description_and_body() {
    let selected = vec![SelectedSkill {
        skill: skill("s", "the description", &[], SkillOrigin::Workspace),
        score: 1.0,
        matched_terms: vec![],
        matched_domains: vec![],
    }];
    let out = render_skills_section(&selected);
    assert!(out.contains("Applicable skills"));
    assert!(out.contains("### s"));
    assert!(out.contains("the description"));
    assert!(out.contains("body of s"));
}

#[test]
fn render_section_truncates_an_over_budget_body_with_a_marker() {
    let big_body = "word ".repeat(2000); // ~2857 tokens, over the 400 budget
    let mut s = skill("s", "d", &[], SkillOrigin::Workspace);
    s.body = big_body;
    let selected = vec![SelectedSkill {
        skill: s,
        score: 1.0,
        matched_terms: vec![],
        matched_domains: vec![],
    }];
    let out = render_skills_section(&selected);
    assert!(out.contains("truncated to fit the context budget"));
}

#[test]
fn render_section_drops_extra_skills_past_the_total_budget() {
    // Six skills each near the per-skill budget overrun the section total.
    let selected: Vec<SelectedSkill> = (0..6)
        .map(|i| {
            let mut s = skill(&format!("s{i}"), "d", &[], SkillOrigin::Workspace);
            s.body = "word ".repeat(300); // ~430 tokens each after estimate
            SelectedSkill {
                skill: s,
                score: 1.0,
                matched_terms: vec![],
                matched_domains: vec![],
            }
        })
        .collect();
    let out = render_skills_section(&selected);
    assert!(out.contains("additional lower-ranked skills omitted"));
    // At least the first skill always renders.
    assert!(out.contains("### s0"));
}

// ---- mining ----

fn observation(text: &str, occurred_at: u64) -> SkillObservation {
    SkillObservation {
        text: text.to_string(),
        occurred_at,
        domains: vec![],
        salient: false,
        reference: format!("trace:t#{occurred_at}"),
    }
}

#[test]
fn mines_a_recurring_preference_at_the_threshold() {
    let obs = vec![
        observation("the user prefers tables over prose for comparisons", 1),
        observation("the user prefers tables over prose for comparisons", 2),
        observation("the user prefers tables over prose for comparisons", 3),
    ];
    let candidates = mine_skill_candidates(obs, &[], &SkillMineConfig::default());
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].occurrences, 3);
    assert_eq!(candidates[0].evidence.len(), 3);
    assert!(candidates[0].body.contains("prefers tables"));
}

#[test]
fn does_not_mine_a_one_off_below_the_threshold() {
    let obs = vec![observation("a one-time thing nobody repeated", 1)];
    assert!(mine_skill_candidates(obs, &[], &SkillMineConfig::default()).is_empty());
}

#[test]
fn mines_a_single_salient_observation() {
    let obs = vec![SkillObservation {
        text: "always use pnpm, never npm, in this repo".to_string(),
        occurred_at: 1,
        domains: vec!["tooling".to_string()],
        salient: true,
        reference: "memory:m1".to_string(),
    }];
    let candidates = mine_skill_candidates(obs, &[], &SkillMineConfig::default());
    assert_eq!(candidates.len(), 1);
    assert!(candidates[0].salient);
    assert_eq!(candidates[0].occurrences, 1);
    assert_eq!(candidates[0].domains, vec!["tooling"]);
}

#[test]
fn dedups_against_an_existing_skill() {
    let obs = vec![
        observation("prefer tables over prose for comparisons", 1),
        observation("prefer tables over prose for comparisons", 2),
        observation("prefer tables over prose for comparisons", 3),
    ];
    let existing = vec![skill(
        "tables",
        "Prefer tables over prose for comparisons",
        &[],
        SkillOrigin::Workspace,
    )];
    assert!(mine_skill_candidates(obs, &existing, &SkillMineConfig::default()).is_empty());
}

#[test]
fn mining_unions_domains_across_the_cluster() {
    let obs = vec![
        SkillObservation {
            text: "format sql with lowercase keywords".to_string(),
            occurred_at: 1,
            domains: vec!["sql".to_string()],
            salient: false,
            reference: "t#1".to_string(),
        },
        SkillObservation {
            text: "format sql with lowercase keywords".to_string(),
            occurred_at: 2,
            domains: vec!["formatting".to_string()],
            salient: false,
            reference: "t#2".to_string(),
        },
        SkillObservation {
            text: "format sql with lowercase keywords".to_string(),
            occurred_at: 3,
            domains: vec!["SQL".to_string()], // case-insensitive dup of "sql"
            salient: false,
            reference: "t#3".to_string(),
        },
    ];
    let candidates = mine_skill_candidates(obs, &[], &SkillMineConfig::default());
    assert_eq!(candidates[0].domains, vec!["sql", "formatting"]);
}

#[test]
fn mining_respects_the_limit() {
    let lessons = [
        "always add a regression test for every bug fix",
        "database credentials must never appear in log output",
        "team members must not force push shared branches",
        "response handlers should validate required null fields",
        "queries must always use the correct tenant scope",
    ];
    let mut obs = Vec::new();
    for lesson in lessons {
        for occurrence in 0..3u64 {
            obs.push(observation(lesson, occurrence));
        }
    }
    let config = SkillMineConfig {
        limit: 2,
        ..SkillMineConfig::default()
    };
    assert_eq!(mine_skill_candidates(obs, &[], &config).len(), 2);
}

#[test]
fn mining_is_deterministic_across_reruns() {
    let obs = vec![
        observation("prefer tables over prose for comparisons", 1),
        observation("prefer tables over prose for comparisons", 2),
        observation("prefer tables over prose for comparisons", 3),
    ];
    let first = mine_skill_candidates(obs.clone(), &[], &SkillMineConfig::default());
    let second = mine_skill_candidates(obs, &[], &SkillMineConfig::default());
    assert_eq!(first[0].name, second[0].name);
}

// ---- markdown round-trip ----

#[test]
fn rendered_markdown_round_trips_through_the_parser() {
    let obs = vec![SkillObservation {
        text: "prefer tables over prose for comparisons".to_string(),
        occurred_at: 1,
        domains: vec!["formatting".to_string(), "docs".to_string()],
        salient: true,
        reference: "memory:m1".to_string(),
    }];
    let candidates = mine_skill_candidates(obs, &[], &SkillMineConfig::default());
    let candidate = &candidates[0];
    let markdown = render_skill_markdown(candidate);
    let parsed = skill_from_file(&format!("{}.md", candidate.name), &markdown).unwrap();
    assert_eq!(parsed.name, candidate.name);
    assert_eq!(parsed.description, candidate.description);
    assert_eq!(parsed.domains, candidate.domains);
    // The `origin: auto` marker means a reload tags it AutoCreated.
    assert_eq!(parsed.origin, SkillOrigin::AutoCreated);
    assert!(parsed.body.contains("prefer tables"));
    assert!(parsed.body.contains("## Evidence"));
}

// ---- auto-creation decision ----

fn a_candidate() -> SkillCandidate {
    SkillCandidate {
        name: "prefer-tables".to_string(),
        description: "Learned from 3 observations.".to_string(),
        domains: vec![],
        body: "prefer tables".to_string(),
        occurrences: 3,
        salient: false,
        evidence: vec![],
        score: 30.0,
    }
}

#[test]
fn auto_create_writes_when_absent_and_under_the_cap() {
    let decision = decide_auto_creation(
        &a_candidate(),
        "/ws/.stella/skills",
        &[],
        0,
        appraisal::EvalEvidence::MeasuredLift,
        &AutoCreateConfig::default(),
    );
    assert_eq!(
        decision,
        AutoCreateDecision::Create {
            path: "/ws/.stella/skills/prefer-tables.md".to_string()
        }
    );
}

#[test]
fn auto_create_refuses_to_clobber_an_existing_file() {
    let existing = vec!["/ws/.stella/skills/prefer-tables.md".to_string()];
    let decision = decide_auto_creation(
        &a_candidate(),
        "/ws/.stella/skills",
        &existing,
        0,
        appraisal::EvalEvidence::MeasuredLift,
        &AutoCreateConfig::default(),
    );
    assert!(matches!(
        decision,
        AutoCreateDecision::Skip {
            reason: AutoCreateSkip::FileExists { .. }
        }
    ));
}

#[test]
fn auto_create_stops_at_the_session_cap() {
    let decision = decide_auto_creation(
        &a_candidate(),
        "/ws/.stella/skills",
        &[],
        2, // already created 2 this session
        appraisal::EvalEvidence::MeasuredLift,
        &AutoCreateConfig::default(),
    );
    assert_eq!(
        decision,
        AutoCreateDecision::Skip {
            reason: AutoCreateSkip::SessionCapReached { cap: 2 }
        }
    );
}

// ---- install vocabulary ----

#[test]
fn install_proposal_and_decision_are_constructible() {
    let proposal = SkillInstallProposal {
        query: "pdf extraction".to_string(),
        registry_result_summary: "pdf-extract — extract text and tables from PDFs".to_string(),
        target_dir_hint: "/ws/.stella/skills".to_string(),
    };
    assert_eq!(proposal.query, "pdf extraction");
    assert_ne!(InstallDecision::Confirmed, InstallDecision::Declined);
}

// ---- proptest: render/parse round-trip over awkward content ----

proptest::proptest! {
    #[test]
    fn render_parse_round_trip_preserves_name_description_domains(
        name in "[a-z][a-z0-9-]{0,29}",
        description in "[a-zA-Z0-9 .,!?()-]{1,60}",
        domains in proptest::collection::vec("[a-z][a-z0-9-]{0,15}", 0..4),
    ) {
        // `description:` is trimmed on parse, so an all-whitespace one
        // would fail to load — skip those (never produced by mining).
        proptest::prop_assume!(!description.trim().is_empty());
        // `parse_domains` intentionally dedups case-insensitively (matching
        // `union_domains`), so a round-trip only preserves duplicate-free
        // lists — normalize the input to that real-world invariant.
        let mut seen = std::collections::HashSet::new();
        let domains: Vec<String> = domains
            .iter()
            .filter(|d| seen.insert(d.to_ascii_lowercase()))
            .cloned()
            .collect();

        let candidate = SkillCandidate {
            name: name.clone(),
            description: description.clone(),
            domains: domains.clone(),
            body: "some body text".to_string(),
            occurrences: 1,
            salient: false,
            evidence: vec![],
            score: 10.0,
        };
        let markdown = render_skill_markdown(&candidate);
        let parsed = skill_from_file(&format!("{name}.md"), &markdown).unwrap();
        proptest::prop_assert_eq!(&parsed.name, &name);
        proptest::prop_assert_eq!(&parsed.description, description.trim());
        proptest::prop_assert_eq!(&parsed.domains, &domains);
        proptest::prop_assert_eq!(parsed.origin, SkillOrigin::AutoCreated);
    }
}
