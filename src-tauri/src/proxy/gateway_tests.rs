//! Loopback HTTP tests: exercise the real server, route selection, adapters and
//! accounting, without real API keys, external upstreams or CLI config writes.

use super::{server::ProxyServer, ProxyConfig};
use crate::database::{Database, HardState, NewApiEndpoint, NewApiKey, UpstreamType};
use axum::{
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

struct Upstream {
    url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Upstream {
    async fn start(router: Router) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self { url, task }
    }
}

impl Drop for Upstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn endpoint(
    db: &Database,
    upstream: UpstreamType,
    url: &str,
    models: Vec<String>,
    priority: i64,
) -> String {
    db.create_api_endpoint(&NewApiEndpoint {
        name: "loopback".into(),
        upstream_type: upstream,
        base_url: url.into(),
        models,
        priority,
        notes: None,
    })
    .unwrap()
}

fn key(db: &Database, endpoint_id: &str, secret: &str, priority: i64) -> String {
    db.create_api_key(&NewApiKey {
        endpoint_id: endpoint_id.into(),
        api_key: secret.into(),
        name: None,
        internal_priority: priority,
    })
    .unwrap()
}

async fn gateway(db: Arc<Database>, app: &str) -> (ProxyServer, String) {
    super::http_client::init(None).unwrap();
    let mut config = db.get_proxy_config_for_app(app).await.unwrap();
    config.auto_failover_enabled = false;
    config.max_retries = 3;
    config.non_streaming_timeout = 3;
    config.streaming_first_byte_timeout = 3;
    db.update_proxy_config_for_app(config).await.unwrap();
    let server = ProxyServer::new(
        ProxyConfig {
            listen_port: 0,
            enable_logging: false,
            ..Default::default()
        },
        db,
        None,
    );
    let info = server.start().await.unwrap();
    let url = format!("http://127.0.0.1:{}", info.port);
    (server, url)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .unwrap()
}

fn claude_success() -> Value {
    json!({"id":"msg_test", "type":"message", "role":"assistant", "model":"claude-test",
        "content":[{"type":"text", "text":"pong"}], "stop_reason":"end_turn",
        "usage":{"input_tokens":1,"output_tokens":1}})
}

fn chat_success() -> Value {
    json!({"id":"chatcmpl_test","object":"chat.completion","created":0,"model":"test-model",
        "choices":[{"index":0,"message":{"role":"assistant","content":"pong"},"finish_reason":"stop"}],
        "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}})
}

fn credential(headers: &HeaderMap) -> String {
    headers
        .get("authorization")
        .or_else(|| headers.get("x-api-key"))
        .or_else(|| headers.get("x-goog-api-key"))
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .trim_start_matches("Bearer ")
        .into()
}

#[tokio::test]
#[serial_test::serial]
async fn gateway_http_rotates_keys_and_fails_over_with_legacy_failover_disabled() {
    let observed = Arc::new(Mutex::new(Vec::<String>::new()));
    let calls = observed.clone();
    let upstream = Upstream::start(Router::new().route(
        "/v1/messages",
        post(move |headers: HeaderMap| {
            let secret = credential(&headers);
            calls.lock().unwrap().push(secret.clone());
            async move {
                if secret == "sk-invalid" {
                    (
                        StatusCode::UNAUTHORIZED,
                        Json(json!({"error":{"message":"invalid API key"}})),
                    )
                        .into_response()
                } else {
                    Json(claude_success()).into_response()
                }
            }
        }),
    ))
    .await;
    let db = Arc::new(Database::memory().unwrap());
    let ep = endpoint(&db, UpstreamType::Claude, &upstream.url, vec![], 100);
    let invalid = key(&db, &ep, "sk-invalid", 0);
    key(&db, &ep, "sk-good-a", 50);
    key(&db, &ep, "sk-good-b", 50);
    let (server, url) = gateway(db.clone(), "claude").await;
    for _ in 0..3 {
        let response = client().post(format!("{url}/v1/messages")).json(&json!({
            "model":"claude-test", "max_tokens":16, "messages":[{"role":"user","content":"ping"}]
        })).send().await.unwrap();
        let status = response.status();
        let body = response.text().await.unwrap();
        assert!(status.is_success(), "{status}: {body}");
        assert!(body.contains("pong"));
    }
    let calls = observed.lock().unwrap().clone();
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[0], "sk-invalid");
    assert_ne!(calls[1], calls[2]);
    assert_eq!(calls[1], calls[3]);
    let keys = db.list_api_keys(&ep).unwrap();
    assert_eq!(
        keys.iter()
            .find(|key| key.id == invalid)
            .unwrap()
            .hard_state,
        Some(HardState::AuthInvalid)
    );
    assert_eq!(keys.iter().map(|key| key.success_count).sum::<i64>(), 3);
    assert_eq!(server.get_status().await.failover_count, 0);
    assert!(db.get_all_providers("claude").unwrap().is_empty());
    server.stop().await.unwrap();
}

#[tokio::test]
#[serial_test::serial]
async fn gateway_http_openai_and_deepseek_support_chat_and_responses_clients() {
    for protocol in [UpstreamType::Openai, UpstreamType::Deepseek] {
        let observed = Arc::new(Mutex::new(Vec::<Value>::new()));
        let calls = observed.clone();
        let upstream = Upstream::start(Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
            calls.lock().unwrap().push(body);
            async { Json(json!({"id":"chatcmpl_test","object":"chat.completion","created":0,"model":"deepseek-chat",
                "choices":[{"index":0,"message":{"role":"assistant","content":"pong"},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}})) }
        }))).await;
        let db = Arc::new(Database::memory().unwrap());
        let ep = endpoint(&db, protocol, &format!("{}/v1", upstream.url), vec![], 100);
        key(&db, &ep, "sk-chat", 50);
        let (server, url) = gateway(db.clone(), "codex").await;
        for (path, body) in [
            (
                "/v1/chat/completions",
                json!({"model":"deepseek-chat","messages":[{"role":"user","content":"ping"}],"stream":false}),
            ),
            (
                "/v1/responses",
                json!({"model":"deepseek-chat","input":"ping","stream":false}),
            ),
        ] {
            let response = client()
                .post(format!("{url}{path}"))
                .json(&body)
                .send()
                .await
                .unwrap();
            let status = response.status();
            let text = response.text().await.unwrap();
            assert!(status.is_success(), "{protocol:?} {path}: {status}: {text}");
            assert!(text.contains("pong"));
        }
        let calls = observed.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert!(calls[1]["messages"].is_array());
        drop(calls);
        assert_eq!(db.list_api_keys(&ep).unwrap()[0].success_count, 2);
        server.stop().await.unwrap();
    }
}

