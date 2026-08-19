//! The chunker's contract: every character survives, and the seams land where
//! the document declares them.

use super::*;

/// Rebuild the document from its chunks. The property every other test leans
/// on: a slicer that loses a character is the defect #2407 filed, wearing a
/// different shape.
fn rejoined(chunks: &[Chunk]) -> String {
    chunks.iter().map(|c| c.text.as_str()).collect()
}

fn doc_with_sections(count: usize, body_chars: usize) -> String {
    (0..count)
        .map(|i| format!("## Section {i}\n\n{}\n\n", "x".repeat(body_chars)))
        .collect()
}

/// The witness for #2407: a document far past any single-call cap is covered
/// whole, not to the cap and no further.
///
/// Fails on the old code, where `bounded_content` returned the first 120,000
/// characters and dropped the rest before the model was called.
#[test]
fn a_document_far_past_one_call_is_covered_completely() {
    // 40 sections of 6,000 characters — 240,000 characters, twice the old
    // single-call cap and ten times the cap that motivated the issue.
    let doc = doc_with_sections(40, 6_000);
    assert!(
        doc.chars().count() > 200_000,
        "fixture must exceed the old cap"
    );

    let chunks = split(&doc);

    assert!(chunks.len() > 1, "a document this size must divide");
    assert_eq!(rejoined(&chunks), doc, "every character must survive");
    assert_eq!(
        dropped_chars(&doc),
        0,
        "nothing is dropped below the document ceiling"
    );
}

/// The specific document the issue is about. `AGENTS.md` at its filed size was
/// extracted at 54%; every character of it now reaches a call.
#[test]
fn an_agents_md_sized_document_loses_nothing() {
    let doc = doc_with_sections(8, 5_500); // ~44,500 characters
    let chunks = split(&doc);

    assert_eq!(rejoined(&chunks), doc);
    let covered: usize = chunks.iter().map(|c| c.text.chars().count()).sum();
    assert_eq!(covered, doc.chars().count(), "100% of the document is read");
}

/// Slices are what one model call can answer, so none may exceed the target
/// unless a single unbreakable line does.
#[test]
fn no_chunk_exceeds_the_call_target() {
    let doc = doc_with_sections(30, 7_000);
    for chunk in split(&doc) {
        assert!(
            chunk.text.chars().count() <= TARGET_CHARS,
            "chunk of {} characters exceeds the {TARGET_CHARS} target",
            chunk.text.chars().count()
        );
    }
}

/// Short sections pack together rather than buying one paid call each.
#[test]
fn many_short_sections_share_a_call() {
    let doc = doc_with_sections(50, 100);
    let chunks = split(&doc);
    assert_eq!(
        chunks.len(),
        1,
        "50 short sections fit one call: {chunks:?}"
    );
}

/// The seam is the heading, not an offset. Cutting AGENTS.md at a character
/// offset severed a table row mid-cell; a chunk must start where a section does.
#[test]
fn chunks_begin_at_a_heading() {
    let doc = doc_with_sections(6, 5_000);
    let chunks = split(&doc);
    assert!(chunks.len() > 1, "fixture must divide");
    for chunk in &chunks {
        assert!(
            chunk.text.starts_with("## Section"),
            "chunk did not begin at a heading: {:?}",
            &chunk.text[..40.min(chunk.text.len())]
        );
    }
}

/// A `#` inside a fenced code block is a shell comment, not a heading. Splitting
/// there leaves an unterminated fence in both halves.
#[test]
fn a_comment_inside_a_fence_is_not_a_seam() {
    let doc = "\
## Real heading

```sh
# not a heading
make gate
```

more text
";
    let secs = sections(doc);
    assert_eq!(secs.len(), 1, "the fence must not open a section: {secs:?}");
    assert_eq!(secs[0].path, vec!["Real heading".to_string()]);
}

/// A tilde fence closes only on tildes, and a longer backtick fence only on at
/// least as many backticks — otherwise a nested fence ends the outer one.
#[test]
fn fences_close_on_their_own_marker_and_width() {
    let doc = "\
# Top

~~~
# inside tildes
~~~

````
```
# inside nested backticks
```
````

## Second
";
    let secs = sections(doc);
    let paths: Vec<_> = secs.iter().map(|s| s.path.clone()).collect();
    assert_eq!(
        paths,
        vec![
            vec!["Top".to_string()],
            // Nested, because `## Second` sits under `# Top` — the point here is
            // that only two sections exist at all, the fenced `#` lines having
            // opened none.
            vec!["Top".to_string(), "Second".to_string()],
        ],
        "only the two real headings open sections"
    );
}

