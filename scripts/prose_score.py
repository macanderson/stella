#!/usr/bin/env python3
"""
Prose scoring system for markdown files.

Scores each file on four dimensions (0-100):
  1. human_sound    — closeness to human-sounding prose
  2. grammar        — grammar correctness
  3. simple_lang    — simple language
  4. grade_8        — 8th-grader understandability (0 = no 8th grader could understand, 100 = every 8th grader in the US could understand)

Usage:
  python3 scripts/prose_score.py [path ...]
  python3 scripts/prose_score.py --all          # scan all project .md files
  python3 scripts/prose_score.py --all --fix    # also rewrite worst offenders
"""

import re
import sys
import os
import json
import textstat
from pathlib import Path
from dataclasses import dataclass, asdict
from typing import List, Tuple

# ---------------------------------------------------------------------------
# Data structures
# ---------------------------------------------------------------------------

@dataclass
class ProseScore:
    file: str
    human_sound: float
    grammar: float
    simple_lang: float
    grade_8: float
    word_count: int
    sentence_count: int
    avg_sentence_len: float
    flesch_reading_ease: float
    flesch_kincaid_grade: float
    smog_index: float
    issues: List[str]

    @property
    def overall(self) -> float:
        return (self.human_sound + self.grammar + self.simple_lang + self.grade_8) / 4


# ---------------------------------------------------------------------------
# Text extraction
# ---------------------------------------------------------------------------

def extract_prose(md_text: str) -> str:
    """Strip markdown syntax, code blocks, frontmatter, and HTML — keep only prose."""
    # Remove YAML frontmatter
    text = re.sub(r'^---\s*\n.*?\n---\s*\n', '', md_text, flags=re.DOTALL)
    # Remove code blocks
    text = re.sub(r'```[\s\S]*?```', '', text)
    text = re.sub(r'`[^`]+`', '', text)
    # Remove HTML tags
    text = re.sub(r'<[^>]+>', '', text)
    # Remove images and links (keep link text)
    text = re.sub(r'!\[([^\]]*)\]\([^)]+\)', r'\1', text)
    text = re.sub(r'\[([^\]]+)\]\([^)]+\)', r'\1', text)
    # Remove headers markers
    text = re.sub(r'^#{1,6}\s+', '', text, flags=re.MULTILINE)
    # Remove bold/italic markers
    text = re.sub(r'\*\*([^*]+)\*\*', r'\1', text)
    text = re.sub(r'\*([^*]+)\*', r'\1', text)
    text = re.sub(r'__([^_]+)__', r'\1', text)
    text = re.sub(r'_([^_]+)_', r'\1', text)
    # Remove horizontal rules
    text = re.sub(r'^[-*_]{3,}\s*$', '', text, flags=re.MULTILINE)
    # Remove list markers
    text = re.sub(r'^\s*[-*+]\s+', '', text, flags=re.MULTILINE)
    text = re.sub(r'^\s*\d+\.\s+', '', text, flags=re.MULTILINE)
    # Remove blockquote markers
    text = re.sub(r'^\s*>\s?', '', text, flags=re.MULTILINE)
    # Remove table pipes
    text = re.sub(r'\|', ' ', text)
    # Collapse whitespace
    text = re.sub(r'\n{3,}', '\n\n', text)
    return text.strip()


# ---------------------------------------------------------------------------
# Scoring helpers
# ---------------------------------------------------------------------------

def _clamp(v: float, lo: float = 0.0, hi: float = 100.0) -> float:
    return max(lo, min(hi, v))