#[tokio::test]
#[serial_test::serial]
async fn gateway_http_gemini_uses_uri_model_before_selecting_key() {
    let observed = Arc::new(Mutex::new(Vec::<String>::new()));
    let calls = observed.clone();
    let upstream = Upstream::start(Router::new().fallback(move |headers: HeaderMap| {
        calls.lock().unwrap().push(credential(&headers));
        async { Json(json!({"candidates":[{"content":{"role":"model","parts":[{"text":"pong"}]},"finishReason":"STOP"}],
            "usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":1,"totalTokenCount":2}})) }
    })).await;
    let db = Arc::new(Database::memory().unwrap());
    let wrong = endpoint(
        &db,
        UpstreamType::Gemini,
        &upstream.url,
        vec!["model-a".into()],
        0,
    );
    key(&db, &wrong, "wrong-model-key", 0);
    let right = endpoint(
        &db,
        UpstreamType::Gemini,
        &upstream.url,
        vec!["model-b".into()],
        100,
    );
    key(&db, &right, "right-model-key", 50);
    let (server, url) = gateway(db, "gemini").await;
    let response = client()
        .post(format!("{url}/v1beta/models/model-b:generateContent"))
        .json(&json!({"contents":[{"role":"user","parts":[{"text":"ping"}]}]}))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert!(status.is_success(), "{status}: {body}");
    assert_eq!(*observed.lock().unwrap(), vec!["right-model-key"]);
    // Model-restricted endpoints must still be usable for catalog discovery,
    // where the URI deliberately has no models/<model-id> segment.
    let catalog = client()
        .get(format!("{url}/v1beta/models"))
        .send()
        .await
        .unwrap();
    assert!(catalog.status().is_success());
    server.stop().await.unwrap();
}

