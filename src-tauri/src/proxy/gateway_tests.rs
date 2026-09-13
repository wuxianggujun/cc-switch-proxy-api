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
    let id = db
        .create_api_key(&NewApiKey {
            endpoint_id: endpoint_id.into(),
            api_key: secret.into(),
            name: None,
            internal_priority: priority,
        })
        .unwrap();
    db.set_api_endpoint_enabled(endpoint_id, true).unwrap();
    id
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

#[tokio::test]
#[serial_test::serial]
async fn gateway_http_custom_tools_keep_opaque_input_and_result_identity() {
    const INPUT: &str = r#"{"input":"原始 JSON 工具文本"}"#;
    for protocol in [UpstreamType::Claude, UpstreamType::Codex] {
        let observed = Arc::new(Mutex::new(Vec::<Value>::new()));
        let calls = observed.clone();
        let upstream=Upstream::start(Router::new().route(protocol_path(protocol),post(move |Json(body):Json<Value>| {
            let first={let mut calls=calls.lock().unwrap();calls.push(body);calls.len()==1};
            async move {Json(if !first {protocol_json(protocol)} else if protocol==UpstreamType::Claude {
                json!({"id":"msg_custom","type":"message","model":"test-model","role":"assistant","content":[{
                    "type":"tool_use","id":"call_custom","name":"raw_tool","input":{"input":INPUT}}],"stop_reason":"tool_use","usage":{"input_tokens":1,"output_tokens":2}})
            } else {json!({"id":"resp_custom","object":"response","model":"test-model","status":"completed","output":[{
                "type":"custom_tool_call","call_id":"call_custom","name":"raw_tool","input":INPUT}]})})}
        }))).await;
        let db = Arc::new(Database::memory().unwrap());
        let ep = endpoint(&db, protocol, &upstream.url, vec!["test-model".into()], 100);
        key(&db, &ep, "sk-custom", 50);
        let (server, url) = gateway(db, "codex").await;
        let mut request = json!({"model":"test-model","messages":[{"role":"user","content":"执行工具"}],
            "tools":[{"type":"custom","custom":{"name":"raw_tool","format":{"type":"text"}}}]});
        let response = client()
            .post(format!("{url}/v1/chat/completions"))
            .json(&request)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let reply: Value = response.json().await.unwrap();
        assert_eq!(status, StatusCode::OK, "{protocol:?}: {reply}");
        let message = &reply["choices"][0]["message"];
        assert_eq!(
            message["tool_calls"][0]["type"], "custom",
            "{protocol:?}: {reply}"
        );
        assert_eq!(
            message["tool_calls"][0]["custom"]["input"], INPUT,
            "{protocol:?}: {reply}"
        );
        request["messages"] = json!([{"role":"user","content":"执行工具"},message,{"role":"tool","tool_call_id":"call_custom","content":"完成"}]);
        let response = client()
            .post(format!("{url}/v1/chat/completions"))
            .json(&request)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let text = response.text().await.unwrap();
        assert_eq!(status, StatusCode::OK, "{protocol:?}: {text}");
        let calls = observed.lock().unwrap();
        if protocol == UpstreamType::Codex {
            assert!(calls[1]["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["type"] == "custom_tool_call_output"
                    && item["call_id"] == "call_custom"));
        } else {
            assert!(calls[1].to_string().contains("完成"));
        }
        drop(calls);
        server.stop().await.unwrap();
    }
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

fn responses_success() -> Value {
    json!({"id":"resp_test","object":"response","created_at":1,"model":"test-model",
        "status":"completed","output":[{"id":"msg_test","type":"message","role":"assistant",
            "status":"completed","content":[{"type":"output_text","text":"pong","annotations":[]}]}],
        "usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}})
}

#[tokio::test]
#[serial_test::serial]
async fn gateway_http_chat_entry_returns_chat_for_every_upstream_protocol() {
    for protocol in [
        UpstreamType::Claude,
        UpstreamType::Codex,
        UpstreamType::Openai,
    ] {
        let path = match protocol {
            UpstreamType::Claude => "/v1/messages",
            UpstreamType::Codex => "/v1/responses",
            _ => "/v1/chat/completions",
        };
        let observed = Arc::new(Mutex::new(Vec::<Value>::new()));
        let calls = observed.clone();
        let upstream = Upstream::start(Router::new().route(
            path,
            post(move |Json(body): Json<Value>| {
                calls.lock().unwrap().push(body);
                async move {
                    Json(match protocol {
                        UpstreamType::Claude => claude_success(),
                        UpstreamType::Codex => responses_success(),
                        _ => chat_success(),
                    })
                }
            }),
        ))
        .await;
        let db = Arc::new(Database::memory().unwrap());
        let ep = endpoint(&db, protocol, &upstream.url, vec!["test-model".into()], 100);
        key(&db, &ep, "sk-fixture", 50);
        let (server, url) = gateway(db, "codex").await;
        let response = client()
            .post(format!("{url}/v1/chat/completions"))
            .json(&json!({
                "model":"test-model", "messages":[{"role":"system","content":"保留系统提示词"},
                    {"role":"user","content":"原始中文提示词"}], "max_tokens":64, "stream":false
            }))
            .send()
            .await
            .unwrap();
        let status = response.status();
        let text = response.text().await.unwrap();
        server.stop().await.unwrap();
        assert_eq!(status, StatusCode::OK, "{protocol:?}: {text}");
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["object"], "chat.completion", "{protocol:?}: {text}");
        assert_eq!(value["choices"][0]["message"]["content"], "pong");
        assert_eq!(value["choices"][0]["finish_reason"], "stop");
        let calls = observed.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].to_string().contains("原始中文提示词"));
        assert!(calls[0].to_string().contains("保留系统提示词"));
        if protocol == UpstreamType::Codex {
            assert!(calls[0]["input"].is_array());
            assert!(calls[0].get("messages").is_none());
        } else {
            assert!(calls[0]["messages"].is_array());
        }
    }
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

fn protocol_path(protocol: UpstreamType) -> &'static str {
    match protocol {
        UpstreamType::Claude => "/v1/messages",
        UpstreamType::Codex => "/v1/responses",
        _ => "/v1/chat/completions",
    }
}

