//! Lexical mining helpers shared by the rules and skills miners
//! ([`crate::rules`], [`crate::skills`]). Both cluster free-text
//! observations, pick a stable representative wording, and mint
//! deterministic `<slug>-<hash8>` ids — one home so a stopword, similarity,
//! or slug tweak can never land in one miner and silently miss the other.
//! (These started as two byte-identical private copies; the divergence risk
//! is why they were merged.)

use std::collections::{HashMap, HashSet};

const STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "to", "of", "in", "on", "for", "with", "is", "are", "be",
    "this", "that", "it", "as", "at", "by", "from", "into", "we", "you", "i", "was", "were", "has",
    "have", "had", "not", "but", "if", "so", "then", "than", "when", "where", "which", "will",
    "would", "should", "did",
];

/// Terms dropped by [`score_terms`] ONLY — the low-selectivity vocabulary
/// that appears in nearly every prompt a coding agent ever sees and therefore
/// discriminates between no two skills: the generic coding words, the English
/// function words that survive [`STOPWORDS`], and the two words this corpus
/// puts in every skill description it has (`user`, `agent`).
///
/// Deliberately **not** added to [`STOPWORDS`], because that constant is also
/// consulted by [`terms`], which is the **id space**: every mined lesson's
/// `<slug>-<hash8>` is derived from it, so widening it there would silently
/// invalidate every stored id (see [`terms`]'s own doc comment, which names
/// that migration as deliberate rather than drive-by). Scoring is ephemeral
/// and carries no such contract, so the two vocabularies are allowed to differ
/// and this is the one that may grow.
///
/// Born from a real misfire (#3688): in session `ses-1787071651715-48158` the
/// prompt "find a problem with stellas turn loop code" selected the skill
/// `source-command-remove-deadcode` on `terms: code, find` — two words that
/// carry no information about the task, clearing
/// `SelectionConfig`'s two-shared-term corroboration floor on their own and
/// injecting a dead-code-removal skill into a turn-loop debugging turn. The
/// floor was never the defect; what it counted was.
///
/// The function words joined it the same way (#4384). [`STOPWORDS`] was
/// written for the miners' lesson text and stops at 43 entries, so `they`,
/// `its`, `itself`, `such` and their neighbours reached the score at full
/// strength — and cleared the same corroboration floor in pairs. Session
/// `ses-1787465453163-60967` recorded the selections verbatim: a TUI
/// keybinding turn drew `source-command-update-docs` on `terms: source, such`
/// and `find-skills` on `terms: agent, they`, and a PR-watcher turn drew
/// `ultra-audit` on `terms: its, itself, number, user`.
///
/// A hand list is the cheap answer, not the durable one: what makes `user`
/// and `agent` noise is that *every* installed skill's description carries
/// them, which is a property of the corpus and would be measured by IDF over
/// it rather than declared here. That is its own change — this one stops the
/// misfires the trace shows.
const SCORING_STOPWORDS: &[&str] = &[
    "code", "find", "fix", "add", "make", "use", "used", "using", "get", "set", "run", "check",
    "problem", "issue", "bug", "error", "test", "tests", "file", "files", "function", "method",
    "line", "lines", "change", "update", "new", "old", "work", "works", "need", "want", "help",
    "please", "can", "could", "how", "what", "why", "does", "any", "all", "some", "more", "most",
    "just", "also", "one", "two", "there", "here", "out", "off", "way", "thing", "things", "about",
    "like", "look", "see", "show", "tell", "give", "take", "know", "think", "they", "them",
    "their", "theirs", "its", "itself", "she", "her", "hers", "herself", "him", "his", "himself",
    "our", "ours", "your", "yours", "yourself", "such", "these", "those", "each", "every",
    "another", "other", "others", "both", "same", "who", "whom", "whose", "been", "being",
    "having", "very", "quite", "rather", "still", "even", "user", "agent",
];

/// Split text into lowercased, de-stopped terms (>2 chars) for lexical
/// scoring/clustering (TS: `terms`). Stopword checks scan the 43-item
/// static slice directly — this runs inside the clustering loops, and a
/// per-call `HashSet` rebuild cost more than the linear scans it saved.
///
/// Word characters are **ASCII** alphanumerics plus `_`; everything else is a
/// boundary. That is deliberate for the English-shaped lesson text the miners
/// see, but it does mean an accented or non-Latin observation tokenizes to
/// nothing and therefore never clusters. Widening this would change every
/// mined `<slug>-<hash8>` id, so it is a deliberate migration, not a
/// drive-by tweak. This is the **id space**; ephemeral relevance scoring runs
/// in [`score_terms`]'s Unicode-aware space instead (#3298).
pub(crate) fn terms(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in text.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            current.push(ch);
        } else if !current.is_empty() {
            if current.len() > 2 && !STOPWORDS.contains(&current.as_str()) {
                out.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }
    if current.len() > 2 && !STOPWORDS.contains(&current.as_str()) {
        out.push(current);
    }
    out
}

