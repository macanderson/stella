//! Ranking indexed files by NAME — the rung a prose query reaches when no
//! embedder resolves.
//!
//! This is the only ranking most benchmark runs ever see, because no
//! embedder is configured there, so its failure mode is the one that costs
//! the most and is noticed the least. It is still a name match and every
//! answer carrying it says so; the job here is to make it the *best* name
//! match, not to let it pass for meaning.
//!
//! # The three things a naive term match gets wrong (#3138)
//!
//! Scoring "how many distinct query terms appear as substrings of
//! `path + concatenated symbol names`" fails three ways at once, and the
//! three compound:
//!
//! 1. **The query's spelling is the one the haystack misses.** `streaming`
//!    is not a substring of `StreamFallbackPosture` (`stream` would be), and
//!    `recovers` is not a substring of `Recovery`. So the single strongest
//!    signal in the corpus is invisible to the single most natural phrasing.
//!    `stem` normalises both sides to a common form first.
//! 2. **A substring is not a word.** Concatenating a file's symbol names into
//!    one haystack lets any term match across a name boundary or mid-word,
//!    which is a coincidence, not a signal. Both sides are split into word
//!    units instead (`push_units`) — on `_`, on `-`, on `/`, and on case
//!    boundaries — so `path` matches the symbol `only_path_ref` and not the
//!    letters inside `pathological`.
//! 3. **A file with fifty symbols has fifty chances.** Distinct-terms-matched
//!    rewards size directly: the measured rank 1 for
//!    *"how a provider recovers when its streaming path is broken"* was a
//!    100-symbol Python test module that mentioned `provider`, `recovers` and
//!    `path` in three unrelated test names. Two independent brakes answer
//!    this: repeats inside one file saturate logarithmically
//!    (`repetition_weight`), and a term matching much of the tree is worth
//!    proportionally less than a rare one (`rarity_weight`) — the
//!    generalisation of the argument `STOPWORDS` already makes for `the`
//!    and `and`.
//!
//! # Determinism (invariant 7)
//!
//! Every weight here is integer arithmetic over `u64` — no floats, no `ln`,
//! no platform libm — and every collection iterated during scoring is a
//! `Vec` in a caller-supplied order, never a `HashMap`. The final order is
//! score descending then path ascending, and paths in a code-graph index are
//! unique, so the comparison is a total order and two runs of one query rank
//! byte-identically on any machine.
//!
//! # Why the scoring lives here and not beside the graph query
//!
//! [`rank`] is a pure function of `(corpus, query)`. `super::enrich::name_hits`
//! is the half that talks to [`stella_graph::CodeGraph`]; everything a probe
//! needs to measure is in this module, which is what lets
//! `crates/stella-tools/tests/search_recall.rs` pin the ranking against a
//! frozen corpus with no index, no network and no key.

/// One indexed file, as the name rung sees it.
///
/// Deliberately owns its strings rather than borrowing the graph's: the
/// corpus is gathered once per query from `file_neighborhood`, and a
/// borrowed shape would tie the pure ranking to the lifetime of a database
/// handle it must not know about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedNames {
    /// Workspace-relative path, `/`-separated.
    pub path: String,
    /// Every symbol name the index holds for this file, in index order.
    pub symbols: Vec<String>,
}

/// One file's standing in a ranking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoredName {
    /// The weighted score. Comparable only against other scores from the
    /// same `rank` call — it is corpus-relative by construction.
    pub score: u64,
    /// How many of the query's terms matched at all. This is what the answer
    /// discloses to the reader, because "matched 2 of 5 terms" is a fact a
    /// human can check and a weighted score is not.
    pub matched_terms: usize,
    /// The file's workspace-relative path.
    pub path: String,
}

/// A term found in the file's own **name** is worth this many found in its
/// symbols.
///
/// `retry.rs` is about retries; a file that happens to define one function
/// mentioning retry is not. Three, matching [`super::scan`]'s
/// `PATH_TERM_WEIGHT` and for the same reason — the two rungs answer the
/// same question against different haystacks, and a reader comparing their
/// answers should not have to hold two different weightings in mind.
const BASENAME_WEIGHT: u64 = 3;

