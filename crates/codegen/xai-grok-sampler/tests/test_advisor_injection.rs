//! G002 adversarial QA: end-to-end proof that BOTH the non-streaming
//! (`create_message` / `conversation_messages`) and the streaming
//! (`create_message_stream` / `conversation_stream_messages`) Anthropic
//! Messages code paths inject the server-side advisor tool + beta header
//! under the active gate -- not just one path, and not just at the
//! `inject_messages_extra_tools` unit level (see `client.rs`'s
//! `advisor_probe`-based matrix for that). This drives the real public
//! `SamplingClient` methods against a real (mock) HTTP server and inspects
//! the literal bytes that left the process.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::routing::post;
use futures_util::stream::{self, StreamExt};
use indexmap::IndexMap;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use xai_grok_sampler::{ApiBackend, AuthScheme, BearerResolver, SamplerConfig, SamplingClient};
use xai_grok_sampling_types::{
    ConversationItem, ConversationRequest, ProviderRequestAdapter, UserItem,
};

// ---------------------------------------------------------------------------
// Mock server harness (mirrors tests/test_actor.rs's MockServer)
// ---------------------------------------------------------------------------

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

#[derive(Debug)]
struct StaticBearer(&'static str);

impl BearerResolver for StaticBearer {
    fn current_bearer(&self) -> Option<String> {
        Some(self.0.to_string())
    }
}

/// Gate-active config: Messages backend, Bearer auth, an Anthropic adapter,
/// and `advisor_server_model` set -- `advisor_gate_active()`'s AND is fully
/// satisfied.
fn gate_active_config(base_url: String, model: &str) -> SamplerConfig {
    SamplerConfig {
        api_key: None,
        base_url,
        model: model.into(),
        max_completion_tokens: Some(256),
        temperature: None,
        top_p: None,
        api_backend: ApiBackend::Messages,
        auth_scheme: AuthScheme::Bearer,
        extra_headers: IndexMap::new(),
        query_params: IndexMap::new(),
        env_http_headers: IndexMap::new(),
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
        bearer_resolver: Some(Arc::new(StaticBearer(
            "sk-ant-oat01-abcdefghijklmnopqrstuvwxyz012345",
        ))),
        supports_backend_search: false,
        compactions_remaining: None,
        compaction_at_tokens: None,
        doom_loop_recovery: None,
        provider_request_adapter: Some(ProviderRequestAdapter::Anthropic {
            tool_name_prefix: "mcp__".to_string(),
            command: None,
        }),
        advisor_server_model: Some(model.to_string()),
        header_injector: None,
    }
}

fn user_request(text: &str) -> ConversationRequest {
    ConversationRequest {
        items: vec![ConversationItem::User(UserItem {
            content: vec![xai_grok_sampling_types::ContentPart::Text {
                text: std::sync::Arc::<str>::from(text),
            }],
            synthetic_reason: None,
            ..Default::default()
        })],
        ..Default::default()
    }
}

fn advisor_tool_present(body: &Value) -> bool {
    body.get("tools")
        .and_then(|t| t.as_array())
        .map(|tools| {
            tools.iter().any(|t| {
                t.get("type").and_then(|v| v.as_str()) == Some("advisor_20260301")
                    && t.get("name").and_then(|v| v.as_str()) == Some("advisor")
            })
        })
        .unwrap_or(false)
}

fn advisor_beta_present(headers: &HeaderMap) -> bool {
    headers
        .get("anthropic-beta")
        .and_then(|v| v.to_str().ok())
        .map(|betas| betas.contains("advisor-tool-2026-03-01"))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Case 1: streaming path injects too, not just non-streaming
// ---------------------------------------------------------------------------

/// Non-streaming `create_message` (via `conversation_messages`), gate active:
/// the raw request body that actually left the process contains the advisor
/// tool and the outgoing headers carry the advisor beta.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_streaming_create_message_injects_advisor_tool_on_the_wire() {
    let captured: Arc<Mutex<Option<(Value, HeaderMap)>>> = Arc::new(Mutex::new(None));
    let captured_handler = captured.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(
            move |headers: HeaderMap, axum::Json(body): axum::Json<Value>| {
                let captured = captured_handler.clone();
                async move {
                    *captured.lock().unwrap() = Some((body, headers));
                    (
                        StatusCode::OK,
                        json!({
                            "id": "msg_1",
                            "type": "message",
                            "role": "assistant",
                            "content": [{"type": "text", "text": "hi"}],
                            "model": "claude-opus-5",
                            "stop_reason": "end_turn",
                            "usage": {"input_tokens": 1, "output_tokens": 1}
                        })
                        .to_string(),
                    )
                }
            },
        ),
    );
    let server = MockServer::spawn(app).await;
    let client = SamplingClient::new(gate_active_config(server.base_url(), "claude-opus-5"))
        .expect("client should build");

    let result = client.conversation_messages(user_request("hi")).await;
    server.shutdown();
    result.expect("non-streaming request must succeed");

    let (body, headers) = captured
        .lock()
        .unwrap()
        .take()
        .expect("request must be captured");
    assert!(
        advisor_tool_present(&body),
        "non-streaming create_message must inject the advisor tool into the wire body: {body}"
    );
    assert!(
        advisor_beta_present(&headers),
        "non-streaming create_message must carry the advisor beta header"
    );
}