#[tokio::test]
#[serial_test::serial]
async fn gateway_http_exhausted_keys_do_not_fall_back_to_legacy_account() {
    let db = Arc::new(Database::memory().unwrap());
    let legacy = crate::provider::Provider::with_id(
        "legacy".into(),
        "legacy".into(),
        json!({
            "env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:9","ANTHROPIC_AUTH_TOKEN":"legacy-secret"}
        }),
        None,
    );
    db.save_provider("claude", &legacy).unwrap();
    db.set_current_provider("claude", "legacy").unwrap();
    let ep = endpoint(&db, UpstreamType::Claude, "http://127.0.0.1:9", vec![], 100);
    let id = key(&db, &ep, "sk-invalid", 50);
    db.record_api_key_failure(&id, "invalid", None, Some(HardState::AuthInvalid))
        .unwrap();
    let (server, url) = gateway(db, "claude").await;
    let response = client()
        .post(format!("{url}/v1/messages"))
        .json(&json!({
            "model":"claude-test", "max_tokens":16,"messages":[{"role":"user","content":"ping"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    // Context creation failed before forwarding: no legacy account was attempted.
    assert_eq!(server.get_status().await.total_requests, 0);
    server.stop().await.unwrap();
}

#[tokio::test]
#[serial_test::serial]
async fn gateway_http_honors_retry_after_from_upstream_headers() {
    let upstream = Upstream::start(Router::new().route(
        "/v1/messages",
        post(|headers: HeaderMap| async move {
            if credential(&headers) == "sk-limited" {
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    [("retry-after", "2")],
                    Json(json!({"error":"rate limited"})),
                )
                    .into_response()
            } else {
                Json(claude_success()).into_response()
            }
        }),
    ))
    .await;
    let db = Arc::new(Database::memory().unwrap());
    let ep = endpoint(&db, UpstreamType::Claude, &upstream.url, vec![], 100);
    let limited = key(&db, &ep, "sk-limited", 0);
    key(&db, &ep, "sk-good", 50);
    let (server, url) = gateway(db.clone(), "claude").await;
    let response = client()
        .post(format!("{url}/v1/messages"))
        .json(&json!({
            "model":"claude-test","max_tokens":16,"messages":[{"role":"user","content":"ping"}]
        }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let keys = db.list_api_keys(&ep).unwrap();
    let cooldown = keys
        .iter()
        .find(|key| key.id == limited)
        .unwrap()
        .cooldown_until
        .unwrap();
    assert!((0..=2).contains(&(cooldown - chrono::Utc::now().timestamp())));
    server.stop().await.unwrap();
}

#[tokio::test]
#[serial_test::serial]
async fn gateway_http_media_retry_and_streaming_keep_gateway_accounting() {
    const SSE: &str = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_stream\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-test\",\"content\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"pong\"}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    let upstream = Upstream::start(Router::new().route(
        "/v1/messages",
        post(|Json(body): Json<Value>| async move {
            if super::media_sanitizer::contains_image_blocks(&body) {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error":{"message":"This model does not support image input"}})),
                )
                    .into_response()
            } else if body["stream"] == true {
                ([("content-type", "text/event-stream")], SSE).into_response()
            } else {
                Json(claude_success()).into_response()
            }
        }),
    ))
    .await;
    let db = Arc::new(Database::memory().unwrap());
    let mut rectifier = db.get_rectifier_config().unwrap();
    rectifier.enabled = true;
    rectifier.request_media_fallback = true;
    rectifier.request_media_heuristic = false;
    db.set_rectifier_config(&rectifier).unwrap();
    let ep = endpoint(&db, UpstreamType::Claude, &upstream.url, vec![], 100);
    key(&db, &ep, "sk-media", 50);
    let (server, url) = gateway(db.clone(), "claude").await;
    for body in [
        json!({"model":"claude-test","max_tokens":16,"messages":[{"role":"user","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"AAAA"}},{"type":"text","text":"ping"}]}]}),
        json!({"model":"claude-test","max_tokens":16,"stream":true,"messages":[{"role":"user","content":"ping"}]}),
    ] {
        let response = client()
            .post(format!("{url}/v1/messages"))
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let text = response.text().await.unwrap();
        assert!(status.is_success(), "{status}: {text}");
        assert!(text.contains("pong"));
    }
    let keys = db.list_api_keys(&ep).unwrap();
    assert_eq!(keys[0].request_count, 2);
    assert_eq!(keys[0].success_count, 2);
    assert_eq!(server.get_status().await.failover_count, 0);
    server.stop().await.unwrap();
}

#[tokio::test]
#[serial_test::serial]
async fn gateway_http_pool_exit_feedback_and_socks_remote_dns() {
    use crate::proxy_pool::{
        types::{PoolConfig, SubscriptionSource},
        ProxyPoolService,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let reject = Arc::new(AtomicBool::new(false));
    let flag = reject.clone();
    let exit = Upstream::start(Router::new().fallback(move || {
        let rejected = flag.load(Ordering::Relaxed);
        async move {
            if rejected {
                StatusCode::PROXY_AUTHENTICATION_REQUIRED.into_response()
            } else {
                Json(chat_success()).into_response()
            }
        }
    }))
    .await;
    let db = Arc::new(Database::memory().unwrap());
    let pool = ProxyPoolService::new(db.clone());
    let subscription = pool
        .add_subscription(
            "HTTP exit".into(),
            SubscriptionSource::Inline,
            String::new(),
            exit.url.clone(),
            0,
        )
        .await
        .unwrap();
    let config = PoolConfig {
        enabled: true,
        probe_interval_secs: 0,
        max_consecutive_failures: 1,
        ..Default::default()
    };
    pool.set_config(config.clone()).await.unwrap();
    crate::proxy_pool::init_global(pool.clone());
    let ep = endpoint(
        &db,
        UpstreamType::Openai,
        "http://upstream.invalid/v1",
        vec![],
        100,
    );
    let id = key(&db, &ep, "sk-pool", 50);
    let (server, url) = gateway(db.clone(), "codex").await;
    let body = json!({"model":"test-model","messages":[{"role":"user","content":"ping"}]});
    let first = client()
        .post(format!("{url}/v1/chat/completions"))
        .json(&body)
        .send()
        .await;
    reject.store(true, Ordering::Relaxed);
    let second = client()
        .post(format!("{url}/v1/chat/completions"))
        .json(&body)
        .send()
        .await;
    let health = pool.node_views().await[0].health.clone();
    pool.set_config(PoolConfig {
        enabled: false,
        ..config.clone()
    })
    .await
    .unwrap();
    assert!(first.unwrap().status().is_success());
    assert!(!second.unwrap().status().is_success());
    assert!(health.latency_ewma_ms.is_some());
    assert!(health.is_circuit_open());

    pool.delete_subscription(&subscription.subscription_id)
        .await
        .unwrap();
    db.clear_api_key_penalty(&id).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let resolved = Arc::new(Mutex::new(String::new()));
    let hostname = resolved.clone();
    let socks = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut greeting = [0; 2];
        socket.read_exact(&mut greeting).await.unwrap();
        let mut methods = vec![0; greeting[1] as usize];
        socket.read_exact(&mut methods).await.unwrap();
        socket.write_all(&[5, 0]).await.unwrap();
        let mut request = [0; 5];
        socket.read_exact(&mut request).await.unwrap();
        assert_eq!(request[3], 3, "upstream hostname must be resolved by SOCKS");
        let mut domain = vec![0; request[4] as usize];
        socket.read_exact(&mut domain).await.unwrap();
        *hostname.lock().unwrap() = String::from_utf8(domain).unwrap();
        let mut port = [0; 2];
        socket.read_exact(&mut port).await.unwrap();
        socket
            .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 80])
            .await
            .unwrap();
        let mut received = Vec::new();
        loop {
            let mut chunk = [0; 4096];
            let count = socket.read(&mut chunk).await.unwrap();
            assert!(count > 0);
            received.extend_from_slice(&chunk[..count]);
            if let Some(end) = received.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&received[..end]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                            .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if received.len() >= end + 4 + length {
                    break;
                }
            }
            assert!(received.len() < 16384);
        }
        let body = chat_success().to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    });
    pool.add_subscription(
        "SOCKS exit".into(),
        SubscriptionSource::Inline,
        String::new(),
        format!("socks5h://{address}"),
        0,
    )
    .await
    .unwrap();
    pool.set_config(config.clone()).await.unwrap();
    let third = client()
        .post(format!("{url}/v1/chat/completions"))
        .json(&body)
        .send()
        .await;
    pool.set_config(PoolConfig {
        enabled: false,
        ..config
    })
    .await
    .unwrap();
    server.stop().await.unwrap();
    let response = third.unwrap();
    let status = response.status();
    let text = response.text().await.unwrap();
    socks.abort();
    assert!(status.is_success(), "{status}: {text}");
    assert_eq!(*resolved.lock().unwrap(), "upstream.invalid");
}