/// A term found in a **directory** component is worth this many found in the
/// file's symbols.
///
/// Lower than [`BASENAME_WEIGHT`] on purpose: `crates/stella-model/` says
/// this file is somewhere in the model crate, which is a category rather
/// than a claim, and every one of that crate's files makes it equally.
const DIRECTORY_WEIGHT: u64 = 1;

/// The shortest stem that may match by prefix rather than by equality.
///
/// Below this a prefix is not evidence: `log` is a prefix of `logic` and
/// `login`, and neither is what someone searching for logging meant. Four is
/// also the floor the stemmer honours, so a suffix is never stripped down to
/// a fragment that would then match half the tree.
const MIN_PREFIX_STEM: usize = 4;

/// How much of a word a prefix match is allowed to leave unexplained.
///
/// A prefix match exists to close the gap the stemmer could not — `recover`
/// against `recoveri`, `compact` against `compaction` — and every such gap is
/// an inflectional tail of a character or three. Past that the two words are
/// simply different words: `path` is a prefix of `pathological` and `work` of
/// `workspace`, and reading either as a match is exactly the incidental hit
/// this rung is being fixed to stop making.
const MAX_PREFIX_TAIL: usize = 3;

/// English function words that carry no signal in a path or a symbol name.
///
/// A length floor alone cannot do this job: `the`, `and` and `for` are three
/// characters and pure noise, while `api`, `sql`, `url` and `log` are three
/// characters and exactly what someone is searching for. So the floor stays
/// at three and the handful of words that would otherwise match half the tree
/// are named. Deliberately short — this is a noise filter, not a linguistics
/// project, and every word added is a word a caller can no longer search for.
///
/// [`rarity_weight`] is the same argument made continuously, and does not
/// replace this list: a stopword is noise in *every* corpus, whereas rarity
/// is a fact about *this* one.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "are", "was", "were", "that", "this", "with", "from", "into", "when",
    "where", "what", "which", "does", "how", "its", "not", "but", "all", "any", "can", "has",
    "have", "before", "after", "them", "then", "than",
];

/// Words worth matching on: lowercase, alphanumeric, at least three
/// characters, and not a stopword.
///
/// This is the shared vocabulary of both term-matching rungs — [`rank`] and
/// [`super::scan::scan_hits`] — so a word one of them refuses to search for
/// is a word neither searches for.
pub fn terms_of(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().count() >= 3)
        .map(str::to_lowercase)
        .filter(|word| !STOPWORDS.contains(&word.as_str()))
        .collect();
    terms.sort();
    terms.dedup();
    terms
}

/// [`terms_of`], stemmed — the vocabulary [`rank`] actually matches on.
///
/// Stemming is applied here rather than inside [`terms_of`] on purpose, and
/// the divergence is deliberate rather than an oversight. `scan_hits` matches
/// its terms as substrings of raw file *contents*, where the inflected
/// spelling is usually present verbatim and a stem would only widen the
/// match; this rung matches whole word units of *identifiers*, where the
/// inflection is exactly what is missing. The two rungs therefore share the
/// stopword list and the length floor — the part that decides what is worth
/// searching for — and differ only in the normalisation each haystack needs.
pub fn stems_of(query: &str) -> Vec<String> {
    let mut stems: Vec<String> = terms_of(query).iter().map(|term| stem(term)).collect();
    stems.sort();
    stems.dedup();
    stems
}

