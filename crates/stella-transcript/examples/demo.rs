//! Render the reference session to both surfaces.
//!
//! `cargo run -p stella-transcript --example demo -- html > demo.html`
//! `cargo run -p stella-transcript --example demo -- tui`
//!
//! The fixture is the session the design was drawn against, so the output is
//! directly comparable with the reference renderings — which is the only way to
//! check a visual change without a human staring at a live run.

use stella_transcript::digest::{self};
use stella_transcript::model::*;
use stella_transcript::{FoldState, Zoom, grid, html};

fn main() {
    let run = fixture();
    let mut state = FoldState::new();
    state.set_zoom(Zoom::Steps);

    match std::env::args().nth(1).as_deref() {
        Some("tui") => {
            println!("{}", grid::to_ansi256(&grid::render(&run, &state, 118)));
        }
        Some("plain") => {
            println!("{}", grid::to_plain(&grid::render(&run, &state, 118)));
        }
        _ => print!("{}", html::render_page(&run, &state)),
    }
    eprintln!(
        "{} turns · {} steps · {}",
        run.turns.len(),
        run.step_count(),
        digest::format_cost(run.rollup().micros)
    );
}

fn accounting(micros: u64) -> Accounting {
    Accounting {
        tokens_in: 7_000,
        tokens_out: 34,
        cached_in: 6_400,
        micros,
    }
}

fn step(call: Call, offset_ms: u64, micros: u64) -> Step {
    Step {
        call: Some(call),
        accounting: accounting(micros),
        offset_ms,
    }
}

const MAIN_TEX_BEFORE: &str = "\\documentclass{article}\n\
\\usepackage[margin=1in]{geometry}\n\
\\setlength{\\parindent}{15pt}\n\
\\begin{document}\n\
In my younger and more vulnerable years my father gave me some advice\n\
that I've been turning over in my mind ever since.\n\
\\end{document}\n";

const MAIN_TEX_AFTER: &str = "\\documentclass{article}\n\
\\usepackage[margin=1in]{geometry}\n\
\\usepackage{microtype}\n\
\\setlength{\\parindent}{12pt}\n\
\\emergencystretch=1em\n\
\\begin{document}\n\
In my younger and more vulnerable years my father gave me a piece of advice\n\
that I've been turning over in my mind ever since.\n\
\\end{document}\n";

