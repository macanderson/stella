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

use std::time::Duration;

use async_trait::async_trait;

use crate::hash::l2_normalize;
use crate::seam::{EmbedError, Embedder, EmbedderFingerprint, Embedding, SimilarityPosture};

/// How long one embedding request may take before it is abandoned. Generous
/// because a cold local model server can take seconds to load weights, and
/// bounded because this sits on a tool call the agent is waiting on.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The admission floor applied when nothing overrides it.
///
/// **This number is provisional and is not a measured separation point.** The
/// [`SimilarityPosture`] contract asks for a floor derived from a model's
/// observed relevant/irrelevant score distributions, and no such measurement
/// exists yet for these backends on a code corpus. It is deliberately
/// permissive: its job today is to drop the obviously-unrelated tail from an
/// ordered list, not to certify anything. Measuring it per model is tracked
/// work — see the crate README.
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

/// The environment [`resolve`] reads, captured as data so resolution is a pure
/// function and its table of precedences is testable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    pub fn from_process() -> Self {
        let read = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        Self {
            url: read("STELLA_EMBED_URL"),
            model: read("STELLA_EMBED_MODEL"),
            api_key: read("STELLA_EMBED_API_KEY"),
            dims: read("STELLA_EMBED_DIMS"),
            floor: read("STELLA_EMBED_FLOOR"),
            voyage_api_key: read("VOYAGE_API_KEY"),
            openai_api_key: read("OPENAI_API_KEY"),
        }
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
#[derive(Debug)]
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

        let mut request = self.client.post(&self.endpoint).json(&serde_json::json!({
            "model": self.model,
            "input": texts,
        }));
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }

        let response = request
            .send()
            .await
            .map_err(|error| EmbedError::Backend(format!("{}: {error}", self.endpoint)))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let body: String = body.chars().take(400).collect();
            return Err(EmbedError::Backend(format!(
                "{} returned HTTP {status}: {body}",
                self.endpoint
            )));
        }

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
    /// corpus, and measuring one per model is tracked in #2993.
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
        let mut env = EmbedderEnv::default();
        for (key, value) in pairs {
            let slot = match *key {
                "STELLA_EMBED_URL" => &mut env.url,
                "STELLA_EMBED_MODEL" => &mut env.model,
                "STELLA_EMBED_API_KEY" => &mut env.api_key,
                "STELLA_EMBED_DIMS" => &mut env.dims,
                "STELLA_EMBED_FLOOR" => &mut env.floor,
                "VOYAGE_API_KEY" => &mut env.voyage_api_key,
                "OPENAI_API_KEY" => &mut env.openai_api_key,
                other => panic!("unknown variable {other}"),
            };
            *slot = Some((*value).to_string());
        }
        env
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
}
