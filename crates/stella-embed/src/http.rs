// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! [`HttpEmbedder`] — the semantic backend, and the one place in this crate
//! that performs I/O.
//!
//! It speaks the OpenAI `POST {base}/embeddings` shape, which is not a vendor
//! choice so much as the industry's de-facto embedding wire format: Voyage
//! (`voyage-code-3`, purpose-built for code retrieval), OpenAI
//! (`text-embedding-3-small`/`-large`), Ollama, llama.cpp's server and
//! HuggingFace TEI all accept the same request and return the same response.
//! One adapter therefore covers both the hosted case and the fully offline
//! case (a local server on `127.0.0.1`), which is why this crate does not
//! vendor an inference runtime to get semantics.
//!
//! # Configuration
//!
//! Resolution is a **pure function** of [`EmbedderEnv`] ([`resolve`]), so it is
//! tested without touching process environment. [`from_env`] is the thin
//! wrapper that reads the real one.
//!
//! | variable | meaning |
//! |---|---|
//! | `STELLA_EMBED_URL` | base URL, e.g. `http://127.0.0.1:11434/v1` — wins over every shortcut |
//! | `STELLA_EMBED_MODEL` | model id; **required** whenever `STELLA_EMBED_URL` is set |
//! | `STELLA_EMBED_API_KEY` | bearer token, optional for a local server |
//! | `STELLA_EMBED_DIMS` | vector width; required for a model this crate does not know |
//! | `STELLA_EMBED_FLOOR` | override the admission floor |
//! | `VOYAGE_API_KEY` | shortcut: `voyage-code-3` at `api.voyageai.com/v1` |
//! | `OPENAI_API_KEY` | shortcut: `text-embedding-3-small` at `api.openai.com/v1` |
//!
//! Nothing set is not an error — it is [`Resolution::Unconfigured`], and the
//! caller degrades to a labelled lexical answer. A *half*-set configuration is
//! [`Resolution::Incomplete`], which names the missing piece rather than
//! silently behaving as if nothing were configured at all.

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;

use crate::hash::l2_normalize;
use crate::seam::{EmbedError, Embedder, EmbedderFingerprint, Embedding, SimilarityPosture};

/// How long one embedding request may take before it is abandoned. Generous
/// because a cold local model server can take seconds to load weights, and
/// bounded because this sits on a tool call the agent is waiting on.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How many times one embedding request is issued before its failure becomes
/// the caller's.
///
/// **A rate limit is not an error, it is an instruction.** A backend that
/// answers `429` is saying "ask again shortly"; treating that as a terminal
/// failure hands the caller a dead pass over a condition that resolves by
/// itself. This mattered little while the chunk embedding pass issued one
/// request at a time and merely ran slowly, and it matters now that the pass
/// keeps several in flight (#4144) — concurrency is exactly what provokes a
/// rate limit, so the pass that got faster would otherwise be the pass that
/// started aborting.
///
/// Bounded, because retrying forever is how a caller waits on a backend that
/// is never going to answer. Four attempts spans roughly seven seconds of
/// backoff, which clears an ordinary burst limit without turning a genuine
/// outage into a long silence.
const MAX_ATTEMPTS: u32 = 4;

/// The first pause between attempts; each subsequent one doubles.
const INITIAL_BACKOFF: Duration = Duration::from_millis(500);

/// The ceiling on any single pause — including one a `Retry-After` header
/// asks for. A backend is entitled to ask for a delay; it is not entitled to
/// park a session-start backfill for an hour, and a header this large is more
/// often a misconfigured proxy than a real instruction.
const MAX_BACKOFF: Duration = Duration::from_secs(8);