/// Streaming `create_message_stream` (via `conversation_stream_messages`),
/// gate active: the raw request body that actually left the process for the
/// STREAMING path also contains the advisor tool and beta header. This is
/// the case-1 adversarial check -- the streaming path must not silently skip
/// injection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_create_message_stream_injects_advisor_tool_on_the_wire() {
    let captured: Arc<Mutex<Option<(Value, HeaderMap)>>> = Arc::new(Mutex::new(None));
    let captured_handler = captured.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |headers: HeaderMap, axum::Json(body): axum::Json<Value>| {
            let captured = captured_handler.clone();
            async move {
                *captured.lock().unwrap() = Some((body, headers));
                let events: Vec<Event> = vec![
                    Event::default().event("message_start").data(
                        json!({
                            "type": "message_start",
                            "message": {
                                "id": "msg_1", "type": "message", "role": "assistant",
                                "content": [], "model": "claude-opus-5",
                                "stop_reason": null, "usage": {"input_tokens": 1, "output_tokens": 0}
                            }
                        })
                        .to_string(),
                    ),
                    Event::default().event("content_block_start").data(
                        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}})
                            .to_string(),
                    ),
                    Event::default().event("content_block_delta").data(
                        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "hi"}})
                            .to_string(),
                    ),
                    Event::default().event("content_block_stop").data(
                        json!({"type": "content_block_stop", "index": 0}).to_string(),
                    ),
                    Event::default().event("message_delta").data(
                        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 1}})
                            .to_string(),
                    ),
                    Event::default().event("message_stop").data(
                        json!({"type": "message_stop"}).to_string(),
                    ),
                ];
                Sse::new(stream::iter(events.into_iter().map(Ok::<_, std::convert::Infallible>)))
            }
        }),
    );
    let server = MockServer::spawn(app).await;
    let client = SamplingClient::new(gate_active_config(server.base_url(), "claude-opus-5"))
        .expect("client should build");

    let result = client
        .conversation_stream_messages(user_request("hi"))
        .await;
    let (stream, _meta) = result.expect("streaming request must succeed");
    // Drain the stream to completion so the request has fully round-tripped.
    let events: Vec<_> = stream.collect().await;
    assert!(!events.is_empty(), "stream must yield events");
    server.shutdown();

    let (body, headers) = captured
        .lock()
        .unwrap()
        .take()
        .expect("request must be captured");
    assert!(
        advisor_tool_present(&body),
        "streaming create_message_stream must inject the advisor tool into the wire body: {body}"
    );
    assert!(
        advisor_beta_present(&headers),
        "streaming create_message_stream must carry the advisor beta header"
    );
}