fn protocol_json(protocol: UpstreamType) -> Value {
    match protocol {
        UpstreamType::Claude => claude_success(),
        UpstreamType::Codex => responses_success(),
        _ => chat_success(),
    }
}

fn protocol_sse(protocol: UpstreamType) -> String {
    let events = match protocol {
        UpstreamType::Claude => vec![
            json!({"type":"message_start","message":{"id":"msg_test","type":"message","role":"assistant","model":"test-model","content":[],"usage":{"input_tokens":1,"output_tokens":0}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"你好"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}),
            json!({"type":"message_stop"}),
        ],
        UpstreamType::Codex => {
            let mut response = responses_success();
            response["output"][0]["content"][0]["text"] = json!("你好");
            vec![
                json!({"type":"response.created","response":{"id":"resp_test","model":"test-model","status":"in_progress","output":[]}}),
                json!({"type":"response.output_text.delta","item_id":"msg_test","delta":"你好"}),
                json!({"type":"response.completed","response":response}),
            ]
        }
        _ => vec![
            json!({"id":"chat_test","object":"chat.completion.chunk","model":"test-model","choices":[{"index":0,"delta":{"role":"assistant","content":"你好"},"finish_reason":null}]}),
            json!({"id":"chat_test","object":"chat.completion.chunk","model":"test-model","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}}),
        ],
    };
    let mut text = events
        .into_iter()
        .map(|event| match event.get("type").and_then(Value::as_str) {
            Some(kind) => format!("event: {kind}\ndata: {event}\n\n"),
            None => format!("data: {event}\n\n"),
        })
        .collect::<String>();
    if !matches!(protocol, UpstreamType::Claude | UpstreamType::Codex) {
        text.push_str("data: [DONE]\n\n");
    }
    text
}

#[tokio::test]
#[serial_test::serial]
async fn gateway_http_three_protocol_matrix_preserves_json_sse_and_native_auth() {
    for upstream_protocol in [
        UpstreamType::Claude,
        UpstreamType::Codex,
        UpstreamType::Openai,
    ] {
        let observed = Arc::new(Mutex::new(Vec::<(HeaderMap, Value)>::new()));
        let calls = observed.clone();
        let upstream = Upstream::start(Router::new().route(
            protocol_path(upstream_protocol),
            post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let streaming = body["stream"].as_bool().unwrap_or(false);
                calls.lock().unwrap().push((headers, body));
                async move {
                    if streaming {
                        let chunks = protocol_sse(upstream_protocol)
                            .into_bytes()
                            .chunks(5)
                            .map(|bytes| {
                                Ok::<_, std::io::Error>(bytes::Bytes::copy_from_slice(bytes))
                            })
                            .collect::<Vec<_>>();
                        (
                            [
                                ("content-type", "text/event-stream"),
                                ("x-upstream-fixture", "matrix"),
                            ],
                            axum::body::Body::from_stream(futures::stream::iter(chunks)),
                        )
                            .into_response()
                    } else {
                        Json(protocol_json(upstream_protocol)).into_response()
                    }
                }
            }),
        ))
        .await;
        for entry in [
            UpstreamType::Claude,
            UpstreamType::Codex,
            UpstreamType::Openai,
        ] {
            let db = Arc::new(Database::memory().unwrap());
            let ep = endpoint(
                &db,
                upstream_protocol,
                &upstream.url,
                vec!["test-model".into()],
                100,
            );
            key(&db, &ep, "sk-matrix", 50);
            let (server, url) = gateway(
                db,
                if entry == UpstreamType::Claude {
                    "claude"
                } else {
                    "codex"
                },
            )
            .await;
            for streaming in [false, true] {
                let mut body = if entry == UpstreamType::Codex {
                    json!({"input":"原始中文提示词"})
                } else {
                    json!({"messages":[{"role":"user","content":"原始中文提示词"}]})
                };
                body["model"] = json!("test-model");
                body["stream"] = json!(streaming);
                if entry == UpstreamType::Claude {
                    body["max_tokens"] = json!(64);
                }
                let response = client()
                    .post(format!("{url}{}", protocol_path(entry)))
                    .json(&body)
                    .send()
                    .await
                    .unwrap();
                let status = response.status();
                let text = response.text().await.unwrap();
                assert_eq!(
                    status,
                    StatusCode::OK,
                    "{entry:?}->{upstream_protocol:?} stream={streaming}: {text}"
                );
                if streaming {
                    assert!(
                        text.contains("你好"),
                        "{entry:?}->{upstream_protocol:?}: {text}"
                    );
                    let marker = match entry {
                        UpstreamType::Claude => "message_stop",
                        UpstreamType::Codex => "response.completed",
                        _ => "[DONE]",
                    };
                    assert!(
                        text.contains(marker),
                        "{entry:?}->{upstream_protocol:?}: {text}"
                    );
                    if entry == UpstreamType::Openai {
                        assert!(!text.contains("event: message_start"));
                        assert!(!text.contains("event: response.completed"));
                    }
                } else {
                    let value: Value = serde_json::from_str(&text).unwrap();
                    match entry {
                        UpstreamType::Claude => assert_eq!(value["type"], "message"),
                        UpstreamType::Codex => assert_eq!(value["object"], "response"),
                        _ => assert_eq!(value["object"], "chat.completion"),
                    }
                    assert!(text.contains("pong"));
                }
            }
            server.stop().await.unwrap();
        }
        let calls = observed.lock().unwrap();
        assert_eq!(calls.len(), 6);
        for (headers, body) in calls.iter() {
            if upstream_protocol == UpstreamType::Claude {
                assert_eq!(headers.get("x-api-key").unwrap(), "sk-matrix");
                assert_eq!(headers.get("anthropic-version").unwrap(), "2023-06-01");
            } else {
                assert_eq!(headers.get("authorization").unwrap(), "Bearer sk-matrix");
            }
            assert!(body.to_string().contains("原始中文提示词"));
        }
    }
}

async fn completed_trace(db: &Database, id: &str) -> crate::database::RequestTraceDetail {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(detail) = db.get_request_trace(id).unwrap() {
                if detail.summary.state != "in_progress" {
                    break detail;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("trace must finish after response body is consumed")
}

#[tokio::test]
#[serial_test::serial]
async fn gateway_http_request_trace_records_real_ip_raw_body_retries_and_redacted_errors() {
    let upstream=Upstream::start(Router::new().route("/v1/chat/completions",post(|headers:HeaderMap|async move {
        if credential(&headers)=="sk-invalid-sensitive" {
            (StatusCode::UNAUTHORIZED,Json(json!({"error":{"message":"invalid key sk-invalid-sensitive","code":"bad_key"}}))).into_response()
        } else {Json(chat_success()).into_response()}
    }))).await;
    let db = Arc::new(Database::memory().unwrap());
    let ep = endpoint(&db, UpstreamType::Openai, &upstream.url, vec![], 100);
    key(&db, &ep, "sk-invalid-sensitive", 1);
    key(&db, &ep, "sk-working-sensitive", 2);
    let (server, url) = gateway(db.clone(), "codex").await;
    let original="{\n  \"model\": \"test-model\", \"messages\": [{\"role\":\"user\",\"content\":\"精确保留中文提示词\"}]\n}";
    let response = client()
        .post(format!(
            "{url}/v1/chat/completions?api_key=client-query-secret"
        ))
        .header("authorization", "Bearer client-sensitive-token")
        .header("x-forwarded-for", "203.0.113.99")
        .header("content-type", "application/json")
        .body(original)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let id = response.headers()["x-ccswitch-request-id"]
        .to_str()
        .unwrap()
        .to_string();
    response.bytes().await.unwrap();
    let detail = completed_trace(&db, &id).await;
    assert_eq!(detail.summary.client_ip, "127.0.0.1");
    assert_eq!(detail.summary.state, "completed");
    assert_eq!(detail.summary.attempt_count, 2);
    assert_eq!(detail.request.body, original);
    assert_eq!(detail.attempts[0].status_code, Some(401));
    assert!(detail.attempts[0]
        .response
        .as_ref()
        .unwrap()
        .body
        .contains("[REDACTED]"));
    assert_eq!(detail.attempts[1].status_code, Some(200));
    let outbound: Value = serde_json::from_str(&detail.attempts[1].request.body).unwrap();
    assert_eq!(outbound["messages"][0]["content"], "精确保留中文提示词");
    assert!(detail.response.as_ref().unwrap().body.contains("pong"));
    let json = serde_json::to_string(&detail).unwrap();
    for secret in [
        "client-sensitive-token",
        "sk-invalid-sensitive",
        "sk-working-sensitive",
        "client-query-secret",
    ] {
        assert!(!json.contains(secret), "trace leaked {secret}");
    }
    assert!(
        json.contains("203.0.113.99"),
        "untrusted forwarded header should remain inspectable"
    );
    server.stop().await.unwrap();
}

#[tokio::test]
#[serial_test::serial]
async fn gateway_http_request_trace_includes_pre_route_errors_and_can_be_disabled() {
    let db = Arc::new(Database::memory().unwrap());
    let (server, url) = gateway(db.clone(), "codex").await;
    let response = client()
        .post(format!("{url}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body("{invalid 中文")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let id = response.headers()["x-ccswitch-request-id"]
        .to_str()
        .unwrap()
        .to_string();
    response.bytes().await.unwrap();
    let detail = completed_trace(&db, &id).await;
    assert_eq!(detail.summary.state, "error");
    assert!(detail.attempts.is_empty());
    assert_eq!(detail.request.body, "{invalid 中文");
    db.set_request_trace_config(&crate::database::RequestTraceConfig {
        enabled: false,
        ..Default::default()
    })
    .unwrap();
    let response = client()
        .post(format!("{url}/v1/chat/completions"))
        .json(&json!({"model":"missing","messages":[]}))
        .send()
        .await
        .unwrap();
    assert!(!response.headers().contains_key("x-ccswitch-request-id"));
    response.bytes().await.unwrap();
    assert_eq!(
        db.list_request_traces(&Default::default(), 0, 20)
            .unwrap()
            .total,
        1
    );
    server.stop().await.unwrap();
}

#[tokio::test]
#[serial_test::serial]
async fn gateway_http_full_endpoint_urls_and_prefixed_entries_do_not_duplicate_paths() {
    for protocol in [
        UpstreamType::Claude,
        UpstreamType::Openai,
        UpstreamType::Codex,
    ] {
        let upstream = Upstream::start(Router::new().route(
            protocol_path(protocol),
            post(move || async move { Json(protocol_json(protocol)) }),
        ))
        .await;
        let db = Arc::new(Database::memory().unwrap());
        let ep = endpoint(
            &db,
            protocol,
            &format!("{}{}", upstream.url, protocol_path(protocol)),
            vec!["test-model".into()],
            100,
        );
        key(&db, &ep, "sk-full-url", 50);
        let (server, url) = gateway(
            db,
            if protocol == UpstreamType::Claude {
                "claude"
            } else {
                "codex"
            },
        )
        .await;
        let (path, body) = match protocol {
            UpstreamType::Claude => (
                "/claude/v1/messages",
                json!({"model":"test-model","messages":[{"role":"user","content":"hi"}],"max_tokens":64}),
            ),
            UpstreamType::Codex => (
                "/codex/v1/responses",
                json!({"model":"test-model","input":"hi"}),
            ),
            _ => (
                "/codex/v1/chat/completions",
                json!({"model":"test-model","messages":[{"role":"user","content":"hi"}]}),
            ),
        };
        let response = client()
            .post(format!("{url}{path}?client=1"))
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let text = response.text().await.unwrap();
        assert_eq!(status, StatusCode::OK, "{protocol:?}: {text}");
        assert!(text.contains("pong"));
        server.stop().await.unwrap();
    }
}

#[tokio::test]
#[serial_test::serial]
async fn gateway_http_models_returns_standard_list_and_keeps_codex_catalog_shape() {
    let db = Arc::new(Database::memory().unwrap());
    let claude_endpoint = endpoint(
        &db,
        UpstreamType::Claude,
        "http://127.0.0.1",
        vec!["claude-test".into(), "claude-*".into()],
        100,
    );
    key(&db, &claude_endpoint, "sk-claude-models", 50);
    let openai_endpoint = endpoint(
        &db,
        UpstreamType::Openai,
        "http://127.0.0.1",
        vec!["gpt-test".into()],
        100,
    );
    key(&db, &openai_endpoint, "sk-openai-models", 50);
    let (server, url) = gateway(db, "codex").await;
    let response: Value = client()
        .get(format!("{url}/v1/models"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["object"], "list");
    assert!(response["models"].is_array());
    let ids = response["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(ids.contains(&"claude-test"));
    assert!(ids.contains(&"gpt-test"));
    assert!(!ids.contains(&"claude-*"));
    server.stop().await.unwrap();
}

#[tokio::test]
#[serial_test::serial]
async fn gateway_http_chat_handles_json_stream_fallback_and_usage_option() {
    for content_type in ["application/json", "application/octet-stream"] {
        let upstream = Upstream::start(Router::new().route(
            "/v1/responses",
            post(move || async move {
                (
                    [("content-type", content_type)],
                    responses_success().to_string(),
                )
            }),
        ))
        .await;
        let db = Arc::new(Database::memory().unwrap());
        let ep = endpoint(&db, UpstreamType::Codex, &upstream.url, vec![], 100);
        key(&db, &ep, "sk-json", 50);
        let (server, url) = gateway(db, "codex").await;
        for include_usage in [false, true] {
            let response=client().post(format!("{url}/v1/chat/completions")).json(&json!({"model":"test-model",
                "messages":[{"role":"user","content":"hi"}],"stream":true,"stream_options":{"include_usage":include_usage}})).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let text = response.text().await.unwrap();
            assert!(text.contains("pong"), "{content_type}: {text}");
            assert!(text.contains("[DONE]"));
            let events = text
                .lines()
                .filter_map(|line| line.strip_prefix("data: "))
                .filter_map(|data| serde_json::from_str::<Value>(data).ok())
                .collect::<Vec<_>>();
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event.get("usage").is_some())
                    .count(),
                usize::from(include_usage)
            );
            if include_usage {
                assert_eq!(events.last().unwrap()["choices"], json!([]));
            }
        }
        server.stop().await.unwrap();
    }
}

#[tokio::test]
#[serial_test::serial]
async fn gateway_http_chat_reports_late_sse_error_without_success_or_retry() {
    let events=concat!("event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"部分中文\"}\n\n",
        "event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"message\":\"policy block sk-trace-secret\",\"code\":\"content_policy\"}}}\n\n");
    let upstream = Upstream::start(Router::new().route(
        "/v1/responses",
        post(move || async move { ([("content-type", "text/event-stream")], events) }),
    ))
    .await;
    let db = Arc::new(Database::memory().unwrap());
    let ep = endpoint(&db, UpstreamType::Codex, &upstream.url, vec![], 100);
    key(&db, &ep, "sk-trace-secret", 50);
    let (server, url) = gateway(db.clone(), "codex").await;
    let response=client().post(format!("{url}/v1/chat/completions")).json(&json!({"model":"test-model","messages":[{"role":"user","content":"hi"}],"stream":true})).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let id = response.headers()["x-ccswitch-request-id"]
        .to_str()
        .unwrap()
        .to_string();
    let text = response.text().await.unwrap();
    assert!(text.contains("部分中文"));
    assert!(text.contains("event: error"));
    assert!(!text.contains("\"finish_reason\":\"stop\""));
    let detail = completed_trace(&db, &id).await;
    assert_eq!(detail.summary.state, "error");
    assert_eq!(detail.summary.attempt_count, 1);
    assert!(detail.error.unwrap().contains("policy block [REDACTED]"));
    assert!(!serde_json::to_string(&detail.attempts)
        .unwrap()
        .contains("sk-trace-secret"));
    server.stop().await.unwrap();
}

#[tokio::test]
#[serial_test::serial]
async fn gateway_http_gzip_claude_request_is_forwarded_and_logged_as_utf8() {
    use std::io::Write;
    let seen = Arc::new(Mutex::new(None::<Value>));
    let calls = seen.clone();
    let upstream = Upstream::start(Router::new().route(
        "/v1/messages",
        post(move |headers: HeaderMap, Json(body): Json<Value>| {
            assert!(!headers.contains_key("content-encoding"));
            *calls.lock().unwrap() = Some(body);
            async { Json(claude_success()) }
        }),
    ))
    .await;
    let db = Arc::new(Database::memory().unwrap());
    let ep = endpoint(&db, UpstreamType::Claude, &upstream.url, vec![], 100);
    key(&db, &ep, "sk-gzip", 50);
    let (server, url) = gateway(db.clone(), "claude").await;
    let body=json!({"model":"test-model","messages":[{"role":"user","content":"压缩中文提示词"}],"max_tokens":64}).to_string();
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(body.as_bytes()).unwrap();
    let response = client()
        .post(format!("{url}/claude/v1/messages"))
        .header("content-type", "application/json")
        .header("content-encoding", "gzip")
        .body(encoder.finish().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let id = response.headers()["x-ccswitch-request-id"]
        .to_str()
        .unwrap()
        .to_string();
    response.bytes().await.unwrap();
    assert!(seen
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .to_string()
        .contains("压缩中文提示词"));
    assert_eq!(completed_trace(&db, &id).await.request.body, body);
    server.stop().await.unwrap();
}

#[tokio::test]
#[serial_test::serial]
async fn gateway_http_tool_calls_and_results_round_trip_across_all_protocol_pairs() {
    for upstream_protocol in [
        UpstreamType::Claude,
        UpstreamType::Codex,
        UpstreamType::Openai,
    ] {
        for entry in [
            UpstreamType::Claude,
            UpstreamType::Codex,
            UpstreamType::Openai,
        ] {
            let observed = Arc::new(Mutex::new(Vec::<Value>::new()));
            let calls = observed.clone();
            let upstream=Upstream::start(Router::new().route(protocol_path(upstream_protocol),post(move |Json(body):Json<Value>| {
                let first={let mut calls=calls.lock().unwrap();calls.push(body);calls.len()==1};
                async move {Json(if !first {protocol_json(upstream_protocol)} else {match upstream_protocol {
                    UpstreamType::Claude=>json!({"id":"msg_tool","type":"message","role":"assistant","model":"test-model","content":[{"type":"tool_use","id":"call_demo","name":"lookup","input":{"q":"中文"}}],"stop_reason":"tool_use","usage":{"input_tokens":1,"output_tokens":2}}),
                    UpstreamType::Codex=>json!({"id":"resp_tool","object":"response","status":"completed","model":"test-model","output":[{"type":"function_call","id":"fc_demo","call_id":"call_demo","name":"lookup","arguments":"{\"q\":\"中文\"}"}],"usage":{"input_tokens":1,"output_tokens":2}}),
                    _=>json!({"id":"chat_tool","object":"chat.completion","model":"test-model","choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_demo","type":"function","function":{"name":"lookup","arguments":"{\"q\":\"中文\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":1,"completion_tokens":2}})
                }})}
            }))).await;
            let db = Arc::new(Database::memory().unwrap());
            let ep = endpoint(
                &db,
                upstream_protocol,
                &upstream.url,
                vec!["test-model".into()],
                100,
            );
            key(&db, &ep, "sk-tools", 50);
            let (server, url) = gateway(
                db,
                if entry == UpstreamType::Claude {
                    "claude"
                } else {
                    "codex"
                },
            )
            .await;
            let schema =
                json!({"type":"object","properties":{"q":{"type":"string"}},"required":["q"]});
            let mut request = match entry {
                UpstreamType::Claude => {
                    json!({"messages":[{"role":"user","content":"查找"}],"max_tokens":64,"tools":[{"name":"lookup","input_schema":schema}]})
                }
                UpstreamType::Codex => {
                    json!({"input":"查找","tools":[{"type":"function","name":"lookup","parameters":schema}]})
                }
                _ => {
                    json!({"messages":[{"role":"user","content":"查找"}],"tools":[{"type":"function","function":{"name":"lookup","parameters":schema}}]})
                }
            };
            request["model"] = json!("test-model");
            let response = client()
                .post(format!("{url}{}", protocol_path(entry)))
                .json(&request)
                .send()
                .await
                .unwrap();
            let status = response.status();
            let body: Value = response.json().await.unwrap();
            assert_eq!(
                status,
                StatusCode::OK,
                "{entry:?}->{upstream_protocol:?}: {body}"
            );
            match entry {
                UpstreamType::Claude => {
                    request["messages"] = json!([{"role":"user","content":"查找"},{"role":"assistant","content":body["content"]},
                    {"role":"user","content":[{"type":"tool_result","tool_use_id":"call_demo","content":"真实工具结果"}]}])
                }
                UpstreamType::Codex => {
                    let mut input = vec![json!({"role":"user","content":"查找"})];
                    input.extend(body["output"].as_array().unwrap().clone());
                    input.push(json!({"type":"function_call_output","call_id":"call_demo","output":"真实工具结果"}));
                    request["input"] = json!(input);
                }
                _ => {
                    request["messages"] = json!([{"role":"user","content":"查找"},body["choices"][0]["message"],{"role":"tool","tool_call_id":"call_demo","content":"真实工具结果"}])
                }
            }
            let response = client()
                .post(format!("{url}{}", protocol_path(entry)))
                .json(&request)
                .send()
                .await
                .unwrap();
            let status = response.status();
            let text = response.text().await.unwrap();
            assert_eq!(
                status,
                StatusCode::OK,
                "{entry:?}->{upstream_protocol:?}: {text}"
            );
            let calls = observed.lock().unwrap();
            assert_eq!(calls.len(), 2);
            assert!(
                calls[1].to_string().contains("真实工具结果"),
                "{entry:?}->{upstream_protocol:?}: {}",
                calls[1]
            );
            assert!(calls[1].to_string().contains("call_demo"));
            drop(calls);
            server.stop().await.unwrap();
        }
    }
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