/// The admission floor applied when nothing overrides it.
///
/// **This number is provisional and is not a measured separation point.** The
/// [`SimilarityPosture`] contract asks for a floor derived from a model's
/// observed relevant/irrelevant score distributions, and no such measurement
/// exists yet for these backends on a code corpus. It is deliberately
/// permissive: its job today is to drop the obviously-unrelated tail from an
/// ordered list, not to certify anything.
///
/// The measurement that settles it is written and reproducible:
/// `crates/stella-tools/tests/relevance_calibration.rs` prints the labelled
/// relevant/irrelevant score distributions over this repository for whatever
/// backend is configured, and names the number to write here (#3096). It is
/// `#[ignore]`d because it needs a real key and a full embedding pass. Until
/// somebody runs it, this paragraph stays — deleting it without the
/// measurement would turn an honest disclosure into a silent assumption.
const DEFAULT_ADMISSION_FLOOR: f32 = 0.25;

/// Models this crate knows the vector width of, so `STELLA_EMBED_DIMS` is only
/// mandatory for something it has never heard of. Width matters before the
/// first call because it is part of the fingerprint every stored vector is
/// stamped with.
const KNOWN_DIMS: &[(&str, usize)] = &[
    ("voyage-code-3", 1024),
    ("voyage-3", 1024),
    ("voyage-3-lite", 512),
    ("text-embedding-3-small", 1536),
    ("text-embedding-3-large", 3072),
    ("nomic-embed-text", 768),
    ("bge-small-en-v1.5", 384),
    ("bge-base-en-v1.5", 768),
    ("bge-m3", 1024),
];

/// `STELLA_EMBED_URL` — the base URL.
const VAR_URL: &str = "STELLA_EMBED_URL";
/// `STELLA_EMBED_MODEL` — the model id.
const VAR_MODEL: &str = "STELLA_EMBED_MODEL";
/// `STELLA_EMBED_API_KEY` — the bearer token for that base URL.
const VAR_API_KEY: &str = "STELLA_EMBED_API_KEY";
/// `STELLA_EMBED_DIMS` — the vector width.
const VAR_DIMS: &str = "STELLA_EMBED_DIMS";
/// `STELLA_EMBED_FLOOR` — the admission floor.
const VAR_FLOOR: &str = "STELLA_EMBED_FLOOR";
/// `VOYAGE_API_KEY` — the Voyage shortcut.
const VAR_VOYAGE_API_KEY: &str = "VOYAGE_API_KEY";
/// `OPENAI_API_KEY` — the OpenAI shortcut.
const VAR_OPENAI_API_KEY: &str = "OPENAI_API_KEY";

/// Every process-environment variable [`EmbedderEnv::from_process`] reads, so
/// a caller that has to *neutralize* this crate's configuration surface — a
/// test spawning a binary it must not let reach a billed backend, a sandbox
/// building a child environment — enumerates it instead of transcribing a
/// list that then drifts (#4542).
///
/// A shortcut key is enough on its own to resolve a hosted backend, and
/// `STELLA_EMBED_URL` redirects one, so the whole table is the credential
/// surface rather than the three fields whose names say `KEY`.
pub const ENV_VARS: &[&str] = &[
    VAR_URL,
    VAR_MODEL,
    VAR_API_KEY,
    VAR_DIMS,
    VAR_FLOOR,
    VAR_VOYAGE_API_KEY,
    VAR_OPENAI_API_KEY,
];

/// The environment [`resolve`] reads, captured as data so resolution is a pure
/// function and its table of precedences is testable.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct EmbedderEnv {
    /// `STELLA_EMBED_URL`.
    pub url: Option<String>,
    /// `STELLA_EMBED_MODEL`.
    pub model: Option<String>,
    /// `STELLA_EMBED_API_KEY`.
    pub api_key: Option<String>,
    /// `STELLA_EMBED_DIMS`.
    pub dims: Option<String>,
    /// `STELLA_EMBED_FLOOR`.
    pub floor: Option<String>,
    /// `VOYAGE_API_KEY`.
    pub voyage_api_key: Option<String>,
    /// `OPENAI_API_KEY`.
    pub openai_api_key: Option<String>,
}

