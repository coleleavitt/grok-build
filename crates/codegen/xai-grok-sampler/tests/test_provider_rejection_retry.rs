//! Integration coverage for provider-rejected request fields.
//!
//! Anthropic 400s deterministically on parameters a model no longer accepts
//! (`temperature` on newer Claude models) and on JSON Schema keywords its
//! structured-output validator does not implement (`maxItems`). Both are
//! body-only failures, so the Messages path strips the named field and sends
//! once more instead of failing the caller. These tests drive a real HTTP
//! server so the retry is observed on the wire, not mocked at a seam.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use indexmap::IndexMap;
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use xai_grok_sampler::{ApiBackend, AuthScheme, SamplerConfig, SamplingClient};
use xai_grok_sampling_types::MessagesRequestWrapper;
use xai_grok_sampling_types::messages::{
    Message as AnthropicMessage, MessageContent, MessageRole, MessagesRequest, OutputConfig,
    OutputFormat,
};

/// Bodies the server received, in order.
type SeenBodies = Arc<Mutex<Vec<serde_json::Value>>>;

struct MockServer {
    addr: SocketAddr,
    shutdown_tx: oneshot::Sender<()>,
}

impl MockServer {
    async fn spawn(app: Router) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await;
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        Self { addr, shutdown_tx }
    }

    fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    fn shutdown(self) {
        let _ = self.shutdown_tx.send(());
    }
}

const SSE_OK: &str = concat!(
    "event: message_start\n",
    r#"data: {"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"m","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":1,"output_tokens":1}}}"#,
    "\n\n",
    "event: message_stop\n",
    r#"data: {"type":"message_stop"}"#,
    "\n\n",
);

/// Predicate over the received JSON body deciding whether to reject it.
type RejectWhen = fn(&serde_json::Value) -> bool;

/// Reject every body `reject_when` still matches, then serve a real SSE stream.
async fn handler(
    State((seen, reject_when, error_body)): State<(SeenBodies, RejectWhen, &'static str)>,
    body: String,
) -> Response {
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
    let reject = reject_when(&parsed);
    seen.lock().unwrap().push(parsed);
    if reject {
        return (
            StatusCode::BAD_REQUEST,
            [("content-type", "application/json")],
            error_body.to_owned(),
        )
            .into_response();
    }
    (
        StatusCode::OK,
        [("content-type", "text/event-stream")],
        SSE_OK.to_owned(),
    )
        .into_response()
}

fn messages_config(base_url: String, model: &str) -> SamplerConfig {
    SamplerConfig {
        api_key: Some("test-key".into()),
        base_url,
        model: model.into(),
        max_completion_tokens: Some(64),
        temperature: None,
        top_p: None,
        api_backend: ApiBackend::Messages,
        auth_scheme: AuthScheme::Bearer,
        extra_headers: IndexMap::new(),
        context_window: 128_000,
        force_http1: false,
        max_retries: Some(0),
        stream_tool_calls: false,
        idle_timeout_secs: Some(30),
        reasoning_effort: None,
        origin_client: None,
        client_identifier: None,
        deployment_id: None,
        user_id: None,
        client_version: None,
        attribution_callback: None,
        bearer_resolver: None,
        supports_backend_search: false,
        compactions_remaining: None,
        compaction_at_tokens: None,
        doom_loop_recovery: None,
        provider_request_adapter: None,
        advisor_server_model: None,
        header_injector: None,
    }
}

fn request(model: &str) -> MessagesRequest {
    MessagesRequest {
        model: model.to_owned(),
        max_tokens: 64,
        messages: vec![AnthropicMessage {
            role: MessageRole::User,
            content: MessageContent::Text("hello".to_owned()),
        }],
        ..Default::default()
    }
}

/// `` `temperature` is deprecated for this model. `` must cost one retry, not
/// the whole request: the second send drops `temperature` and succeeds.
#[tokio::test]
async fn deprecated_temperature_is_stripped_and_the_request_succeeds() {
    let seen: SeenBodies = Arc::new(Mutex::new(Vec::new()));
    let error_body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"`temperature` is deprecated for this model."}}"#;
    let app = Router::new()
        .route("/v1/messages", post(handler))
        .with_state((
            seen.clone(),
            (|body: &serde_json::Value| body.get("temperature").is_some()) as RejectWhen,
            error_body,
        ));
    let server = MockServer::spawn(app).await;

    let model = "test-model-deprecated-temperature";
    let client = SamplingClient::new(messages_config(server.base_url(), model)).unwrap();

    let mut req = request(model);
    req.temperature = Some(0.0);
    let result = client
        .create_message_stream(MessagesRequestWrapper::new(req))
        .await;
    assert!(
        result.is_ok(),
        "stripping the rejected field must recover the request: {:?}; bodies={:?}",
        result.err(),
        seen.lock().unwrap()
    );

    let bodies = seen.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2, "expected exactly one retry: {bodies:?}");
    assert_eq!(bodies[0]["temperature"], 0.0);
    assert!(
        bodies[1].get("temperature").is_none(),
        "retry must omit the rejected field: {}",
        bodies[1]
    );
    assert_eq!(bodies[1]["max_tokens"], 64, "retry must keep the rest");

    server.shutdown();
}