/// A crude suffix stemmer: enough to make `streaming`/`Stream` and
/// `recovers`/`Recovery` meet, and nothing more.
///
/// One suffix is stripped, never two, and only when at least
/// [`MIN_PREFIX_STEM`] characters survive — a stem shorter than that stops
/// being a word and starts being a prefix of everything. The trailing `y → i`
/// fold is Porter's, and is what makes the *plural* of a `-y` word meet its
/// singular (`directories` → `director`, `directory` → `directori`, which
/// then match by prefix).
///
/// Deliberately not a real Porter/Snowball implementation: identifiers are
/// not English prose, a full stemmer would pull in a dependency for the
/// three suffixes that actually occur in queries, and every extra rule is
/// another way for two unrelated words to collide.
fn stem(word: &str) -> String {
    const SUFFIXES: [&str; 4] = ["ing", "ed", "es", "s"];
    let mut base = word;
    for suffix in SUFFIXES {
        if let Some(shorter) = word.strip_suffix(suffix)
            && shorter.chars().count() >= MIN_PREFIX_STEM
        {
            base = shorter;
            break;
        }
    }
    if base.chars().count() >= MIN_PREFIX_STEM
        && let Some(without_y) = base.strip_suffix('y')
    {
        return format!("{without_y}i");
    }
    base.to_string()
}

/// Append `text`'s word units to `out`, already lowercased and stemmed.
///
/// Splits on every non-alphanumeric character and on case boundaries, so
/// `StreamFallbackPosture`, `stream_fallback_posture` and
/// `stream-fallback-posture` all yield the same three units. An acronym run
/// keeps its shape until the word that follows it starts: `HTTPServer` is
/// `http` + `server`, not `h` + `t` + `t` + `p` + `server`.
fn push_units(text: &str, out: &mut Vec<String>) {
    let chars: Vec<char> = text.chars().collect();
    let mut current = String::new();
    for (index, &ch) in chars.iter().enumerate() {
        if !ch.is_alphanumeric() {
            flush(&mut current, out);
            continue;
        }
        let previous = index.checked_sub(1).and_then(|before| chars.get(before));
        let starts_word = previous.is_some_and(|&before| {
            ch.is_uppercase()
                && (before.is_lowercase()
                    || before.is_numeric()
                    || (before.is_uppercase()
                        && chars.get(index + 1).is_some_and(|next| next.is_lowercase())))
        });
        if starts_word {
            flush(&mut current, out);
        }
        current.push(ch);
    }
    flush(&mut current, out);
}

/// Move `current` into `out` as a lowercased stem, leaving it empty.
fn flush(current: &mut String, out: &mut Vec<String>) {
    if !current.is_empty() {
        out.push(stem(&current.to_lowercase()));
        current.clear();
    }
}

/// Whether `stem` matches any of `units`, which are already stemmed.
///
/// Equality first, then a **bounded** prefix in either direction: one side of
/// a pair the stemmer could not fold (`recover` / `recoveri`, `compact` /
/// `compaction`) is always a prefix of the other, and which side is shorter
/// depends on which spelling the *query* happened to use. Each direction
/// guards the shorter string against [`MIN_PREFIX_STEM`] and the longer one
/// against [`MAX_PREFIX_TAIL`], so a match is a word and its inflections and
/// never a word that merely starts the same way.
fn matches(stem: &str, units: &[String]) -> bool {
    units
        .iter()
        .any(|unit| unit == stem || extends(unit, stem) || extends(stem, unit))
}

/// Whether `longer` is `shorter` plus a non-empty, at-most-inflectional tail.
///
/// Equality is handled by its caller, so this never has to decide whether a
/// three-character term equals itself — the length floor here is only ever
/// asked about a genuine prefix.
fn extends(longer: &str, shorter: &str) -> bool {
    let Some(tail) = longer.strip_prefix(shorter) else {
        return false;
    };
    shorter.chars().count() >= MIN_PREFIX_STEM && tail.chars().count() <= MAX_PREFIX_TAIL
}

/// How much a term is worth for matching in a corpus where `hits` of
/// `corpus` files contain it: `1 + floor(log2(corpus / hits))`.
///
/// Integer inverse document frequency. A term in every file scores 1 — it
/// still counts, because a query is not evidence-free just because its words
/// are common — and each halving of its reach adds one. A term in 1 file of
/// 2000 is worth 11 of a term in every file.
///
/// `floor(log2(…))` rather than a real logarithm so the arithmetic is exact
/// and identical on every platform (invariant 7).
fn rarity_weight(corpus: usize, hits: usize) -> u64 {
    let ratio = corpus.max(1) / hits.max(1);
    u64::from(ratio.max(1).ilog2()) + 1
}