impl EmbedderEnv {
    /// Read the real process environment. The only impure part of resolution.
    ///
    /// Every name it reads is a constant listed in [`ENV_VARS`], which is what
    /// makes that list the environment surface rather than a description of
    /// it — proven in both directions by
    /// `from_lookup_reads_exactly_the_listed_variables`.
    pub fn from_process() -> Self {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// [`Self::from_process`] over an arbitrary lookup, so the set of names it asks
    /// for is observable without mutating the process environment under a
    /// parallel suite.
    fn from_lookup(mut get: impl FnMut(&str) -> Option<String>) -> Self {
        let mut read = |name: &str| {
            get(name)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        Self {
            url: read(VAR_URL),
            model: read(VAR_MODEL),
            api_key: read(VAR_API_KEY),
            dims: read(VAR_DIMS),
            floor: read(VAR_FLOOR),
            voyage_api_key: read(VAR_VOYAGE_API_KEY),
            openai_api_key: read(VAR_OPENAI_API_KEY),
        }
    }
}

/// A secret field's *presence*, never its value. Debug output is the one
/// rendering of these structs that reaches a log line, a `dbg!()`, or a panic
/// message without anyone deciding it should, so the key never travels in it —
/// while whether a key was set at all is exactly what a reader is debugging.
struct Redacted<'a>(&'a Option<String>);

impl fmt::Debug for Redacted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(_) => f.write_str("Some(<redacted>)"),
            None => f.write_str("None"),
        }
    }
}

/// Hand-rolled so the three key fields cannot be printed. Deriving `Debug`
/// here would put a live API key one `{:?}` away from a log file; this crate
/// is a leaf and cannot borrow `stella-model`'s `ApiKey`, so it copies the
/// shape of that type's redacting impl (`crates/stella-model/src/credential.rs`).
impl fmt::Debug for EmbedderEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EmbedderEnv")
            .field("url", &self.url)
            .field("model", &self.model)
            .field("api_key", &Redacted(&self.api_key))
            .field("dims", &self.dims)
            .field("floor", &self.floor)
            .field("voyage_api_key", &Redacted(&self.voyage_api_key))
            .field("openai_api_key", &Redacted(&self.openai_api_key))
            .finish()
    }
}

/// What the environment resolved to. Three outcomes, because "nothing
/// configured" and "configured wrong" are different answers and collapsing
/// them is how a typo becomes a silent quality regression.
#[derive(Debug)]
pub enum Resolution {
    /// A usable semantic backend.
    Configured(Box<HttpEmbedder>),
    /// Nothing was set. Not an error: the caller degrades, labelled.
    Unconfigured,
    /// Something was set but not enough to build a backend. The string names
    /// the missing piece and is meant to be shown to the user verbatim.
    Incomplete(String),
}

/// Resolve an embedder from a captured environment. Pure.
pub fn resolve(env: &EmbedderEnv) -> Resolution {
    let floor = match env.floor.as_deref() {
        None => DEFAULT_ADMISSION_FLOOR,
        Some(raw) => match raw.parse::<f32>() {
            Ok(value) if value.is_finite() => value,
            _ => {
                return Resolution::Incomplete(format!(
                    "STELLA_EMBED_FLOOR is `{raw}`, which is not a finite number"
                ));
            }
        },
    };

    // An explicit base URL wins over every shortcut: it is the only way to
    // point at a local server, and a stray vendor key in the environment must
    // not silently redirect an offline setup to the network.
    if let Some(url) = env.url.as_deref() {
        let Some(model) = env.model.as_deref() else {
            return Resolution::Incomplete(
                "STELLA_EMBED_URL is set but STELLA_EMBED_MODEL is not — a base URL does not \
                 imply a model, and guessing one would produce vectors under a fingerprint that \
                 lies about what made them"
                    .to_string(),
            );
        };
        return match dims_for(model, env.dims.as_deref()) {
            Ok(dims) => Resolution::Configured(Box::new(HttpEmbedder::new(
                url,
                model,
                env.api_key.clone(),
                dims,
                floor,
            ))),
            Err(message) => Resolution::Incomplete(message),
        };
    }

    if let Some(key) = env.voyage_api_key.as_deref() {
        // voyage-code-3 is trained for code retrieval specifically, which is
        // the question `graph_query` is being asked.
        return Resolution::Configured(Box::new(HttpEmbedder::new(
            "https://api.voyageai.com/v1",
            "voyage-code-3",
            Some(key.to_string()),
            1024,
            floor,
        )));
    }

    if let Some(key) = env.openai_api_key.as_deref() {
        return Resolution::Configured(Box::new(HttpEmbedder::new(
            "https://api.openai.com/v1",
            "text-embedding-3-small",
            Some(key.to_string()),
            1536,
            floor,
        )));
    }

    Resolution::Unconfigured
}