/// A schema keyword rejected by path must be removed everywhere under that
/// path — including nested schemas — so one retry is enough.
#[tokio::test]
async fn unsupported_schema_keyword_is_stripped_and_the_request_succeeds() {
    let seen: SeenBodies = Arc::new(Mutex::new(Vec::new()));
    let error_body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"output_config.format.schema: For 'array' type, property 'maxItems' is not supported"}}"#;
    let app = Router::new()
        .route("/v1/messages", post(handler))
        .with_state((
            seen.clone(),
            (|body: &serde_json::Value| body.to_string().contains("maxItems")) as RejectWhen,
            error_body,
        ));
    let server = MockServer::spawn(app).await;

    let model = "test-model-unsupported-max-items";
    let client = SamplingClient::new(messages_config(server.base_url(), model)).unwrap();

    let mut req = request(model);
    req.output_config = Some(OutputConfig {
        effort: None,
        format: Some(OutputFormat::JsonSchema {
            schema: json!({
                "type": "object",
                "properties": {
                    "pages": {
                        "type": "array",
                        "maxItems": 12,
                        "items": {
                            "type": "object",
                            "properties": {
                                "sources": { "type": "array", "maxItems": 4 }
                            }
                        }
                    }
                },
                "required": ["pages"]
            }),
        }),
    });

    let result = client
        .create_message_stream(MessagesRequestWrapper::new(req))
        .await;
    assert!(
        result.is_ok(),
        "stripping the rejected schema keyword must recover the request: {:?}",
        result.err()
    );

    let bodies = seen.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2, "expected exactly one retry: {bodies:?}");
    let retried_schema = &bodies[1]["output_config"]["format"]["schema"];
    assert!(
        retried_schema["properties"]["pages"]
            .get("maxItems")
            .is_none(),
        "retry must drop the rejected keyword: {retried_schema}"
    );
    assert!(
        retried_schema["properties"]["pages"]["items"]["properties"]["sources"]
            .get("maxItems")
            .is_none(),
        "nested occurrences must be dropped too: {retried_schema}"
    );
    assert_eq!(
        retried_schema["properties"]["pages"]["type"], "array",
        "the rest of the schema must survive: {retried_schema}"
    );

    server.shutdown();
}

/// A 400 we cannot attribute to a request field must surface as an error after
/// a single attempt — no silent mutation, no retry loop.
#[tokio::test]
async fn unattributable_400_fails_without_retrying() {
    let seen: SeenBodies = Arc::new(Mutex::new(Vec::new()));
    let error_body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"credit balance is too low"}}"#;
    let app = Router::new()
        .route("/v1/messages", post(handler))
        .with_state((
            seen.clone(),
            (|_: &serde_json::Value| true) as RejectWhen,
            error_body,
        ));
    let server = MockServer::spawn(app).await;

    let model = "test-model-unattributable-400";
    let client = SamplingClient::new(messages_config(server.base_url(), model)).unwrap();

    let result = client
        .create_message_stream(MessagesRequestWrapper::new(request(model)))
        .await;
    assert!(result.is_err(), "an unattributable 400 must surface");
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "an unattributable 400 must not be retried"
    );

    server.shutdown();
}