def score_human_sound(text: str) -> Tuple[float, List[str]]:
    """Heuristics for human-sounding prose."""
    issues = []
    score = 100.0

    # Passive voice detection (simple heuristic)
    passive_patterns = [
        r'\b(?:is|are|was|were|be|been|being)\s+\w+ed\b',
        r'\b(?:is|are|was|were|be|been|being)\s+\w+en\b',
    ]
    passive_count = sum(len(re.findall(p, text, re.IGNORECASE)) for p in passive_patterns)
    sentences = re.split(r'[.!?]+', text)
    sentences = [s.strip() for s in sentences if s.strip()]
    if sentences:
        passive_ratio = passive_count / len(sentences)
        if passive_ratio > 0.3:
            penalty = min(30, (passive_ratio - 0.3) * 100)
            score -= penalty
            issues.append(f"High passive voice ({passive_ratio:.0%} of sentences)")

    # Nominalizations (words ending in -tion, -ment, -ness, -ity used as subjects)
    nominalizations = len(re.findall(r'\b\w+(?:tion|ment|ness|ity|ance|ence)\b', text, re.IGNORECASE))
    words = len(text.split())
    if words > 0:
        nom_ratio = nominalizations / words
        if nom_ratio > 0.05:
            penalty = min(20, (nom_ratio - 0.05) * 400)
            score -= penalty
            issues.append(f"Heavy nominalization ({nom_ratio:.1%} of words)")

    # Corporate / AI buzzwords
    buzzwords = [
        'leverage', 'synergy', 'paradigm', 'utilize', 'facilitate', 'implement',
        'optimize', 'streamline', 'robust', 'scalable', 'seamless', 'cutting-edge',
        'state-of-the-art', 'best-in-class', 'world-class', 'next-generation',
        'mission-critical', 'enterprise-grade', 'holistic', 'granular',
        'actionable', 'deliverable', 'stakeholder', 'bandwidth', 'circle back',
        'deep dive', 'move the needle', 'low-hanging fruit', 'think outside the box',
        'at the end of the day', 'it is what it is', 'synergize', 'incentivize',
        'productize', 'operationalize', 'monetize', 'ideate', 'impactful',
        'efforting', 'learnings', 'ask'  # as noun
    ]
    found_buzz = [b for b in buzzwords if re.search(r'\b' + re.escape(b) + r'\b', text, re.IGNORECASE)]
    if found_buzz:
        penalty = min(25, len(found_buzz) * 3)
        score -= penalty
        issues.append(f"Buzzwords: {', '.join(found_buzz[:5])}")

    # Excessive hedging
    hedges = ['it is important to note', 'it should be noted', 'it is worth mentioning',
              'it could be argued', 'one might say', 'to some extent', 'in a sense',
              'as it were', 'so to speak', 'if you will']
    found_hedges = [h for h in hedges if h in text.lower()]
    if found_hedges:
        penalty = min(15, len(found_hedges) * 5)
        score -= penalty
        issues.append(f"Hedging phrases: {', '.join(found_hedges[:3])}")

    # Sentence length variance (human writing has rhythm)
    if len(sentences) >= 3:
        lengths = [len(s.split()) for s in sentences]
        mean_len = sum(lengths) / len(lengths)
        variance = sum((l - mean_len) ** 2 for l in lengths) / len(lengths)
        std_dev = variance ** 0.5
        if std_dev < 3 and mean_len > 15:
            score -= 10
            issues.append("Monotonous sentence rhythm (all sentences similar length)")

    return _clamp(score), issues


def score_grammar(text: str) -> Tuple[float, List[str]]:
    """Heuristics for grammar correctness."""
    issues = []
    score = 100.0

    # Common grammar mistakes
    mistakes = [
        (r'\btheir\s+is\b', "their/there confusion"),
        (r'\bthere\s+are\s+\w+\s+is\b', "subject-verb disagreement"),
        (r'\bits\s+is\b', "its/it's confusion"),
        (r'\byou\s+was\b', "you was"),
        (r'\bhe\s+don\'t\b', "he don't"),
        (r'\bshe\s+don\'t\b', "she don't"),
        (r'\bit\s+don\'t\b', "it don't"),
        (r'\bdoes\s+not\s+exists\b', "does not exists"),
        (r'\bcan\s+not\b', "cannot (one word)"),
        (r'\balot\b', "a lot (two words)"),
        (r'\bdefinately\b', "definitely"),
        (r'\bseperate\b', "separate"),
        (r'\boccured\b', "occurred"),
        (r'\bneccessary\b', "necessary"),
        (r'\brecieve\b', "receive"),
        (r'\bacheive\b', "achieve"),
        (r'\bwich\b', "which"),
        (r'\bteh\b', "the"),
        (r'\bfrom\s+the\s+the\b', "double 'the'"),
        (r'\bto\s+the\s+the\b', "double 'the'"),
        (r'\bin\s+the\s+the\b', "double 'the'"),
    ]
    for pattern, desc in mistakes:
        if re.search(pattern, text, re.IGNORECASE):
            score -= 5
            issues.append(f"Grammar: {desc}")

    # Double spaces (often a sign of sloppy editing)
    if '  ' in text:
        score -= 2
        issues.append("Double spaces")

    # Missing terminal punctuation on sentences
    lines = [l.strip() for l in text.split('\n') if l.strip()]
    for line in lines:
        if len(line.split()) > 5 and not line.endswith(('.', '!', '?', ':', ';', '"', "'", ')', ']', '}')):
            score -= 1
            issues.append(f"Missing terminal punctuation: '{line[:50]}...'")
            break  # only flag once

    return _clamp(score), issues