/// Resolve an embedder from the real process environment.
pub fn from_env() -> Resolution {
    resolve(&EmbedderEnv::from_process())
}

fn dims_for(model: &str, override_dims: Option<&str>) -> Result<usize, String> {
    if let Some(raw) = override_dims {
        return raw
            .parse::<usize>()
            .ok()
            .filter(|dims| *dims > 0)
            .ok_or_else(|| {
                format!("STELLA_EMBED_DIMS is `{raw}`, which is not a positive integer")
            });
    }
    KNOWN_DIMS
        .iter()
        .find(|(known, _)| *known == model)
        .map(|(_, dims)| *dims)
        .ok_or_else(|| {
            format!(
                "STELLA_EMBED_DIMS is required for model `{model}`, whose vector width this build \
                 does not know. The width is part of the fingerprint stamped on every stored \
                 vector, so it has to be right before the first call, not discovered after it"
            )
        })
}

/// An embedder backed by an OpenAI-shaped `/embeddings` endpoint.
pub struct HttpEmbedder {
    endpoint: String,
    model: String,
    api_key: Option<String>,
    dims: usize,
    admission_floor: f32,
    client: reqwest::Client,
}

impl HttpEmbedder {
    /// Build an embedder against `base_url` (with or without a trailing
    /// slash); `/embeddings` is appended.
    pub fn new(
        base_url: &str,
        model: &str,
        api_key: Option<String>,
        dims: usize,
        admission_floor: f32,
    ) -> Self {
        Self {
            endpoint: format!("{}/embeddings", base_url.trim_end_matches('/')),
            model: model.to_string(),
            api_key,
            dims,
            admission_floor,
            // A builder failure here means the TLS backend could not be
            // initialised at all. Falling back to a default client keeps this
            // constructor infallible — the very next request fails with a
            // named `EmbedError::Backend` carrying the real reason, which is a
            // better error than one raised before anyone asked for a vector.
            client: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .unwrap_or_default(),
        }
    }

    /// The endpoint this embedder posts to. Surfaced so a caller can tell the
    /// user *where* its vectors came from without re-deriving the URL.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

/// Hand-rolled for the same reason as [`EmbedderEnv`]'s: the bearer token this
/// struct holds must not reach a log line through a derived `Debug`.
/// [`Resolution`] keeps its derive, since the key is only reachable through
/// here.
impl fmt::Debug for HttpEmbedder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpEmbedder")
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("api_key", &Redacted(&self.api_key))
            .field("dims", &self.dims)
            .field("admission_floor", &self.admission_floor)
            .finish_non_exhaustive()
    }
}

/// How long the backend asked us to wait, if it said so in a form worth
/// trusting.
///
/// Only the delta-seconds form of `Retry-After` is read. The HTTP-date form
/// is legal and is deliberately ignored: honouring it means trusting the
/// caller's clock to agree with the server's, and a skewed clock turns a
/// two-second pause into a two-hour one. Falling back to the local backoff
/// schedule is wrong by at most a few seconds; trusting a bad date is wrong
/// by however far the clocks differ.
fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