/// `true` for a character of an unsegmented CJK script (Han ideographs,
/// hiragana, katakana) — scripts that put no spaces between words, so a
/// word-accumulating tokenizer would collapse a whole sentence into one
/// exact-match-only term. [`score_terms`] emits character bigrams for runs of
/// these instead. Hangul is deliberately absent: Korean is space-separated,
/// so the ordinary word path handles it.
fn is_unsegmented_cjk(ch: char) -> bool {
    matches!(u32::from(ch),
        0x3040..=0x30FF          // hiragana + katakana
        | 0x31F0..=0x31FF        // katakana phonetic extensions
        | 0x3400..=0x4DBF        // CJK unified ideographs extension A
        | 0x4E00..=0x9FFF        // CJK unified ideographs
        | 0xF900..=0xFAFF        // CJK compatibility ideographs
        | 0x20000..=0x2EBEF     // CJK unified ideographs extensions B–F
    )
}

/// Split text into lowercased, de-stopped terms for **ephemeral relevance
/// scoring only** — skill selection (`crate::skills::select_skills`). Scores
/// are recomputed from scratch every turn and mint no ids, so this tokenizer
/// can be Unicode-aware without the migration [`terms`]'s doc warns about.
///
/// **Two token spaces, on purpose (#3298).** [`terms`] is the *id space*: it
/// feeds clustering, dedup, and the minted `<slug>-<hash8>` ids, so widening
/// it re-tokenizes every stored artifact's provenance. This function is the
/// *scoring space*: nothing durable derives from it. The module history (two
/// byte-identical private copies that risked drifting) is why this split is
/// named so loudly — these two are **not** copies of each other, and
/// reconciling them means the id migration, never editing one to match the
/// other.
///
/// The split has **two** axes now, not one. #3298 gave the scoring space a
/// wider *alphabet* (Unicode, where the id space stays ASCII); #3688 gives it
/// a narrower *vocabulary* — [`SCORING_STOPWORDS`] on top of [`STOPWORDS`], so
/// the generic coding words that discriminate between no two skills stop
/// counting as matches. Both axes point the same way: the scoring space is
/// free to change because nothing durable derives from it.
///
/// Shape: space-separated scripts (Latin with diacritics, Cyrillic, Greek,
/// Hangul, …) accumulate word characters (`char::is_alphanumeric` plus `_`)
/// into words kept when longer than two **characters** (not bytes) and not
/// stopwords. Runs of unsegmented CJK have no spaces to split on, so they are
/// emitted as character bigrams — the standard cheap approximation for CJK
/// retrieval — with a one-character run kept as-is (each ideograph carries
/// word-level meaning, so the >2 gate does not apply). Other unsegmented
/// scripts (Thai, Khmer, …) fall through the word path and become one long
/// term: exact-phrase matching only, which is still strictly more than the
/// zero terms the ASCII tokenizer yields for them.
pub(crate) fn score_terms(text: &str) -> Vec<String> {
    fn flush_word(word: &mut String, out: &mut Vec<String>) {
        if word.chars().count() > 2
            && !STOPWORDS.contains(&word.as_str())
            && !SCORING_STOPWORDS.contains(&word.as_str())
        {
            out.push(std::mem::take(word));
        } else {
            word.clear();
        }
    }
    fn flush_cjk(run: &mut Vec<char>, out: &mut Vec<String>) {
        match run.len() {
            0 => {}
            1 => out.push(run[0].to_string()),
            _ => out.extend(run.windows(2).map(|pair| pair.iter().collect::<String>())),
        }
        run.clear();
    }

    let mut out = Vec::new();
    let mut word = String::new();
    let mut run: Vec<char> = Vec::new();
    for ch in text.to_lowercase().chars() {
        if is_unsegmented_cjk(ch) {
            flush_word(&mut word, &mut out);
            run.push(ch);
        } else if ch.is_alphanumeric() || ch == '_' {
            flush_cjk(&mut run, &mut out);
            word.push(ch);
        } else {
            flush_word(&mut word, &mut out);
            flush_cjk(&mut run, &mut out);
        }
    }
    flush_word(&mut word, &mut out);
    flush_cjk(&mut run, &mut out);
    out
}