def score_simple_lang(text: str) -> Tuple[float, List[str]]:
    """Score simple language (0-100)."""
    issues = []
    score = 100.0

    words = text.split()
    if not words:
        return 0.0, ["No prose found"]

    # Long words (3+ syllables)
    long_words = [w for w in words if textstat.syllable_count(w) >= 3]
    long_ratio = len(long_words) / len(words)
    if long_ratio > 0.15:
        penalty = min(30, (long_ratio - 0.15) * 200)
        score -= penalty
        issues.append(f"Too many long words ({long_ratio:.0%} have 3+ syllables)")

    # Jargon / technical terms (simple heuristic: words > 10 chars)
    jargon = [w for w in words if len(w) > 10 and w.isalpha()]
    jargon_ratio = len(jargon) / len(words)
    if jargon_ratio > 0.05:
        penalty = min(25, (jargon_ratio - 0.05) * 300)
        score -= penalty
        issues.append(f"Technical jargon ({jargon_ratio:.0%} of words)")

    # Acronyms
    acronyms = re.findall(r'\b[A-Z]{2,}\b', text)
    if len(acronyms) > 3:
        penalty = min(15, len(acronyms) * 2)
        score -= penalty
        issues.append(f"Many acronyms: {', '.join(acronyms[:5])}")

    # Complex sentence structures (multiple clauses)
    sentences = re.split(r'[.!?]+', text)
    sentences = [s.strip() for s in sentences if s.strip()]
    complex_count = 0
    for s in sentences:
        clauses = len(re.split(r'[,;:—–-]', s))
        if clauses >= 4:
            complex_count += 1
    if sentences:
        complex_ratio = complex_count / len(sentences)
        if complex_ratio > 0.2:
            penalty = min(20, (complex_ratio - 0.2) * 100)
            score -= penalty
            issues.append(f"Complex sentences ({complex_ratio:.0%} have 4+ clauses)")

    return _clamp(score), issues


def score_grade_8(text: str) -> Tuple[float, List[str]]:
    """
    8th-grader understandability: 0 = no 8th grader could understand,
    100 = every 8th grader in the US could understand.
    """
    issues = []

    if not text.strip():
        return 0.0, ["No prose found"]

    # Flesch Reading Ease: 90-100 = very easy (5th grade), 60-70 = standard (8th-9th grade)
    fre = textstat.flesch_reading_ease(text)
    # Map FRE to 0-100: FRE 0 -> 0, FRE 100 -> 100
    fre_score = _clamp(fre)

    # Flesch-Kincaid Grade Level: 8.0 is target
    fk_grade = textstat.flesch_kincaid_grade(text)
    # Map: grade 0 -> 100, grade 8 -> 80, grade 12 -> 60, grade 16+ -> 20
    if fk_grade <= 8:
        fk_score = 100 - (fk_grade * 2.5)  # grade 8 -> 80
    else:
        fk_score = 80 - ((fk_grade - 8) * 5)  # grade 12 -> 60, grade 16 -> 40
    fk_score = _clamp(fk_score)

    # SMOG Index: similar mapping
    smog = textstat.smog_index(text)
    if smog <= 8:
        smog_score = 100 - (smog * 2.5)
    else:
        smog_score = 80 - ((smog - 8) * 5)
    smog_score = _clamp(smog_score)

    # Average sentence length (8th graders handle ~15-20 words)
    sentences = re.split(r'[.!?]+', text)
    sentences = [s.strip() for s in sentences if s.strip()]
    if sentences:
        avg_len = sum(len(s.split()) for s in sentences) / len(sentences)
        if avg_len <= 15:
            len_score = 100
        elif avg_len <= 20:
            len_score = 80
        elif avg_len <= 25:
            len_score = 60
        else:
            len_score = max(0, 60 - (avg_len - 25) * 2)
    else:
        avg_len = 0
        len_score = 0

    # Composite: weight FRE most, then FK, then SMOG, then sentence length
    composite = (fre_score * 0.35 + fk_score * 0.30 + smog_score * 0.20 + len_score * 0.15)

    if fre < 50:
        issues.append(f"Flesch Reading Ease {fre:.0f} (target: 60+)")
    if fk_grade > 9:
        issues.append(f"Flesch-Kincaid grade {fk_grade:.1f} (target: 8.0)")
    if smog > 9:
        issues.append(f"SMOG index {smog:.1f} (target: 8.0)")
    if avg_len > 20:
        issues.append(f"Avg sentence length {avg_len:.1f} words (target: 15-20)")

    return _clamp(composite), issues


# ---------------------------------------------------------------------------
# Main scoring
# ---------------------------------------------------------------------------