#[async_trait]
impl Embedder for HttpEmbedder {
    fn fingerprint(&self) -> EmbedderFingerprint {
        EmbedderFingerprint {
            model_id: self.model.clone(),
            // Revision 1 of *this adapter's* rendering of a request. It is
            // distinct from the model version, which the model id carries;
            // bump it if the request body changes in a way that could change
            // the vector for identical input.
            revision: "1".to_string(),
            dims: self.dims,
            normalization: "l2".to_string(),
        }
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Embedding>, EmbedError> {
        if texts.is_empty() {
            return Err(EmbedError::EmptyInput);
        }

        let body = serde_json::json!({
            "model": self.model,
            "input": texts,
        });

        // Re-issue on the statuses that mean "not now" and on nothing else.
        // A 401 or a 400 will say the same thing however many times it is
        // asked, so retrying one only delays an error the caller has to read
        // anyway; a 429 or a 5xx is a condition that clears.
        //
        // A transport error is deliberately *not* retried here. It is not a
        // backend instruction, it has no `Retry-After` to honour, and the
        // request may well have been received and billed — re-sending it is
        // a decision about the user's money, not a courtesy, and belongs to
        // whoever knows the pass can afford it.
        let response = {
            let mut attempt: u32 = 1;
            let mut backoff = INITIAL_BACKOFF;
            loop {
                let mut request = self.client.post(&self.endpoint).json(&body);
                if let Some(key) = &self.api_key {
                    request = request.bearer_auth(key);
                }
                let response = request
                    .send()
                    .await
                    .map_err(|error| EmbedError::Backend(format!("{}: {error}", self.endpoint)))?;

                let status = response.status();
                if status.is_success() {
                    break response;
                }

                let worth_retrying =
                    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
                if !worth_retrying || attempt >= MAX_ATTEMPTS {
                    let body = response.text().await.unwrap_or_default();
                    let body: String = body.chars().take(400).collect();
                    // The attempt count is in the message because "HTTP 429"
                    // alone reads as "the pass never even waited", and a user
                    // deciding whether their rate limit is the problem needs
                    // to know it already backed off and asked again.
                    let tried = if attempt > 1 {
                        format!(" after {attempt} attempts")
                    } else {
                        String::new()
                    };
                    return Err(EmbedError::Backend(format!(
                        "{} returned HTTP {status}{tried}: {body}",
                        self.endpoint
                    )));
                }

                let pause = retry_after(&response).unwrap_or(backoff).min(MAX_BACKOFF);
                tokio::time::sleep(pause).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
                attempt += 1;
            }
        };

        let payload: EmbeddingsResponse = response.json().await.map_err(|error| {
            EmbedError::Backend(format!("unreadable embeddings response: {error}"))
        })?;

        if payload.data.len() != texts.len() {
            return Err(EmbedError::CountMismatch {
                expected: texts.len(),
                got: payload.data.len(),
            });
        }

        // The API is free to return rows out of order; `index` is what says
        // which input a vector belongs to, and attributing a vector to the
        // wrong file is a silent ranking corruption rather than a failure.
        let mut ordered: Vec<Option<Vec<f32>>> = vec![None; texts.len()];
        for row in payload.data {
            let slot = ordered.get_mut(row.index).ok_or_else(|| {
                EmbedError::Backend(format!("response index {} out of range", row.index))
            })?;
            if row.embedding.len() != self.dims {
                return Err(EmbedError::DimensionMismatch {
                    expected: self.dims,
                    got: row.embedding.len(),
                });
            }
            *slot = Some(row.embedding);
        }

        let fingerprint = self.fingerprint().id();
        ordered
            .into_iter()
            .enumerate()
            .map(|(index, vector)| {
                let mut vector = vector.ok_or_else(|| {
                    EmbedError::Backend(format!("response carried no vector for input {index}"))
                })?;
                l2_normalize(&mut vector);
                Ok(Embedding {
                    fingerprint: fingerprint.clone(),
                    vector,
                })
            })
            .collect()
    }

    /// A trained embedding model maps meaning, which is the whole reason this
    /// backend exists — a query and a file that share no token can still land
    /// close together. The *floor*, unlike the posture, is provisional: no
    /// measured separation point exists yet for these backends on a code
    /// corpus. Measuring one per model is tracked in #2993 and #3096, and the
    /// harness that produces the distribution is
    /// `crates/stella-tools/tests/relevance_calibration.rs`.
    fn similarity_posture(&self) -> SimilarityPosture {
        SimilarityPosture::Semantic {
            admission_floor: self.admission_floor,
        }
    }
}

#[derive(serde::Deserialize)]
struct EmbeddingsResponse {
    data: Vec<EmbeddingRow>,
}

#[derive(serde::Deserialize)]
struct EmbeddingRow {
    #[serde(default)]
    index: usize,
    embedding: Vec<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn env_with(pairs: &[(&str, &str)]) -> EmbedderEnv {
        for (key, _) in pairs {
            assert!(ENV_VARS.contains(key), "unknown variable {key}");
        }
        EmbedderEnv::from_lookup(|name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        })
    }

