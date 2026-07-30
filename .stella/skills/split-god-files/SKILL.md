---
name: split-god-files
description: Use when a file exceeds ~1500 lines of code (or feels bloated/multi-purpose) to break it apart into smaller, cohesive files without changing behavior.
domains: refactoring, code-quality, architecture
---

# Split God Files

Break oversized "god files" into smaller files, each under **1500 lines of code** (excluding blank lines/comments), organized by responsibility rather than arbitrary line cuts.

## When to trigger

- Any file (source, config, test) exceeds ~1500 LOC.
- A file mixes unrelated concerns (e.g., models + routes + utils + constants all in one file).
- User asks to "clean up", "refactor", or "split" a large file.

Always check line count first: `wc -l <file>`. If under 1500, do not split just for the sake of it — the trigger is size + mixed responsibility, not size alone.

## Procedure

1. **Audit before touching anything.**
   - Count lines: `wc -l <file>`.
   - Map the file's contents: list top-level classes/functions/exports and their line ranges (grep for `^class `, `^def `, `^function `, `^export `, etc.).
   - Identify natural seams: distinct responsibilities, layers (data/logic/UI), or groups of related exports.

2. **Design the split before editing.**
   - Propose a target file layout (new filenames + what moves where) and a short rationale per file.
   - Prefer splitting along responsibility boundaries (e.g., `types.ts`, `validators.ts`, `handlers.ts`, `constants.ts`) over mechanical "first half / second half" cuts.
   - Each resulting file should be independently understandable and under 1500 LOC. If a single logical unit (e.g., one class) alone exceeds 1500 LOC, split it internally by method groups/mixins/submodules rather than leaving it whole.
   - Keep the directory structure consistent with existing project conventions (co-locate related files, mirror existing naming patterns).

3. **Extract incrementally, not in one giant rewrite.**
   - Move one cohesive unit at a time (e.g., one class, one group of related functions).
   - After each move: update imports/exports in the new file, update the original file to import from it, and re-check nothing else references old internal paths.
   - Preserve public API: anything previously importable from the original file should still be importable from the same path (re-export from an index/barrel file if needed) unless the user explicitly wants call sites updated too.

4. **Verify after every extraction.**
   - Run existing tests/build/lint after each move, not just at the end.
   - Grep the codebase for imports of the original file to confirm nothing breaks.
   - Confirm no logic changed — this is a pure structural refactor unless told otherwise.

5. **Final check.**
   - Re-run `wc -l` on every resulting file; confirm all are ≤1500 LOC.
   - Summarize the new file layout and what moved where.
   - Flag any file still near/over the limit and explain why (e.g., an irreducible single class) with a follow-up suggestion.

## Guardrails

- Never change behavior, rename public symbols, or "improve" logic while splitting — that's a separate task.
- Don't split purely by line count if it breaks a cohesive unit (e.g., cutting a class in half arbitrarily). Split by responsibility; size is the trigger, not the cutting rule.
- Don't create circular imports between the new files — if two new files need each other, extract the shared piece into a third file.
- If the file is generated code, a data file, or a single indivisible algorithm, say so instead of