/// A slice that continues an oversized section carries the heading path, so the
/// extractor is not reading an unlabelled fragment.
#[test]
fn a_continued_section_carries_its_heading_path() {
    let doc = format!(
        "# Guide\n\n## Deep\n\n{}\n",
        (0..40)
            .map(|i| format!("paragraph {i} {}\n\n", "y".repeat(1_000)))
            .collect::<String>()
    );
    let chunks = split(&doc);
    assert!(chunks.len() > 1, "fixture must divide inside one section");
    for chunk in chunks.iter().skip(1) {
        assert_eq!(
            chunk.path,
            vec!["Guide".to_string(), "Deep".to_string()],
            "a continuation must know where it is"
        );
    }
    assert_eq!(rejoined(&chunks), doc);
}

/// Inside an oversized section the cut prefers a blank line: a paragraph is the
/// next unit the document declares after a heading.
#[test]
fn an_oversized_section_cuts_at_a_paragraph() {
    let body: String = (0..40)
        .map(|i| format!("Paragraph {i}. {}\n\n", "z".repeat(1_000)))
        .collect();
    let pieces = divide(&body);
    assert!(pieces.len() > 1, "fixture must divide");
    assert_eq!(pieces.concat(), body);
    for piece in pieces.iter().skip(1) {
        assert!(
            piece.starts_with("Paragraph"),
            "piece began mid-paragraph: {:?}",
            &piece[..30.min(piece.len())]
        );
    }
}

/// A generated file with no blank line and no short lines still divides — there
/// is no boundary left to respect, and losing the tail is not an option.
#[test]
fn one_unbreakable_line_still_divides_without_loss() {
    let doc = format!("# T\n\n{}\n", "q".repeat(TARGET_CHARS * 3 + 17));
    let chunks = split(&doc);
    assert!(chunks.len() >= 3, "must divide: {} chunks", chunks.len());
    assert_eq!(rejoined(&chunks), doc, "no character is lost");
}

/// Text before the first heading is a section of its own, not a dropped prefix.
#[test]
fn a_preamble_before_the_first_heading_is_kept() {
    let doc = "Some preamble.\n\n# First\n\nBody.\n";
    let chunks = split(doc);
    assert_eq!(rejoined(&chunks), doc);
    assert!(chunks[0].text.starts_with("Some preamble."));
    assert!(
        chunks[0].path.is_empty(),
        "a preamble sits under no heading"
    );
}

/// A heading that skips a level must not reparent the ones under it.
#[test]
fn a_skipped_heading_level_does_not_reparent() {
    let doc = "# One\n\n### Three\n\nbody\n";
    let secs = sections(doc);
    let last = secs.last().expect("a section");
    assert_eq!(last.path, vec!["One".to_string(), "Three".to_string()]);
}

#[test]
fn an_empty_document_asks_for_no_calls() {
    assert!(split("").is_empty());
    assert!(split("   \n\n  \n").is_empty());
}

/// The document ceiling exists, is reported, and cuts on a line boundary.
#[test]
fn the_document_ceiling_truncates_on_a_line_and_reports_the_loss() {
    let doc = format!(
        "{}\n",
        "line of text\n".repeat(MAX_DOCUMENT_CHARS / 13 + 200)
    );
    let (kept, dropped) = bound(&doc);
    assert!(dropped > 0, "fixture must exceed the ceiling");
    assert!(kept.chars().count() <= MAX_DOCUMENT_CHARS);
    assert!(kept.ends_with("line of text"), "cut on a line boundary");
    assert_eq!(
        kept.chars().count() + dropped,
        doc.chars().count(),
        "the reported loss accounts for every character"
    );
}

// The ceiling being at least 6x the cap it replaces is asserted at compile time
// in the parent module (`const _: () = assert!(…)`), not here: both sides are
// constants, so a test would only be checking the optimizer's arithmetic.

/// Below the ceiling nothing is dropped, however large — the property that makes
/// "ingest never silently loses a document" checkable.
#[test]
fn nothing_is_dropped_below_the_ceiling() {
    let doc = "a\n".repeat(MAX_DOCUMENT_CHARS / 2);
    assert_eq!(dropped_chars(&doc), 0);
    assert_eq!(rejoined(&split(&doc)), doc);
}

/// Multi-byte text is counted in characters, not bytes, at every boundary.
#[test]
fn multibyte_text_slices_on_character_boundaries() {
    let doc = format!("# Título\n\n{}\n", "é".repeat(TARGET_CHARS * 2));
    let chunks = split(&doc);
    assert_eq!(rejoined(&chunks), doc);
    for chunk in &chunks {
        assert!(chunk.text.chars().count() <= TARGET_CHARS);
    }
}