/// The fraction of `b`'s vocabulary that `a` covers — `|a ∩ b| / |b|`, 0 when
/// either is empty.
///
/// Asymmetric on purpose, and that is the whole point. [`jaccard`] divides by
/// the UNION, so a long `a` is penalized for every term it holds that `b` does
/// not: a 15-term skill description scored against a 50-term prompt needs 5
/// shared terms to clear 0.08, and against a 200-term prompt it needs about
/// 16. Prompts are long and descriptions are short, so the symmetric measure
/// answered "are these two texts the same size and about the same thing?" when
/// the question selection actually asks is "does this prompt cover what this
/// skill is for?".
///
/// Deliberately a second function rather than a change to [`jaccard`]: the
/// miners' near-duplicate clustering thresholds (`min_similarity`) are tuned
/// against the symmetric measure, and re-pointing them at this one would
/// silently re-cluster every observation.
pub(crate) fn coverage(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    a.intersection(b).count() as f64 / b.len() as f64
}

/// Jaccard similarity of two term sets — 0 when either is empty (TS:
/// `jaccard`).
pub(crate) fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(b).count();
    let union = a.len() + b.len() - intersection;
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

/// Filesystem/id-safe slug: lowercase, alnum + dashes, capped short.
/// `fallback` names the artifact kind when the text slugs to nothing
/// (`"lesson"` for rules, `"skill"` for skills) (TS: `slugify`).
pub(crate) fn slugify(text: &str, fallback: &str) -> String {
    let mut collapsed = String::new();
    let mut prev_dash = false;
    for ch in text.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            collapsed.push(ch);
            prev_dash = false;
        } else if !prev_dash {
            collapsed.push('-');
            prev_dash = true;
        }
    }
    let trimmed = collapsed.trim_matches('-');
    let truncated: String = trimmed.chars().take(40).collect();
    let truncated = truncated.trim_end_matches('-');
    if truncated.is_empty() {
        fallback.to_string()
    } else {
        truncated.to_string()
    }
}

/// Short deterministic content hash (FNV-1a 64-bit, lower 32 bits as 8 hex
/// chars) so re-mining the same data yields the same candidate id (TS:
/// `hash8`, backed by `sha1`). Not cryptographic — this is purely a stable
/// id, not an integrity check, and no hash crate is a workspace dependency;
/// FNV-1a is dependency-free and deterministic, which is all this needs.
pub(crate) fn hash8(text: &str) -> String {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{:08x}", (hash & 0xffff_ffff) as u32)
}

/// The cluster's most representative wording: the most-repeated exact
/// text, longest wins ties. `None` only for an empty cluster, which the
/// miners never construct. `text` projects each observation's wording (TS:
/// `representativeText`).
pub(crate) fn representative_text<T>(cluster: &[T], text: impl Fn(&T) -> &str) -> Option<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for o in cluster {
        *counts.entry(text(o)).or_insert(0) += 1;
    }
    let mut best = text(cluster.first()?);
    let mut best_count = 0usize;
    for (candidate, count) in &counts {
        // Most-frequent, then longest, then lexically smallest. The final
        // lexical tiebreak is load-bearing: without it, two equal-count,
        // equal-length texts resolve in HashMap iteration order (randomized
        // per process), so re-mining the same observations could pick a
        // different representative and mint a duplicate `<slug>-<hash>` file.
        let better = *count > best_count
            || (*count == best_count && candidate.len() > best.len())
            || (*count == best_count && candidate.len() == best.len() && *candidate < best);
        if better {
            best = candidate;
            best_count = *count;
        }
    }
    Some(best.to_string())
}

/// `true` when any existing artifact already says essentially the same
/// thing — the candidate's terms overlap one of the `haystacks` past
/// `min_similarity`. Each miner builds its own haystack (a rule's text —
/// borrowed `&str`s, no clone; a skill's composed name+description+body —
/// owned `String`s): anything `AsRef<str>` works (TS: `alreadyCaptured`).
pub(crate) fn already_captured<S: AsRef<str>>(
    text: &str,
    haystacks: impl IntoIterator<Item = S>,
    min_similarity: f64,
) -> bool {
    let t: HashSet<String> = terms(text).into_iter().collect();
    haystacks.into_iter().any(|haystack| {
        let ht: HashSet<String> = terms(haystack.as_ref()).into_iter().collect();
        jaccard(&t, &ht) >= min_similarity
    })
}