fn fixture() -> Run {
    let read = Call {
        tool: ToolKind::ReadFile,
        header_object: "app/main.tex".to_string(),
        args: Vec::new(),
        output: Output::from_text(MAIN_TEX_BEFORE),
        files: Vec::new(),
        status: Status::Ok,
        duration_ms: 9,
        speculated: false,
        sub_agent_id: None,
    };

    let reproduce = Call {
        tool: ToolKind::Bash,
        header_object: "pdflatex -interaction=nonstopmode main.tex 2>&1 | grep -i overfull"
            .to_string(),
        args: vec![ArgRow {
            key: "cwd".to_string(),
            value: "/app".to_string(),
        }],
        output: Output::from_text(
            "Overfull \\hbox (0.10312pt too wide) in paragraph at lines 5--6\n\
             Overfull \\hbox (2.31043pt too wide) in paragraph at lines 9--12\n\
             Overfull \\hbox (0.88709pt too wide) in paragraph at lines 14--14\n",
        ),
        files: Vec::new(),
        status: Status::Warn,
        duration_ms: 1_400,
        speculated: false,
        sub_agent_id: None,
    };

    let edit = Call {
        tool: ToolKind::EditFile,
        header_object: "app/main.tex".to_string(),
        args: Vec::new(),
        output: Output::default(),
        files: vec![FileChange {
            path: "app/main.tex".to_string(),
            before: MAIN_TEX_BEFORE.to_string(),
            after: MAIN_TEX_AFTER.to_string(),
            status: FileStatus::Modified,
            patch: None,
        }],
        status: Status::Ok,
        duration_ms: 34,
        speculated: false,
        sub_agent_id: None,
    };

    let write = Call {
        tool: ToolKind::WriteFile,
        header_object: "app/.latexmkrc".to_string(),
        args: Vec::new(),
        output: Output::default(),
        files: vec![FileChange {
            path: "app/.latexmkrc".to_string(),
            before: String::new(),
            after: "$pdf_mode = 1;\n$clean_ext = 'aux log out';\n\
                    @default_files = ('main.tex');\n"
                .to_string(),
            status: FileStatus::New,
            patch: None,
        }],
        status: Status::Ok,
        duration_ms: 11,
        speculated: true,
        sub_agent_id: None,
    };

    let verify = Call {
        tool: ToolKind::Bash,
        header_object: "pdflatex -interaction=nonstopmode main.tex 2>&1 | grep -ci overfull"
            .to_string(),
        args: Vec::new(),
        output: Output::from_text("0\n"),
        files: Vec::new(),
        status: Status::Ok,
        duration_ms: 1_300,
        speculated: false,
        sub_agent_id: None,
    };

    let delete = Call {
        tool: ToolKind::DeleteFile,
        header_object: "app/main.aux".to_string(),
        args: Vec::new(),
        output: Output::default(),
        files: vec![FileChange {
            path: "app/main.aux".to_string(),
            before: "\\relax\n\\gdef \\@abspage@last{1}\n".to_string(),
            after: String::new(),
            status: FileStatus::Deleted,
            patch: None,
        }],
        status: Status::Ok,
        duration_ms: 4,
        speculated: false,
        sub_agent_id: None,
    };

    let setup = Turn {
        name: "setup".to_string(),
        prompt: "scaffold the latex project".to_string(),
        prose: Vec::new(),
        steps: vec![
            step(
                Call {
                    tool: ToolKind::WriteFile,
                    header_object: "app/main.tex".to_string(),
                    args: Vec::new(),
                    output: Output::default(),
                    files: vec![FileChange {
                        path: "app/main.tex".to_string(),
                        before: String::new(),
                        after: MAIN_TEX_BEFORE.to_string(),
                        status: FileStatus::New,
                        patch: None,
                    }],
                    status: Status::Ok,
                    duration_ms: 8,
                    speculated: false,
                    sub_agent_id: None,
                },
                1_000,
                300,
            ),
            step(
                Call {
                    tool: ToolKind::Bash,
                    header_object: "pdflatex main.tex".to_string(),
                    args: Vec::new(),
                    output: Output::from_text("Output written on main.pdf (1 page).\n"),
                    files: Vec::new(),
                    status: Status::Ok,
                    duration_ms: 900,
                    speculated: false,
                    sub_agent_id: None,
                },
                5_000,
                300,
            ),
        ],
        answer: Some("Scaffolded main.tex and built it once.".to_string()),
        notes: vec![Note {
            kind: NoteKind::Stage,
            summary: "execute".to_string(),
            detail: Vec::new(),
            before_step: 0,
            inspect: None,
        }],
        status: Status::Ok,
        duration_ms: 9_000,
    };

    let fix = Turn {
        name: "fix-overfull".to_string(),
        prompt: "fix the overfull hbox warnings in main.tex, then clean up the \
                 build artifacts so only the source and pdf remain"
            .to_string(),
        prose: vec![Prose {
            text: "I'll read the file, reproduce the warnings, then fix the typesetting. \
                   Overfull boxes here are almost certainly the long sentences hitting the \
                   margin — enabling microtype plus a small \\emergencystretch should absorb \
                   them without visible damage, and I'll rewrap the one sentence that is \
                   structurally too rigid."
                .to_string(),
            before_step: 0,
        }],
        notes: vec![
            Note {
                kind: NoteKind::Context,
                summary: "recalled 5 frames · 408 tok · 126ms · 5 episode".to_string(),
                detail: vec![
                    "symbol  fn find — stella-cli/src/config_wiring.rs · 34 tok".to_string(),
                    "symbol  fn find — stella-core/src/tasks.rs · 118 tok".to_string(),
                ],
                before_step: 0,
                inspect: None,
            },
            Note {
                kind: NoteKind::Verdict,
                summary: "witness flip confirmed — overfull_hbox_absent".to_string(),
                detail: Vec::new(),
                before_step: 6,
                inspect: None,
            },
        ],
        steps: vec![
            step(read, 4_000, 900),
            step(reproduce, 9_000, 800),
            step(edit, 17_000, 1_600),
            step(write, 22_000, 900),
            step(verify, 29_000, 800),
            step(delete, 35_000, 700),
        ],
        answer: Some(
            "All three overfull warnings are gone. I enabled microtype, added \
             \\emergencystretch=1em, tightened \\parindent to 12pt, and rewrapped the rigid \
             sentence on line 5. A rebuild reports zero warnings, and I removed main.aux — \
             the directory now holds only main.tex, .latexmkrc and main.pdf."
                .to_string(),
        ),
        status: Status::Ok,
        duration_ms: 41_000,
    };

    Run {
        name: "latex-proof".to_string(),
        model: "z-ai/glm-5.2".to_string(),
        started_at: "14:02:11".to_string(),
        turns: vec![setup, fix],
    }
}