    /// [`ENV_VARS`] is the whole environment surface, in both directions: a
    /// name read but unlisted would leave a variable a sandbox cannot know to
    /// clear, and a name listed but unread would have a caller stripping
    /// something that does nothing.
    #[test]
    fn from_lookup_reads_exactly_the_listed_variables() {
        let mut asked = Vec::new();
        let _ = EmbedderEnv::from_lookup(|name| {
            asked.push(name.to_string());
            None
        });
        assert_eq!(asked, ENV_VARS);
    }

    #[test]
    fn every_listed_variable_populates_a_field() {
        for name in ENV_VARS {
            let env = EmbedderEnv::from_lookup(|asked| (asked == *name).then(|| "set".to_string()));
            assert_ne!(
                env,
                EmbedderEnv::default(),
                "{name} is listed in ENV_VARS but populates no field"
            );
        }
    }

    #[test]
    fn an_empty_environment_is_unconfigured_not_an_error() {
        assert!(matches!(
            resolve(&EmbedderEnv::default()),
            Resolution::Unconfigured
        ));
    }

    #[test]
    fn an_explicit_base_url_wins_over_a_vendor_key() {
        // The offline case: a local server must not be silently redirected to
        // the network because a key happens to be exported.
        let env = env_with(&[
            ("STELLA_EMBED_URL", "http://127.0.0.1:11434/v1"),
            ("STELLA_EMBED_MODEL", "nomic-embed-text"),
            ("OPENAI_API_KEY", "sk-test"),
        ]);
        let Resolution::Configured(embedder) = resolve(&env) else {
            panic!("expected a configured embedder");
        };
        assert_eq!(embedder.endpoint(), "http://127.0.0.1:11434/v1/embeddings");
        assert_eq!(embedder.fingerprint().dims, 768);
    }

    #[test]
    fn a_base_url_without_a_model_names_what_is_missing() {
        let env = env_with(&[("STELLA_EMBED_URL", "http://127.0.0.1:11434/v1")]);
        let Resolution::Incomplete(message) = resolve(&env) else {
            panic!("expected an incomplete resolution");
        };
        assert!(message.contains("STELLA_EMBED_MODEL"), "{message}");
    }

    #[test]
    fn an_unknown_model_must_declare_its_width() {
        let env = env_with(&[
            ("STELLA_EMBED_URL", "http://127.0.0.1:8080/v1"),
            ("STELLA_EMBED_MODEL", "some-model-nobody-has-heard-of"),
        ]);
        let Resolution::Incomplete(message) = resolve(&env) else {
            panic!("expected an incomplete resolution");
        };
        assert!(message.contains("STELLA_EMBED_DIMS"), "{message}");
    }

    #[test]
    fn a_voyage_key_selects_the_code_specialised_model() {
        let Resolution::Configured(embedder) = resolve(&env_with(&[("VOYAGE_API_KEY", "vk-test")]))
        else {
            panic!("expected a configured embedder");
        };
        assert_eq!(embedder.fingerprint().model_id, "voyage-code-3");
        assert_eq!(
            embedder.endpoint(),
            "https://api.voyageai.com/v1/embeddings"
        );
    }

    #[test]
    fn voyage_is_preferred_over_openai_when_both_keys_exist() {
        let env = env_with(&[("VOYAGE_API_KEY", "vk"), ("OPENAI_API_KEY", "sk")]);
        let Resolution::Configured(embedder) = resolve(&env) else {
            panic!("expected a configured embedder");
        };
        assert_eq!(embedder.fingerprint().model_id, "voyage-code-3");
    }

