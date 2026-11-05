//! Session identity sent only to the recorder on this machine.

use reqwest::header::HeaderValue;
use stella_protocol::{ProviderError, SessionContext};

pub(super) fn header(
    base_url: &str,
    context: Option<&SessionContext>,
) -> Result<Option<HeaderValue>, ProviderError> {
    let Some(context) = context else {
        return Ok(None);
    };
    let Ok(url) = reqwest::Url::parse(base_url) else {
        return Ok(None);
    };
    let loopback = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if !loopback {
        return Ok(None);
    }
    let id = &context.session_id;
    if id.is_empty() || id.len() > 256 {
        return Err(ProviderError::Malformed(
            "invalid native session identity".into(),
        ));
    }
    HeaderValue::from_str(id)
        .map(Some)
        .map_err(|_| ProviderError::Malformed("invalid native session identity".into()))
}

pub(super) fn client(
    slot: &std::sync::OnceLock<Result<reqwest::Client, String>>,
    unary: bool,
) -> Result<&reqwest::Client, ProviderError> {
    slot.get_or_init(|| build_client(reqwest::Client::builder(), unary))
        .as_ref()
        .map_err(|error| {
            ProviderError::Terminal(format!("could not open the local model connection: {error}"))
        })
}

fn build_client(builder: reqwest::ClientBuilder, unary: bool) -> Result<reqwest::Client, String> {
    builder
        .connect_timeout(crate::http::CONNECT_TIMEOUT)
        .read_timeout(if unary {
            crate::http::UNARY_READ_TIMEOUT
        } else {
            crate::http::STREAM_IDLE_TIMEOUT
        })
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(id: &str) -> SessionContext {
        SessionContext {
            session_id: id.into(),
        }
    }

    #[test]
    fn native_ids_are_exact_and_remote_providers_receive_none() {
        let session = context("ses-1787342320630-36613");
        for url in [
            "http://127.0.0.1:4319/stella",
            "http://[::1]:4319/stella",
            "http://localhost:4319",
        ] {
            assert_eq!(
                header(url, Some(&session)).unwrap().unwrap().to_str().unwrap(),
                session.session_id.as_str()
            );
        }
        for url in [
            "https://api.anthropic.com",
            "https://localhost.attacker.test",
            "https://127.0.0.1.attacker.test",
        ] {
            assert!(header(url, Some(&session)).unwrap().is_none());
        }
    }

    #[test]
    fn requests_keep_their_own_identity_and_invalid_ids_fail() {
        let first = context("ses-first");
        let second = context("ses-second");
        let url = "http://127.0.0.1:4319/stella";
        assert_eq!(header(url, Some(&first)).unwrap().unwrap(), "ses-first");
        assert_eq!(header(url, Some(&second)).unwrap().unwrap(), "ses-second");
        assert_eq!(header(url, Some(&first)).unwrap().unwrap(), "ses-first");
        assert!(header(url, None).unwrap().is_none());
        for id in ["", "x\r\ny", &"x".repeat(257)] {
            assert!(header(url, Some(&context(id))).is_err());
        }
    }
}

#[cfg(test)]
mod dispatch_tests {
    use super::*;
    use crate::anthropic::AnthropicProvider;
    use crate::credential::ApiKey;
    use crate::provider::Provider;
    use stella_protocol::CompletionRequest;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn request(id: &str) -> CompletionRequest {
        CompletionRequest {
            context: Some(SessionContext {
                session_id: id.into(),
            }),
            ..CompletionRequest::default()
        }
    }

    #[tokio::test]
    async fn shared_provider_dispatches_each_request_identity_on_both_paths() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(body_string_contains("\"stream\":true"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("", "text/event-stream"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(body_string_contains("\"stream\":false"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "content": [{"type": "text", "text": "done"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 1, "output_tokens": 1}
            })))
            .mount(&server)
            .await;
        let provider = AnthropicProvider::new(ApiKey::new("test"), "claude-fable-5")
            .with_base_url(server.uri());
        let _ = provider.complete(request("ses-stream")).await;
        let first = request("ses-first");
        let second = request("ses-second");
        let (a, b) = tokio::join!(
            provider.complete_unary_attempt(first.as_borrowed()),
            provider.complete_unary_attempt(second.as_borrowed()),
        );
        assert!(a.is_ok());
        assert!(b.is_ok());
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 3);
        let mut ids: Vec<_> = requests
            .iter()
            .map(|request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                assert!(body.get("context").is_none());
                request.headers.get("x-oxagen-session").unwrap().to_str().unwrap().to_owned()
            })
            .collect();
        ids.sort();
        assert_eq!(ids, ["ses-first", "ses-second", "ses-stream"]);
    }
    #[tokio::test]
    async fn a_redirect_cannot_forward_native_identity_to_another_endpoint() {
        let source = MockServer::start().await;
        let destination = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(307)
                .insert_header("location", format!("{}/v1/messages", destination.uri())))
            .mount(&source)
            .await;
        let provider = AnthropicProvider::new(ApiKey::new("test"), "claude-fable-5")
            .with_base_url(source.uri());
        let req = request("ses-private");
        assert!(provider.complete_unary_attempt(req.as_borrowed()).await.is_err());
        assert!(destination.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn local_identity_requests_ignore_configured_proxies() {
        let proxy = MockServer::start().await;
        let destination = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&destination)
            .await;
        let builder = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(proxy.uri()).unwrap());
        let client = build_client(builder, true).unwrap();
        let response = client.post(destination.uri())
            .header("x-oxagen-session", "ses-private")
            .send().await.unwrap();
        assert!(response.status().is_success());
        assert_eq!(destination.received_requests().await.unwrap().len(), 1);
        assert!(proxy.received_requests().await.unwrap().is_empty());
    }

}