/// How much `repeats` symbols in one file naming the same term are worth:
/// `1 + floor(log2(repeats))`.
///
/// Saturating rather than linear, which is the direct brake on defect 3
/// above: a file whose 40 test functions all say `provider` is more about
/// providers than a file with one, but not forty times more, and left linear
/// it would outrank the file that *defines* the thing.
fn repetition_weight(repeats: usize) -> u64 {
    u64::from(repeats.max(1).ilog2()) + 1
}

/// A file's name and symbols reduced to the word units the ranking compares.
struct FileUnits {
    /// Units of the last path component — the file's own name.
    basename: Vec<String>,
    /// Units of every directory component above it.
    directories: Vec<String>,
    /// One unit list per symbol, kept separate so a term matching four
    /// different symbols is distinguishable from one matching a single
    /// symbol four times over.
    symbols: Vec<Vec<String>>,
}

impl FileUnits {
    /// Split one indexed file into its word units.
    fn of(file: &IndexedNames) -> FileUnits {
        let (directories, basename) = match file.path.rsplit_once('/') {
            Some((above, name)) => (above, name),
            None => ("", file.path.as_str()),
        };
        let mut basename_units = Vec::new();
        push_units(basename, &mut basename_units);
        let mut directory_units = Vec::new();
        push_units(directories, &mut directory_units);
        FileUnits {
            basename: basename_units,
            directories: directory_units,
            symbols: file
                .symbols
                .iter()
                .map(|symbol| {
                    let mut units = Vec::new();
                    push_units(symbol, &mut units);
                    units
                })
                .collect(),
        }
    }

    /// Whether this file contains `stem` anywhere at all — the question
    /// [`rarity_weight`] counts over the corpus.
    fn contains(&self, stem: &str) -> bool {
        matches(stem, &self.basename)
            || matches(stem, &self.directories)
            || self.symbols.iter().any(|symbol| matches(stem, symbol))
    }

    /// This file's unweighted credit for one term: the weighted count of
    /// where it appears, before rarity scales it.
    fn credit(&self, stem: &str) -> u64 {
        let mut credit = 0;
        if matches(stem, &self.basename) {
            credit += BASENAME_WEIGHT;
        }
        if matches(stem, &self.directories) {
            credit += DIRECTORY_WEIGHT;
        }
        let repeats = self
            .symbols
            .iter()
            .filter(|symbol| matches(stem, symbol))
            .count();
        if repeats > 0 {
            credit += repetition_weight(repeats);
        }
        credit
    }
}