    /// #3713: a derived `Debug` on either type put a live bearer token one
    /// `{:?}` away from a log line. Fails on the derive, passes on the
    /// hand-rolled impls.
    #[test]
    fn debug_never_prints_a_key_from_the_environment() {
        let env = env_with(&[
            ("STELLA_EMBED_API_KEY", "sk-secret-value"),
            ("VOYAGE_API_KEY", "vk-secret-value"),
            ("OPENAI_API_KEY", "os-secret-value"),
            ("STELLA_EMBED_URL", "http://127.0.0.1:11434/v1"),
        ]);
        let debug = format!("{env:?}");
        assert!(!debug.contains("sk-secret-value"), "{debug}");
        assert!(!debug.contains("vk-secret-value"), "{debug}");
        assert!(!debug.contains("os-secret-value"), "{debug}");
        assert!(debug.contains("redacted"), "{debug}");
        // Presence is not the secret, and it is the thing being debugged.
        assert!(debug.contains("http://127.0.0.1:11434/v1"), "{debug}");
    }

    /// #3713, the transitive half: `Resolution` keeps a derived `Debug`, so
    /// the embedder's own impl is what stops the key travelling with it.
    #[test]
    fn debug_never_prints_the_key_a_resolution_carries() {
        let resolution = resolve(&env_with(&[("OPENAI_API_KEY", "sk-secret-value")]));
        let debug = format!("{resolution:?}");
        assert!(!debug.contains("sk-secret-value"), "{debug}");
        assert!(debug.contains("redacted"), "{debug}");
    }

    /// #3714: `parse::<f32>` accepts `nan` and `inf`, so the `is_finite`
    /// guard is the only thing rejecting them. Nothing exercised it.
    #[test]
    fn a_malformed_floor_is_rejected_rather_than_swallowed() {
        for raw in ["not-a-number", "nan", "inf", "-inf"] {
            let env = env_with(&[("STELLA_EMBED_FLOOR", raw), ("OPENAI_API_KEY", "sk")]);
            let Resolution::Incomplete(message) = resolve(&env) else {
                panic!("expected `{raw}` to be rejected");
            };
            assert!(message.contains("STELLA_EMBED_FLOOR"), "{message}");
            assert!(message.contains(raw), "{message}");
        }
    }

    /// #3714: the floor has to reach the embedder, not just parse. Compared
    /// against `DEFAULT_ADMISSION_FLOOR` so a silently-ignored override fails
    /// here rather than degrading retrieval quality in the field.
    #[test]
    fn a_parsed_floor_reaches_the_embedder() {
        let env = env_with(&[("STELLA_EMBED_FLOOR", "0.6"), ("OPENAI_API_KEY", "sk")]);
        let Resolution::Configured(embedder) = resolve(&env) else {
            panic!("expected a configured embedder");
        };
        let SimilarityPosture::Semantic { admission_floor } = embedder.similarity_posture() else {
            panic!("expected a semantic posture");
        };
        assert_eq!(admission_floor, 0.6);
        assert_ne!(
            admission_floor, DEFAULT_ADMISSION_FLOOR,
            "the override must not collapse to the default"
        );
    }

    #[test]
    fn the_http_backend_declares_a_semantic_posture() {
        let Resolution::Configured(embedder) = resolve(&env_with(&[("OPENAI_API_KEY", "sk")]))
        else {
            panic!("expected a configured embedder");
        };
        assert!(embedder.similarity_posture().admits());
    }