/// Greedy single-pass clustering: each observation joins the first cluster
/// whose *first* observation's term set overlaps enough with it, else
/// starts a new one. `O(n × clusters)` set comparisons — fine at CLI-local
/// data volumes (TS: the clustering loop inside `mineCandidates`).
///
/// Each cluster head's term set is computed once, when the cluster is
/// created, and carried alongside it: the head never changes, so
/// re-tokenizing it for every incoming observation was pure waste.
pub(crate) fn cluster_observations<T>(
    observations: Vec<T>,
    min_similarity: f64,
    text: impl Fn(&T) -> &str,
) -> Vec<Vec<T>> {
    let mut clusters: Vec<(HashSet<String>, Vec<T>)> = Vec::new();
    for obs in observations {
        let obs_terms: HashSet<String> = terms(text(&obs)).into_iter().collect();
        let home = clusters
            .iter()
            .position(|(head_terms, _)| jaccard(&obs_terms, head_terms) >= min_similarity);
        match home {
            Some(idx) => clusters[idx].1.push(obs),
            None => clusters.push((obs_terms, vec![obs])),
        }
    }
    clusters.into_iter().map(|(_, members)| members).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The id space stays ASCII-only until the #3298 migration: this behavior
    /// is bad but stable, and every minted `<slug>-<hash8>` id depends on it.
    /// If this test breaks, an id migration is in flight — reconcile
    /// `migration_contract` too.
    #[test]
    fn id_space_terms_is_still_ascii_only() {
        // A non-ASCII char is only a boundary, so an accented word degrades
        // to its ASCII fragments…
        assert_eq!(terms("café serveur"), vec!["caf", "serveur"]);
        // …and fully non-Latin text tokenizes to nothing at all.
        assert!(terms("форматировать запросы").is_empty());
        assert!(terms("数据库查询格式化").is_empty());
        assert_eq!(
            terms("format sql queries"),
            vec!["format", "sql", "queries"]
        );
    }

    /// The two spaces still agree on *tokenization* for ASCII text — the
    /// property this has always pinned. The sample deliberately carries no
    /// [`SCORING_STOPWORDS`] entry, so what it compares is the split, not the
    /// vocabulary; the vocabulary divergence is pinned by
    /// `score_terms_drops_generic_coding_words_that_the_id_space_keeps` below.
    #[test]
    fn score_terms_matches_terms_on_ascii() {
        let text = "format the sql_query in main.rs";
        assert_eq!(score_terms(text), terms(text));
    }

    /// #3688: generic coding vocabulary is dropped from the *scoring* space
    /// and kept in the *id* space — dropping it from `terms` would re-tokenize
    /// every minted `<slug>-<hash8>`.
    #[test]
    fn score_terms_drops_generic_coding_words_that_the_id_space_keeps() {
        let text = "find a problem with stellas turn loop code";
        assert_eq!(score_terms(text), vec!["stellas", "turn", "loop"]);
        // The id space is untouched: it still keeps every one of them.
        let id_space = terms(text);
        for generic in ["find", "problem", "code"] {
            assert!(
                id_space.contains(&generic.to_string()),
                "terms() is the id space and must not change: {id_space:?}"
            );
        }
    }

    /// The id space is a stored-artifact contract, so pin it directly rather
    /// than only as the negative half of the test above: if this breaks, every
    /// mined `<slug>-<hash8>` id has been silently re-tokenized.
    #[test]
    fn scoring_stopwords_do_not_reach_the_id_space() {
        for word in SCORING_STOPWORDS {
            assert!(
                !STOPWORDS.contains(word),
                "{word} belongs to exactly one vocabulary, never both"
            );
            if word.chars().count() > 2 {
                assert_eq!(
                    terms(word),
                    vec![word.to_string()],
                    "terms() must still keep {word}"
                );
            }
        }
    }

    #[test]
    fn score_terms_keeps_accented_latin_cyrillic_and_greek_words() {
        assert_eq!(score_terms("Café serveur"), vec!["café", "serveur"]);
        assert_eq!(
            score_terms("форматировать запросы SQL"),
            vec!["форматировать", "запросы", "sql"]
        );
        assert_eq!(score_terms("βάση δεδομένων"), vec!["βάση", "δεδομένων"]);
    }

    /// The length gate counts characters, not bytes: a two-character Cyrillic
    /// token is 4 bytes and must still be dropped like a two-letter ASCII one.
    #[test]
    fn score_terms_length_gate_counts_chars_not_bytes() {
        assert!(score_terms("до").is_empty());
    }

    #[test]
    fn score_terms_emits_cjk_character_bigrams() {
        assert_eq!(
            score_terms("数据库查询"),
            vec!["数据", "据库", "库查", "查询"]
        );
        // A lone ideograph is a word on its own — kept, not length-gated.
        assert_eq!(score_terms("查"), vec!["查"]);
    }

    #[test]
    fn score_terms_splits_mixed_cjk_and_latin_runs() {
        assert_eq!(
            score_terms("格式化 SQL 查询"),
            vec!["格式", "式化", "sql", "查询"]
        );
    }
}