def score_file(path: Path) -> ProseScore:
    raw = path.read_text(encoding='utf-8', errors='replace')
    prose = extract_prose(raw)

    words = prose.split()
    sentences = re.split(r'[.!?]+', prose)
    sentences = [s.strip() for s in sentences if s.strip()]
    avg_len = sum(len(s.split()) for s in sentences) / len(sentences) if sentences else 0

    fre = textstat.flesch_reading_ease(prose) if prose.strip() else 0
    fk = textstat.flesch_kincaid_grade(prose) if prose.strip() else 0
    smog = textstat.smog_index(prose) if prose.strip() else 0

    human, human_issues = score_human_sound(prose)
    grammar, grammar_issues = score_grammar(prose)
    simple, simple_issues = score_simple_lang(prose)
    g8, g8_issues = score_grade_8(prose)

    return ProseScore(
        file=str(path),
        human_sound=round(human, 1),
        grammar=round(grammar, 1),
        simple_lang=round(simple, 1),
        grade_8=round(g8, 1),
        word_count=len(words),
        sentence_count=len(sentences),
        avg_sentence_len=round(avg_len, 1),
        flesch_reading_ease=round(fre, 1),
        flesch_kincaid_grade=round(fk, 1),
        smog_index=round(smog, 1),
        issues=human_issues + grammar_issues + simple_issues + g8_issues,
    )


def find_project_markdown(root: Path) -> List[Path]:
    """Find all .md files in the main repo, excluding worktrees and vendored deps."""
    def excluded(p: Path) -> bool:
        parts = p.parts
        for i, part in enumerate(parts):
            if part in {'.git', 'node_modules', 'target', 'site-packages'}:
                return True
            if part == '.venv' or part.startswith('.venv'):
                return True
            # exclude .claude/worktrees/* and .stella/worktrees/*
            if part == 'worktrees' and i > 0 and parts[i - 1] in {'.claude', '.stella'}:
                return True
        return False
    return sorted(p for p in root.rglob('*.md') if not excluded(p))


def main():
    import argparse
    parser = argparse.ArgumentParser(description='Score markdown prose quality')
    parser.add_argument('paths', nargs='*', help='Files or directories to score')
    parser.add_argument('--all', action='store_true', help='Score all project markdown')
    parser.add_argument('--json', action='store_true', help='Output JSON')
    parser.add_argument('--threshold', type=float, default=70, help='Flag files below this overall score')
    args = parser.parse_args()

    root = Path('/Users/macanderson/Projects/stella')

    if args.all:
        files = find_project_markdown(root)
    elif args.paths:
        files = []
        for p in args.paths:
            pp = Path(p)
            if pp.is_dir():
                files.extend(pp.rglob('*.md'))
            else:
                files.append(pp)
    else:
        parser.print_help()
        sys.exit(1)

    scores = []
    for f in files:
        try:
            s = score_file(f)
            scores.append(s)
        except Exception as e:
            print(f"Error scoring {f}: {e}", file=sys.stderr)

    # Sort by overall score (worst first)
    scores.sort(key=lambda s: s.overall)

    if args.json:
        print(json.dumps([asdict(s) for s in scores], indent=2))
    else:
        print(f"\n{'='*100}")
        print(f"PROSE SCORE REPORT — {len(scores)} files")
        print(f"{'='*100}\n")

        flagged = [s for s in scores if s.overall < args.threshold]
        print(f"Files below threshold ({args.threshold}): {len(flagged)}\n")

        for s in scores:
            rel = os.path.relpath(s.file, root)
            flag = "⚠️ " if s.overall < args.threshold else "  "
            print(f"{flag} {s.overall:5.1f}  {rel}")
            print(f"       human={s.human_sound:5.1f}  grammar={s.grammar:5.1f}  simple={s.simple_lang:5.1f}  grade8={s.grade_8:5.1f}")
            print(f"       words={s.word_count:4d}  sentences={s.sentence_count:3d}  avg_len={s.avg_sentence_len:4.1f}  FRE={s.flesch_reading_ease:5.1f}  FK={s.flesch_kincaid_grade:4.1f}  SMOG={s.smog_index:4.1f}")
            if s.issues:
                for issue in s.issues[:3]:
                    print(f"       → {issue}")
                if len(s.issues) > 3:
                    print(f"       → ... and {len(s.issues)-3} more")
            print()

        # Summary stats
        if scores:
            avg = sum(s.overall for s in scores) / len(scores)
            print(f"{'='*100}")
            print(f"Average overall score: {avg:.1f}")
            print(f"Best:  {scores[-1].overall:.1f}  {os.path.relpath(scores[-1].file, root)}")
            print(f"Worst: {scores[0].overall:.1f}  {os.path.relpath(scores[0].file, root)}")


if __name__ == '__main__':
    main()