    #[tokio::test]
    async fn a_batch_round_trips_in_input_order_even_when_the_api_reorders_it() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .and(header("authorization", "Bearer sk-test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    { "index": 1, "embedding": [0.0, 1.0] },
                    { "index": 0, "embedding": [1.0, 0.0] },
                ]
            })))
            .mount(&server)
            .await;

        let embedder = HttpEmbedder::new(
            &format!("{}/v1", server.uri()),
            "text-embedding-3-small",
            Some("sk-test".into()),
            2,
            0.25,
        );
        let out = embedder
            .embed(&["first".to_string(), "second".to_string()])
            .await
            .expect("embeds");
        assert_eq!(out[0].vector, vec![1.0, 0.0]);
        assert_eq!(out[1].vector, vec![0.0, 1.0]);
        assert_eq!(out[0].fingerprint, "text-embedding-3-small@1/2/l2");
    }

    #[tokio::test]
    async fn a_wrong_width_is_a_dimension_mismatch_not_a_stored_vector() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [ { "index": 0, "embedding": [1.0, 0.0, 0.0] } ]
            })))
            .mount(&server)
            .await;

        let embedder = HttpEmbedder::new(&format!("{}/v1", server.uri()), "m", None, 2, 0.25);
        assert!(matches!(
            embedder.embed(&["only".to_string()]).await,
            Err(EmbedError::DimensionMismatch {
                expected: 2,
                got: 3
            })
        ));
    }

    #[tokio::test]
    async fn an_http_error_carries_the_status_and_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(ResponseTemplate::new(401).set_body_string("bad key"))
            .mount(&server)
            .await;

        let embedder = HttpEmbedder::new(&format!("{}/v1", server.uri()), "m", None, 2, 0.25);
        let Err(EmbedError::Backend(message)) = embedder.embed(&["x".to_string()]).await else {
            panic!("expected a backend error");
        };
        assert!(message.contains("401"), "{message}");
        assert!(message.contains("bad key"), "{message}");
    }

    /// **The witness for #4144's rate-limit constraint.** The chunk embedding
    /// pass now keeps several requests in flight, which is precisely what
    /// provokes a rate limit — so a `429` has to be an instruction to wait
    /// rather than the end of the pass. Without the retry the first one
    /// aborts everything downstream of it.
    #[tokio::test]
    async fn a_rate_limited_request_is_retried_rather_than_failed() {
        let server = MockServer::start().await;
        // Answered in mount order, each `up_to_n_times` once exhausted
        // falling through to the next: one refusal, then the real answer.
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "0")
                    .set_body_string("slow down"),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [ { "index": 0, "embedding": [1.0, 0.0] } ]
            })))
            .mount(&server)
            .await;

        let embedder = HttpEmbedder::new(&format!("{}/v1", server.uri()), "m", None, 2, 0.25);
        let out = embedder
            .embed(&["only".to_string()])
            .await
            .expect("a 429 must be waited out, not surfaced as a failure");
        assert_eq!(out[0].vector, vec![1.0, 0.0]);
    }

    /// The retry is bounded, and says so. A backend that refuses every
    /// attempt still ends as a named error rather than an unbounded wait —
    /// and the message reports that it was asked more than once, so a user
    /// reading "HTTP 429" is not left thinking the pass never backed off.
    #[tokio::test]
    async fn a_backend_that_only_refuses_ends_as_an_error_naming_the_attempts() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "0")
                    .set_body_string("still limited"),
            )
            .mount(&server)
            .await;

        let embedder = HttpEmbedder::new(&format!("{}/v1", server.uri()), "m", None, 2, 0.25);
        let Err(EmbedError::Backend(message)) = embedder.embed(&["x".to_string()]).await else {
            panic!("expected a backend error once the attempts ran out");
        };
        assert!(message.contains("429"), "{message}");
        assert!(
            message.contains(&format!("{MAX_ATTEMPTS} attempts")),
            "the error must report that it retried: {message}"
        );
    }

    /// A rate limit clears; a bad key does not. Retrying a `401` only delays
    /// an error the caller has to read anyway, so it is issued exactly once.
    #[tokio::test]
    async fn an_unauthorized_request_is_not_retried() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(ResponseTemplate::new(401).set_body_string("bad key"))
            .expect(1)
            .mount(&server)
            .await;

        let embedder = HttpEmbedder::new(&format!("{}/v1", server.uri()), "m", None, 2, 0.25);
        assert!(embedder.embed(&["x".to_string()]).await.is_err());
        // `expect(1)` is verified on drop: a second attempt fails the test.
        drop(server);
    }
}
