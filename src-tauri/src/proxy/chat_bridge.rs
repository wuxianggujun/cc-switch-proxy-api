//! Response half of the Chat entry bridge. Routing chooses an upstream protocol;
//! this module always restores the protocol requested by the caller.

use super::{
    forwarder::ActiveConnectionGuard,
    handler_config::{CODEX_PARSER_CONFIG, OPENAI_PARSER_CONFIG},
    handler_context::RequestContext,
    hyper_client::ProxyResponse,
    providers::{
        codex_responses_sse,
        streaming_chat_entry::create_chat_sse_stream_from_responses_with_options,
        streaming_codex_anthropic::create_responses_sse_stream_from_anthropic_with_context,
        transform_chat_entry::responses_response_to_chat,
        transform_codex_anthropic::{
            anthropic_response_to_responses_with_context, anthropic_sse_to_message_value,
        },
        transform_codex_chat::CodexToolContext,
    },
    response_processor::{
        create_logged_passthrough_stream, create_usage_collector, process_response,
        read_decoded_body, strip_entity_headers_for_rebuilt_body,
        strip_hop_by_hop_response_headers,
    },
    server::ProxyState,
    sse::{strip_sse_field, take_sse_block},
    ProxyError,
};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use bytes::Bytes;
use futures::Stream;
use serde_json::Value;
use std::{pin::Pin, time::Duration};

type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>;

pub(super) async fn handle_chat_upstream_response(
    response: ProxyResponse,
    ctx: &RequestContext,
    state: &ProxyState,
    is_stream: bool,
    include_usage: bool,
    tool_context: CodexToolContext,
    connection_guard: Option<ActiveConnectionGuard>,
) -> Result<axum::response::Response, ProxyError> {
    if super::providers::codex_provider_uses_chat_completions(&ctx.provider) {
        return process_response(
            response,
            ctx,
            state,
            &OPENAI_PARSER_CONFIG,
            connection_guard,
        )
        .await;
    }

    let anthropic = super::providers::codex_provider_uses_anthropic(&ctx.provider);
    let status = response.status();
    let mut headers = response.headers().clone();
    let live_sse = response.is_sse() || (is_stream && !response.is_json());

    if is_stream && live_sse {
        if super::content_encoding::get_content_encoding(&headers).is_some() {
            return Err(ProxyError::TransformError(
                "Upstream compressed an SSE response despite Accept-Encoding: identity".into(),
            ));
        }
        let stream: ByteStream = if anthropic {
            Box::pin(create_responses_sse_stream_from_anthropic_with_context(
                response.bytes_stream(),
                tool_context,
            ))
        } else {
            Box::pin(response.bytes_stream())
        };
        return build_chat_stream(
            stream,
            headers,
            status,
            ctx,
            state,
            include_usage,
            connection_guard,
        );
    }

    let timeout = if ctx.routing_retry_enabled() && ctx.app_config.non_streaming_timeout > 0 {
        Duration::from_secs(ctx.app_config.non_streaming_timeout as u64)
    } else {
        Duration::ZERO
    };
    let (decoded_headers, status, bytes) = read_decoded_body(response, ctx.tag, timeout).await?;
    headers = decoded_headers;
    let value = match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) if anthropic => {
            anthropic_response_to_responses_with_context(value, &tool_context)?
        }
        Ok(value) => value,
        Err(_) if live_sse || bytes.starts_with(b"event:") || bytes.starts_with(b"data:") => {
            let text = std::str::from_utf8(&bytes)
                .map_err(|_| ProxyError::TransformError("Upstream SSE is not UTF-8".into()))?;
            if anthropic {
                anthropic_response_to_responses_with_context(
                    anthropic_sse_to_message_value(text)?,
                    &tool_context,
                )?
            } else {
                aggregate_responses_sse(text)?
            }
        }
        Err(error) => {
            return Err(ProxyError::TransformError(format!(
                "Invalid upstream JSON for Chat bridge: {error}"
            )))
        }
    };

    if is_stream {
        // Some compatible upstreams ignore stream:true. A terminal Responses
        // event replays the complete output through the exact same state machine.
        let event = codex_responses_sse::response_completed(&value);
        return build_chat_stream(
            Box::pin(futures::stream::once(async move { Ok(event) })),
            headers,
            status,
            ctx,
            state,
            include_usage,
            connection_guard,
        );
    }
    let chat = responses_response_to_chat(value)?;
    let status = if chat.get("error").is_some() && status.is_success() {
        StatusCode::BAD_GATEWAY
    } else {
        status
    };
    rebuild_headers(&mut headers, false);
    let bytes = serde_json::to_vec(&chat)
        .map_err(|e| ProxyError::TransformError(format!("Cannot serialize Chat response: {e}")))?;
    process_response(
        ProxyResponse::buffered(status, headers, Bytes::from(bytes)),
        ctx,
        state,
        &OPENAI_PARSER_CONFIG,
        connection_guard,
    )
    .await
}

fn build_chat_stream(
    stream: ByteStream,
    mut headers: HeaderMap,
    status: StatusCode,
    ctx: &RequestContext,
    state: &ProxyState,
    include_usage: bool,
    connection_guard: Option<ActiveConnectionGuard>,
) -> Result<axum::response::Response, ProxyError> {
    // Meter before formatting: include_usage is a client presentation option,
    // not permission to disable the proxy's independent usage accounting.
    let collector = create_usage_collector(ctx, state, status.as_u16(), &CODEX_PARSER_CONFIG);
    let logged = create_logged_passthrough_stream(
        stream,
        ctx.tag,
        collector,
        ctx.streaming_timeout_config(),
        connection_guard,
    );
    let chat = create_chat_sse_stream_from_responses_with_options(logged, include_usage);
    rebuild_headers(&mut headers, true);
    let mut response = axum::response::Response::new(axum::body::Body::from_stream(chat));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    Ok(response)
}

fn rebuild_headers(headers: &mut HeaderMap, streaming: bool) {
    strip_entity_headers_for_rebuilt_body(headers);
    strip_hop_by_hop_response_headers(headers);
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(if streaming {
            "text/event-stream; charset=utf-8"
        } else {
            "application/json; charset=utf-8"
        }),
    );
    if streaming {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    }
}

fn aggregate_responses_sse(text: &str) -> Result<Value, ProxyError> {
    let mut buffer = format!("{text}\n\n");
    while let Some(block) = take_sse_block(&mut buffer) {
        let data = block
            .lines()
            .filter_map(|line| strip_sse_field(line, "data"))
            .collect::<Vec<_>>()
            .join("\n");
        let Ok(value) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        if value.get("error").is_some_and(|v| !v.is_null()) {
            return Ok(value);
        }
        if matches!(
            value.get("type").and_then(Value::as_str),
            Some("response.completed" | "response.incomplete" | "response.failed")
        ) {
            return value.get("response").cloned().ok_or_else(|| {
                ProxyError::TransformError("Missing terminal Responses payload".into())
            });
        }
    }
    Err(ProxyError::TransformError(
        "Responses stream ended without a terminal response".into(),
    ))
}