/// Rank `corpus` against `query`, best first.
///
/// Files matching no term at all are absent from the result rather than
/// present with score zero, so `result.len()` is the honest "how many files
/// matched" the caller discloses.
///
/// Deterministic: score descending, then path ascending. See the module doc
/// for why that is a total order and why no step of the arithmetic can vary
/// by platform.
pub fn rank(corpus: &[IndexedNames], query: &str) -> Vec<ScoredName> {
    let stems = stems_of(query);
    if stems.is_empty() {
        return Vec::new();
    }
    let units: Vec<FileUnits> = corpus.iter().map(FileUnits::of).collect();
    let rarity: Vec<u64> = stems
        .iter()
        .map(|stem| {
            let hits = units.iter().filter(|file| file.contains(stem)).count();
            rarity_weight(corpus.len(), hits)
        })
        .collect();

    let mut scored: Vec<ScoredName> = corpus
        .iter()
        .zip(units.iter())
        .filter_map(|(file, units)| {
            let mut score = 0;
            let mut matched_terms = 0;
            for (index, stem) in stems.iter().enumerate() {
                let credit = units.credit(stem);
                if credit > 0 {
                    matched_terms += 1;
                    score += credit * rarity.get(index).copied().unwrap_or(1);
                }
            }
            (matched_terms > 0).then(|| ScoredName {
                score,
                matched_terms,
                path: file.path.clone(),
            })
        })
        .collect();
    scored.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
    scored
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, symbols: &[&str]) -> IndexedNames {
        IndexedNames {
            path: path.to_string(),
            symbols: symbols.iter().map(|symbol| symbol.to_string()).collect(),
        }
    }

    fn paths(scored: &[ScoredName]) -> Vec<&str> {
        scored.iter().map(|hit| hit.path.as_str()).collect()
    }

    /// The stopword list filters function words while keeping the short
    /// identifiers people actually search for.
    #[test]
    fn function_words_are_not_search_terms_but_short_identifiers_are() {
        let terms = terms_of("where does the api log its sql errors");
        assert!(terms.contains(&"api".to_string()));
        assert!(terms.contains(&"sql".to_string()));
        assert!(terms.contains(&"log".to_string()));
        assert!(terms.contains(&"errors".to_string()));
        assert!(!terms.contains(&"where".to_string()));
        assert!(!terms.contains(&"does".to_string()));
        assert!(!terms.contains(&"the".to_string()));
        assert!(!terms.contains(&"its".to_string()));
    }

    /// The pairs the issue named, plus the floor that stops a stem becoming
    /// a fragment.
    #[test]
    fn the_query_spelling_and_the_identifier_spelling_meet() {
        assert_eq!(stem("streaming"), "stream");
        assert_eq!(stem("stream"), "stream");
        assert!(matches(
            &stem("streaming"),
            &[stem("Stream".to_lowercase().as_str())]
        ));

        assert_eq!(stem("recovers"), "recover");
        assert_eq!(stem("recovery"), "recoveri");
        assert!(matches(&stem("recovers"), &[stem("recovery")]));

        // `-y` and `-ies` fold onto the same stem, in both directions.
        assert_eq!(stem("directory"), "directori");
        assert_eq!(stem("directories"), "directori");

        // A stem below the floor is not stripped: `us` would prefix-match
        // half the tree.
        assert_eq!(stem("used"), "used");
        assert_eq!(stem("runs"), "runs");
    }

    /// A bounded prefix is an inflection; an unbounded one is a different
    /// word. This is the rule that keeps `recover`/`recovery` together and
    /// `path`/`pathological` apart.
    #[test]
    fn a_prefix_match_is_an_inflection_and_not_a_longer_word() {
        assert!(matches("compact", &["compaction".into()]));
        assert!(matches("detect", &["detector".into()]));
        assert!(!matches("path", &["pathological".into()]));
        assert!(!matches("work", &["workspace".into()]));
        // Below the floor, only equality counts — `log` must not reach
        // `login`.
        assert!(matches("log", &["log".into()]));
        assert!(!matches("log", &["login".into()]));
    }

    /// Case boundaries, separators and acronym runs all yield the same units.
    #[test]
    fn a_symbol_name_splits_into_the_words_it_is_made_of() {
        let mut units = Vec::new();
        push_units("StreamFallbackPosture", &mut units);
        assert_eq!(units, ["stream", "fallback", "posture"]);
        let mut snake = Vec::new();
        push_units("stream_fallback_posture", &mut snake);
        assert_eq!(snake, units);
        let mut acronym = Vec::new();
        push_units("HTTPServerV2", &mut acronym);
        assert_eq!(acronym, ["http", "server", "v2"]);
    }

    /// **Defect 1.** The natural phrasing of a query reaches the identifier
    /// spelled differently — which plain substring matching cannot do.
    #[test]
    fn an_inflected_query_reaches_the_uninflected_symbol() {
        let corpus = [
            file("src/parity.rs", &["StreamFallbackPosture"]),
            file("src/unrelated.rs", &["Matrix", "multiply"]),
        ];
        let ranked = rank(&corpus, "streaming");
        assert_eq!(paths(&ranked), ["src/parity.rs"]);
        assert!(
            !"src/parity.rs streamfallbackposture".contains("streaming"),
            "the premise: the raw substring match this replaces cannot find it"
        );
    }

    /// **Defect 2.** A term inside a word is a coincidence, not a match.
    #[test]
    fn a_term_matches_a_word_and_not_the_letters_inside_one() {
        let corpus = [
            file("src/routing.rs", &["only_path_ref"]),
            file("src/mid.rs", &["pathological_case"]),
        ];
        let ranked = rank(&corpus, "path");
        assert_eq!(
            paths(&ranked),
            ["src/routing.rs"],
            "`pathological` is not about paths"
        );
    }

    /// **Defect 3, the mechanism.** Repeating a term across a file's symbols
    /// saturates instead of accumulating, so sixty test functions naming
    /// `provider` are worth six of one, not sixty.
    ///
    /// The end-to-end consequence — the 100-symbol bench module losing the
    /// top of the measured repro query — is pinned by the probe set in
    /// `crates/stella-tools/tests/search_recall.rs`, which has a corpus wide
    /// enough for rarity to mean anything. This asserts the brake itself.
    #[test]
    fn repeating_a_term_across_symbols_saturates_instead_of_accumulating() {
        assert_eq!(repetition_weight(1), 1);
        assert_eq!(repetition_weight(2), 2);
        assert_eq!(repetition_weight(60), 6);
        assert!(
            repetition_weight(60) < 60,
            "linear credit for repetition is defect 3 itself"
        );
        // A file that merely mentions the term many times must not outweigh
        // one that is NAMED for it by more than the saturated margin.
        let sprawling: Vec<String> = (0..60)
            .map(|index| format!("test_provider_case_{index:02}"))
            .collect();
        let corpus = vec![
            IndexedNames {
                path: "bench/tests/test_analysis.py".to_string(),
                symbols: sprawling,
            },
            file("src/provider.rs", &[]),
        ];
        let ranked = rank(&corpus, "provider");
        let sprawl = ranked
            .iter()
            .find(|hit| hit.path.starts_with("bench/"))
            .expect("the sprawling file matches");
        let named = ranked
            .iter()
            .find(|hit| hit.path == "src/provider.rs")
            .expect("the named file matches");
        assert!(
            sprawl.score <= named.score * 2,
            "sixty incidental mentions outran the file named for the term: {ranked:?}"
        );
    }

    /// A term that reaches most of the corpus carries less than a rare one,
    /// which is [`STOPWORDS`]'s argument made continuously.
    #[test]
    fn a_term_matching_most_of_the_tree_is_worth_less_than_a_rare_one() {
        let mut corpus: Vec<IndexedNames> = (0..64)
            .map(|index| file_owned(format!("src/common_{index:02}.rs"), vec!["session".into()]))
            .collect();
        corpus.push(file("src/a_common.rs", &["session"]));
        corpus.push(file("src/z_rare.rs", &["quiescence"]));
        let ranked = rank(&corpus, "session quiescence");
        assert_eq!(
            paths(&ranked).first().copied(),
            Some("src/z_rare.rs"),
            "the rare term must outweigh the ubiquitous one: {:?}",
            &ranked[..3.min(ranked.len())]
        );
    }

    fn file_owned(path: String, symbols: Vec<String>) -> IndexedNames {
        IndexedNames { path, symbols }
    }

    /// Invariant 7: one query, one ranking, however the corpus is presented.
    #[test]
    fn the_same_query_ranks_identically_however_the_corpus_is_ordered() {
        let corpus = vec![
            file("src/alpha.rs", &["stream_fallback"]),
            file("src/beta.rs", &["provider_lookup", "provider_row"]),
            file("src/gamma.rs", &["recovery_latch"]),
        ];
        let forward = rank(&corpus, "provider stream recovery");
        let mut reversed = corpus;
        reversed.reverse();
        let backward = rank(&reversed, "provider stream recovery");
        assert_eq!(forward, backward);
        assert_eq!(forward, rank(&reversed, "provider stream recovery"));
    }

    /// A query of nothing but stopwords ranks nothing, rather than ranking
    /// everything equally.
    #[test]
    fn a_query_with_no_terms_ranks_nothing() {
        let corpus = [file("src/alpha.rs", &["anything"])];
        assert!(rank(&corpus, "the and for").is_empty());
        assert!(rank(&corpus, "").is_empty());
    }
}
