//! Bounded, local-only HTTP diagnostics. Observers tee bytes without consuming,
//! delaying, rewriting, or buffering the forwarding stream to completion.

use super::{
    content_encoding::{decompress_body_with_limit, get_content_encoding},
    hyper_client::ProxyResponse,
    server::ProxyState,
};
use crate::database::{
    Database, RequestTraceAttempt, RequestTraceConfig, RequestTraceDetail, RequestTraceSummary,
    TraceHeader, TracePayload,
};
use axum::{
    body::Body,
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, HeaderValue},
    middleware::Next,
    response::Response,
};
use bytes::Bytes;
use futures::Stream;
use http_body::{Body as HttpBody, Frame, SizeHint};
use serde_json::Value;
use std::{
    net::SocketAddr,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll},
    time::Instant,
};

const REDACTED: &str = "[REDACTED]";
const MAX_SSE_EVENT_BYTES: usize = 128 * 1024;

fn sensitive_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase().replace('_', "-");
    matches!(
        name.as_str(),
        "authorization"
            | "proxy-authorization"
            | "cookie"
            | "set-cookie"
            | "api-key"
            | "apikey"
            | "key"
            | "password"
            | "secret"
            | "access-token"
            | "refresh-token"
    ) || name.ends_with("-api-key")
        || name.ends_with("-auth-token")
        || name.ends_with("-access-token")
}

fn header_secrets(headers: &HeaderMap) -> Vec<String> {
    let mut secrets = Vec::new();
    for (name, value) in headers {
        if !sensitive_name(name.as_str()) {
            continue;
        }
        let Ok(value) = value.to_str() else {
            continue;
        };
        secrets.push(value.to_string());
        if let Some((scheme, token)) = value.split_once(' ') {
            if scheme.eq_ignore_ascii_case("bearer") || scheme.eq_ignore_ascii_case("basic") {
                secrets.push(token.to_string());
            }
        }
        if name.as_str().contains("cookie") {
            let count = if name.as_str() == "set-cookie" {
                1
            } else {
                usize::MAX
            };
            for cookie in value.split(';').take(count) {
                if let Some((_, value)) = cookie.split_once('=') {
                    // Single-character flags are not credentials. Replacing them
                    // throughout a JSON body would destroy unrelated prompt text.
                    if value.trim().len() >= 8 {
                        secrets.push(value.trim().to_string());
                    }
                }
            }
        }
    }
    secrets.retain(|s| !s.is_empty());
    secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
    secrets.dedup();
    secrets
}

fn redact_text(text: &str, secrets: &[String]) -> String {
    let mut patterns = Vec::new();
    for secret in secrets.iter().filter(|s| !s.is_empty()) {
        patterns.push(regex::escape(secret));
        // JSON may escape a credential echoed inside an upstream error message.
        if let Ok(encoded) = serde_json::to_string(secret) {
            let escaped = &encoded[1..encoded.len() - 1];
            if escaped != secret {
                patterns.push(regex::escape(escaped));
            }
        }
    }
    if patterns.is_empty() {
        return text.to_string();
    }
    // One pass: a later short credential must not match the redaction marker
    // inserted for an earlier one (which could otherwise expand exponentially).
    match regex::Regex::new(&patterns.join("|")) {
        Ok(pattern) => pattern
            .replace_all(text, regex::NoExpand(REDACTED))
            .into_owned(),
        Err(_) => REDACTED.to_string(),
    }
}

fn url_secrets(raw: &str) -> Vec<String> {
    let raw = if raw.starts_with('/') {
        format!("http://local.invalid{raw}")
    } else {
        raw.to_string()
    };
    let Ok(url) = url::Url::parse(&raw) else {
        return Vec::new();
    };
    let mut secrets = url
        .query_pairs()
        .filter(|(key, _)| sensitive_name(key) || key == "token" || key == "auth")
        .map(|(_, value)| value.into_owned())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if let Some(password) = url.password().filter(|value| !value.is_empty()) {
        secrets.push(password.into());
    }
    secrets
}

fn redact_json_credentials(value: &mut Value) -> bool {
    let mut changed = false;
    match value {
        Value::Object(object) => {
            for (name, value) in object {
                if sensitive_name(name)
                    && !value.is_object()
                    && !value.is_array()
                    && !value.is_null()
                {
                    *value = Value::String(REDACTED.into());
                    changed = true;
                } else {
                    changed |= redact_json_credentials(value);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                changed |= redact_json_credentials(item);
            }
        }
        _ => {}
    }
    changed
}

fn redact_url(raw: &str, secrets: &[String]) -> String {
    let absolute = !raw.starts_with('/');
    let Ok(mut url) = url::Url::parse(if absolute {
        raw
    } else {
        "http://local.invalid"
    }) else {
        return redact_text(raw, secrets);
    };
    if !absolute {
        let Ok(joined) = url.join(raw) else {
            return "[invalid URL]".into();
        };
        url = joined;
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    let query: Vec<(String, String)> = url
        .query_pairs()
        .map(|(key, value)| {
            let value = if sensitive_name(&key) || key == "token" || key == "auth" {
                REDACTED.to_string()
            } else {
                redact_text(&value, secrets)
            };
            (key.into_owned(), value)
        })
        .collect();
    if url.query().is_some() {
        url.query_pairs_mut().clear().extend_pairs(query);
    }
    let rendered = if absolute {
        url.to_string()
    } else {
        format!(
            "{}{}",
            url.path(),
            url.query().map(|q| format!("?{q}")).unwrap_or_default()
        )
    };
    redact_text(&rendered, secrets)
}

fn error_message(value: &Value) -> Option<String> {
    let response = value.get("response").unwrap_or(value);
    if let Some(error) = response.get("error").filter(|error| !error.is_null()) {
        return Some(
            error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.as_str())
                .unwrap_or("Upstream returned an error envelope")
                .to_string(),
        );
    }
    if matches!(
        response.get("status").and_then(Value::as_str),
        Some("failed" | "cancelled")
    ) {
        return Some("Upstream reported a failed/cancelled response".into());
    }
    if matches!(
        value.get("type").and_then(Value::as_str),
        Some("error" | "response.failed")
    ) {
        return Some(
            value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Upstream stream error")
                .into(),
        );
    }
    if response
        .pointer("/incomplete_details/reason")
        .and_then(Value::as_str)
        == Some("content_filter")
        || value.pointer("/delta/stop_reason").and_then(Value::as_str) == Some("refusal")
        || value.get("stop_reason").and_then(Value::as_str) == Some("refusal")
    {
        return Some("Upstream reported content_filter/refusal".into());
    }
    if value
        .get("choices")
        .and_then(Value::as_array)
        .is_some_and(|choices| {
            choices.iter().any(|choice| {
                choice.get("finish_reason").and_then(Value::as_str) == Some("content_filter")
            })
        })
    {
        return Some("Upstream reported content_filter".into());
    }
    None
}

#[derive(Default)]
struct EventErrors {
    buffer: Vec<u8>,
    line_has_content: bool,
    overflow: bool,
    error: Option<String>,
    terminal: bool,
    saw_data: bool,
    skipped_event: bool,
}
impl EventErrors {
    fn push(&mut self, chunk: &[u8]) {
        // Continue inspecting terminal/error events even after body capture fills.
        // An oversized individual event is skipped without growing the buffer.
        for byte in chunk {
            if self.buffer.len() < MAX_SSE_EVENT_BYTES && !self.overflow {
                self.buffer.push(*byte);
            } else {
                self.overflow = true;
            }
            if *byte == b'\n' {
                if !self.line_has_content {
                    self.skipped_event |= self.overflow;
                    if !self.overflow {
                        if let Ok(text) = std::str::from_utf8(&self.buffer) {
                            let data = text
                                .lines()
                                .filter_map(|line| super::sse::strip_sse_field(line, "data"))
                                .collect::<Vec<_>>()
                                .join("\n");
                            if !data.is_empty() {
                                self.saw_data = true;
                            }
                            if data.trim() == "[DONE]" {
                                self.terminal = true;
                            }
                            if let Ok(value) = serde_json::from_str::<Value>(&data) {
                                self.terminal |= matches!(
                                    value.get("type").and_then(Value::as_str),
                                    Some(
                                        "message_stop"
                                            | "response.completed"
                                            | "response.incomplete"
                                            | "response.failed"
                                            | "error"
                                    )
                                ) || value
                                    .pointer("/delta/stop_reason")
                                    .is_some_and(|reason| !reason.is_null())
                                    || value.get("choices").and_then(Value::as_array).is_some_and(
                                        |choices| {
                                            choices.iter().any(|choice| {
                                                choice
                                                    .get("finish_reason")
                                                    .is_some_and(|reason| !reason.is_null())
                                            })
                                        },
                                    );
                                if let Some(error) = error_message(&value) {
                                    self.error = Some(error);
                                }
                            }
                        }
                    }
                    self.buffer.clear();
                    self.overflow = false;
                }
                self.line_has_content = false;
            } else if *byte != b'\r' {
                self.line_has_content = true;
            }
        }
    }
}

struct CaptureData {
    headers: HeaderMap,
    bytes: Vec<u8>,
    observed: u64,
    enabled: bool,
    limit: usize,
    budget: Arc<AtomicUsize>,
    error: Option<String>,
    events: Option<EventErrors>,
    ended_at: Option<Instant>,
}

#[derive(Clone)]
struct PayloadCapture(Arc<Mutex<CaptureData>>);
impl PayloadCapture {
    fn new(headers: HeaderMap, config: &RequestTraceConfig, budget: Arc<AtomicUsize>) -> Self {
        let events = headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .filter(|v| v.contains("text/event-stream"))
            .map(|_| EventErrors::default());
        Self(Arc::new(Mutex::new(CaptureData {
            headers,
            bytes: Vec::new(),
            observed: 0,
            enabled: config.capture_bodies,
            limit: config.max_body_bytes,
            budget,
            error: None,
            events,
            ended_at: None,
        })))
    }
    fn push(&self, bytes: &[u8]) {
        let mut data = self.0.lock().unwrap_or_else(|e| e.into_inner());
        data.observed = data.observed.saturating_add(bytes.len() as u64);
        if let Some(events) = data.events.as_mut() {
            events.push(bytes);
        }
        if !data.enabled {
            return;
        }
        let wanted = bytes.len().min(data.limit.saturating_sub(data.bytes.len()));
        let mut available = 0;
        let _ = data
            .budget
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                available = wanted.min(left);
                Some(left - available)
            });
        data.bytes.extend_from_slice(&bytes[..available]);
    }
    fn end(&self, error: Option<String>) {
        let mut data = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if data.ended_at.is_none() {
            data.ended_at = Some(Instant::now());
        }
        if error.is_some() {
            data.error = error;
        }
        if let Some(events) = data.events.as_mut() {
            events.push(b"\n\n");
            if !events.terminal
                && events.saw_data
                && !events.skipped_event
                && events.error.is_none()
            {
                events.error = Some("SSE stream ended before a terminal event".into());
            }
        }
    }
    fn error(&self) -> Option<String> {
        let data = self.0.lock().unwrap_or_else(|e| e.into_inner());
        data.error
            .clone()
            .or_else(|| data.events.as_ref().and_then(|events| events.error.clone()))
            .or_else(|| {
                serde_json::from_slice::<Value>(&data.bytes)
                    .ok()
                    .as_ref()
                    .and_then(error_message)
            })
    }
    fn snapshot(&self, secrets: &[String]) -> TracePayload {
        let data = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let headers = data
            .headers
            .iter()
            .map(|(name, value)| TraceHeader {
                name: name.to_string(),
                value: if sensitive_name(name.as_str()) {
                    REDACTED.into()
                } else {
                    redact_text(value.to_str().unwrap_or("[binary header]"), secrets)
                },
            })
            .collect();
        let mut payload = TracePayload {
            headers,
            body_bytes: data.observed,
            captured_bytes: data.bytes.len(),
            truncated: data.enabled && data.observed > data.bytes.len() as u64,
            body_encoding: if data.enabled { "utf8" } else { "omitted" }.into(),
            capture_error: data.error.as_ref().map(|error| redact_text(error, secrets)),
            ..Default::default()
        };
        if !data.enabled {
            return payload;
        }
        let decoded = match get_content_encoding(&data.headers) {
            Some(encoding) => {
                match decompress_body_with_limit(&encoding, &data.bytes, data.limit) {
                    Ok(Some(decoded)) => decoded,
                    Ok(None) => data.bytes.clone(),
                    Err(_) => {
                        payload.capture_error = Some(
                            "Compressed capture is incomplete or exceeds the decoded body limit"
                                .into(),
                        );
                        data.bytes.clone()
                    }
                }
            }
            None => data.bytes.clone(),
        };
        let text = match std::str::from_utf8(&decoded) {
            Ok(text) => Some(text),
            Err(error) if payload.truncated && error.error_len().is_none() => {
                std::str::from_utf8(&decoded[..error.valid_up_to()]).ok()
            }
            Err(_) => None,
        };
        if let Some(text) = text {
            let mut redacted = redact_text(text, secrets);
            if let Ok(mut json) = serde_json::from_str::<Value>(&redacted) {
                if redact_json_credentials(&mut json) {
                    if let Ok(encoded) = serde_json::to_string(&json) {
                        redacted = encoded;
                    }
                }
            }
            payload.redacted = redacted != text;
            if redacted.len() > data.limit {
                let mut end = data.limit;
                while !redacted.is_char_boundary(end) {
                    end -= 1;
                }
                redacted.truncate(end);
                payload.truncated = true;
                payload.capture_error =
                    Some("Redacted text exceeds the configured body capture limit".into());
            }
            payload.body = redacted;
        } else {
            // Binary captures may contain credentials that cannot be reliably
            // redacted, so keep byte counts rather than persist opaque secrets.
            payload.body_encoding = "omitted".into();
            payload
                .capture_error
                .get_or_insert_with(|| "Binary or undecodable body was not persisted".into());
        }
        payload
    }
}

struct AttemptRuntime {
    detail: RequestTraceAttempt,
    request: PayloadCapture,
    response: Option<PayloadCapture>,
    started: Instant,
}
struct TraceRuntime {
    summary: RequestTraceSummary,
    request: PayloadCapture,
    response: Option<PayloadCapture>,
    attempts: Vec<AttemptRuntime>,
    secrets: Vec<String>,
    error: Option<String>,
    revision: u64,
    finished: bool,
}
struct TraceShared {
    runtime: Mutex<TraceRuntime>,
    db: Arc<Database>,
    config: RequestTraceConfig,
    started: Instant,
    budget: Arc<AtomicUsize>,
}

#[derive(Clone)]
pub(crate) struct RequestTrace(Arc<TraceShared>);
impl RequestTrace {
    fn snapshot(&self) -> (RequestTraceDetail, u64) {
        let mut runtime = self.0.runtime.lock().unwrap_or_else(|e| e.into_inner());
        runtime.revision += 1;
        let secrets = &runtime.secrets;
        let attempts = runtime
            .attempts
            .iter()
            .map(|attempt| {
                let mut detail = attempt.detail.clone();
                detail.request = attempt.request.snapshot(secrets);
                detail.response = attempt
                    .response
                    .as_ref()
                    .map(|response| response.snapshot(secrets));
                detail.error = attempt
                    .detail
                    .error
                    .clone()
                    .or_else(|| attempt.response.as_ref().and_then(PayloadCapture::error))
                    .map(|error| redact_text(&error, secrets));
                if let Some(response) = &attempt.response {
                    detail.duration_ms = Some(
                        response
                            .0
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .ended_at
                            .unwrap_or_else(Instant::now)
                            .duration_since(attempt.started)
                            .as_millis() as u64,
                    );
                }
                detail.url = redact_url(&detail.url, secrets);
                detail.proxy = detail.proxy.map(|url| redact_url(&url, secrets));
                detail.provider_name = redact_text(&detail.provider_name, secrets);
                detail
            })
            .collect();
        let mut summary = runtime.summary.clone();
        summary.path = redact_url(&summary.path, secrets);
        summary.provider_name = summary
            .provider_name
            .map(|name| redact_text(&name, secrets));
        (
            RequestTraceDetail {
                summary,
                request: runtime.request.snapshot(secrets),
                response: runtime
                    .response
                    .as_ref()
                    .map(|response| response.snapshot(secrets)),
                attempts,
                error: runtime
                    .error
                    .as_ref()
                    .map(|error| redact_text(error, secrets)),
                body_capture_enabled: self.0.config.capture_bodies,
            },
            runtime.revision,
        )
    }
    fn persist(&self) {
        let trace = self.clone();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            // Shutdown may drop an HTTP body outside an entered runtime. The
            // persisted in-progress row is recovered as interrupted next launch.
            log::warn!("[RequestTrace] Runtime stopped before final trace persistence");
            return;
        };
        runtime.spawn_blocking(move || {
            let (detail, revision) = trace.snapshot();
            if let Err(error) = trace
                .0
                .db
                .update_request_trace(&detail, revision, &trace.0.config)
            {
                log::warn!(
                    "[RequestTrace] Persist failed for {}: {error}",
                    detail.summary.request_id
                );
            }
        });
    }
    fn finish(&self, forced_error: Option<String>, cancelled: bool) {
        {
            let mut runtime = self.0.runtime.lock().unwrap_or_else(|e| e.into_inner());
            if runtime.finished {
                return;
            }
            runtime.finished = true;
            runtime.error =
                forced_error.or_else(|| runtime.response.as_ref().and_then(PayloadCapture::error));
            runtime.summary.state = if cancelled {
                "cancelled"
            } else if runtime.error.is_some()
                || runtime.summary.status_code.is_some_and(|s| s >= 400)
            {
                "error"
            } else {
                "completed"
            }
            .into();
            runtime.summary.duration_ms = Some(self.0.started.elapsed().as_millis() as u64);
        }
        self.persist();
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_attempt(
        &self,
        provider: &crate::provider::Provider,
        method: &http::Method,
        url: &str,
        headers: &HeaderMap,
        body: &[u8],
        protocol: &str,
        model: Option<&str>,
        proxy: Option<&str>,
        secrets: &[String],
    ) -> TraceAttempt {
        let request = PayloadCapture::new(headers.clone(), &self.0.config, self.0.budget.clone());
        request.push(body);
        request.end(None);
        let index = {
            let mut runtime = self.0.runtime.lock().unwrap_or_else(|e| e.into_inner());
            runtime.secrets.extend(secrets.iter().cloned());
            runtime.secrets.extend(header_secrets(headers));
            runtime.secrets.extend(url_secrets(url));
            if let Some(proxy) = proxy {
                runtime.secrets.extend(url_secrets(proxy));
            }
            runtime.secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
            runtime.secrets.dedup();
            let index = runtime.attempts.len();
            runtime.summary.attempt_count = index + 1;
            runtime.summary.provider_name = Some(provider.name.clone());
            runtime.attempts.push(AttemptRuntime {
                detail: RequestTraceAttempt {
                    index: index + 1,
                    provider_id: provider.id.clone(),
                    provider_name: provider.name.clone(),
                    protocol: protocol.into(),
                    method: method.to_string(),
                    url: url.into(),
                    proxy: proxy.map(str::to_owned),
                    model: model.map(str::to_owned),
                    started_at: chrono::Utc::now().timestamp_millis(),
                    ..Default::default()
                },
                request,
                response: None,
                started: Instant::now(),
            });
            index
        };
        self.persist();
        TraceAttempt {
            trace: self.clone(),
            index,
        }
    }
    pub(crate) fn set_model(&self, model: &str) {
        self.0
            .runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .summary
            .model = Some(model.into());
    }
}

pub(crate) struct TraceAttempt {
    trace: RequestTrace,
    index: usize,
}
impl TraceAttempt {
    pub(crate) fn transport_error(&self, error: &str) {
        {
            let mut runtime = self
                .trace
                .0
                .runtime
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let attempt = &mut runtime.attempts[self.index];
            attempt.detail.error = Some(error.into());
            attempt.detail.duration_ms = Some(attempt.started.elapsed().as_millis() as u64);
        }
        self.trace.persist();
    }
    pub(crate) fn observe_response(&self, response: ProxyResponse) -> ProxyResponse {
        let status = response.status();
        let headers = response.headers().clone();
        let capture = PayloadCapture::new(
            headers.clone(),
            &self.trace.0.config,
            self.trace.0.budget.clone(),
        );
        {
            let mut runtime = self
                .trace
                .0
                .runtime
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            runtime.secrets.extend(header_secrets(&headers));
            runtime.secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
            runtime.secrets.dedup();
            let attempt = &mut runtime.attempts[self.index];
            attempt.detail.status_code = Some(status.as_u16());
            attempt.response = Some(capture.clone());
        }
        self.trace.persist();
        ProxyResponse::streamed(
            status,
            headers,
            CaptureStream {
                inner: Box::pin(response.bytes_stream()),
                capture,
                ended: false,
            },
        )
    }
}

struct CaptureStream {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
    capture: PayloadCapture,
    ended: bool,
}
impl Stream for CaptureStream {
    type Item = Result<Bytes, std::io::Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let result = self.inner.as_mut().poll_next(cx);
        match &result {
            Poll::Ready(Some(Ok(bytes))) => self.capture.push(bytes),
            Poll::Ready(Some(Err(error))) => {
                self.capture.end(Some(error.to_string()));
                self.ended = true;
            }
            Poll::Ready(None) => {
                self.capture.end(None);
                self.ended = true;
            }
            _ => {}
        }
        result
    }
}
impl Drop for CaptureStream {
    fn drop(&mut self) {
        if !self.ended {
            self.capture.end(None);
        }
    }
}

struct CaptureBody {
    inner: Pin<Box<Body>>,
    capture: PayloadCapture,
    trace: Option<RequestTrace>,
    ended: bool,
}
impl HttpBody for CaptureBody {
    type Data = Bytes;
    type Error = axum::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let result = self.inner.as_mut().poll_frame(cx);
        match &result {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(bytes) = frame.data_ref() {
                    self.capture.push(bytes);
                    if !bytes.is_empty() {
                        if let Some(trace) = &self.trace {
                            let mut runtime =
                                trace.0.runtime.lock().unwrap_or_else(|e| e.into_inner());
                            runtime.summary.first_byte_ms.get_or_insert_with(|| {
                                trace.0.started.elapsed().as_millis() as u64
                            });
                        }
                    }
                }
                if self.inner.is_end_stream() {
                    self.complete(None, false);
                }
            }
            Poll::Ready(Some(Err(error))) => self.complete(Some(error.to_string()), false),
            Poll::Ready(None) => self.complete(None, false),
            _ => {}
        }
        result
    }
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}
impl CaptureBody {
    fn complete(&mut self, error: Option<String>, cancelled: bool) {
        if self.ended {
            return;
        }
        self.ended = true;
        self.capture.end(error.clone());
        if let Some(trace) = &self.trace {
            trace.finish(error, cancelled);
        }
    }
}
impl Drop for CaptureBody {
    fn drop(&mut self) {
        if !self.ended {
            let cancelled = !self.inner.is_end_stream();
            self.complete(
                cancelled.then(|| "Client disconnected before the response completed".into()),
                cancelled,
            );
        }
    }
}

struct RequestLifetime(Option<RequestTrace>);
impl Drop for RequestLifetime {
    fn drop(&mut self) {
        if let Some(trace) = self.0.take() {
            trace.finish(
                Some("Request cancelled before response headers".into()),
                true,
            );
        }
    }
}

pub(crate) async fn middleware(
    State(state): State<ProxyState>,
    mut request: Request,
    next: Next,
) -> Response {
    if matches!(request.uri().path(), "/health" | "/status") {
        return next.run(request).await;
    }
    let config = match state.db.get_request_trace_config() {
        Ok(config) if config.enabled => config,
        Ok(_) => return next.run(request).await,
        Err(error) => {
            log::warn!("[RequestTrace] Cannot load configuration: {error}");
            return next.run(request).await;
        }
    };
    let started = Instant::now();
    let id = uuid::Uuid::new_v4().to_string();
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0);
    let path = request.uri().path();
    let protocol = if path.contains("chat/completions") {
        "openai_chat"
    } else if path.contains("responses") {
        "openai_responses"
    } else if path.ends_with("/messages") {
        "anthropic"
    } else if path.contains("/v1beta/") || path.starts_with("/gemini/") {
        "gemini_native"
    } else {
        "http"
    };
    let budget = Arc::new(AtomicUsize::new(
        (config.max_storage_mb as usize * 1024 * 1024 / 8).min(16 * 1024 * 1024),
    ));
    let capture = PayloadCapture::new(request.headers().clone(), &config, budget.clone());
    let trace = RequestTrace(Arc::new(TraceShared {
        db: state.db.clone(),
        config: config.clone(),
        started,
        budget,
        runtime: Mutex::new(TraceRuntime {
            summary: RequestTraceSummary {
                request_id: id.clone(),
                started_at: chrono::Utc::now().timestamp_millis(),
                method: request.method().to_string(),
                path: request.uri().to_string(),
                client_ip: peer
                    .map(|p| p.ip().to_string())
                    .unwrap_or_else(|| "unknown".into()),
                client_port: peer.map(|p| p.port()),
                entry_protocol: protocol.into(),
                state: "in_progress".into(),
                ..Default::default()
            },
            request: capture.clone(),
            response: None,
            attempts: Vec::new(),
            secrets: header_secrets(request.headers())
                .into_iter()
                .chain(url_secrets(&request.uri().to_string()))
                .collect(),
            error: None,
            revision: 0,
            finished: false,
        }),
    }));
    let initial = trace.snapshot().0;
    let db = state.db.clone();
    match tokio::task::spawn_blocking(move || db.insert_request_trace(&initial, &config)).await {
        Ok(Ok(())) => {}
        error => {
            log::warn!("[RequestTrace] Cannot create trace: {error:?}");
            return next.run(request).await;
        }
    }
    request.extensions_mut().insert(trace.clone());
    let (parts, body) = request.into_parts();
    let body = Body::new(CaptureBody {
        inner: Box::pin(body),
        capture,
        trace: None,
        ended: false,
    });
    let mut lifetime = RequestLifetime(Some(trace.clone()));
    let mut response = next.run(Request::from_parts(parts, body)).await;
    let capture = PayloadCapture::new(
        response.headers().clone(),
        &trace.0.config,
        trace.0.budget.clone(),
    );
    {
        let mut runtime = trace.0.runtime.lock().unwrap_or_else(|e| e.into_inner());
        runtime.summary.status_code = Some(response.status().as_u16());
        runtime.response = Some(capture.clone());
        if runtime.summary.model.is_none() {
            let body = runtime.request.snapshot(&runtime.secrets);
            runtime.summary.model = serde_json::from_str::<Value>(&body.body)
                .ok()
                .and_then(|json| json.get("model").and_then(Value::as_str).map(str::to_owned));
        }
    }
    trace.persist();
    response.headers_mut().insert(
        "x-ccswitch-request-id",
        HeaderValue::from_str(&id).expect("UUID is a valid header"),
    );
    lifetime.0.take();
    let (parts, body) = response.into_parts();
    let observed = CaptureBody {
        inner: Box::pin(body),
        capture,
        trace: Some(trace),
        ended: false,
    };
    Response::from_parts(parts, Body::new(observed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_redaction_does_not_reprocess_its_own_markers() {
        assert_eq!(
            redact_text("A E", &["A".into(), "E".into()]),
            "[REDACTED] [REDACTED]"
        );
    }
    #[test]
    fn trace_capture_redacts_credentials_and_preserves_split_utf8() {
        let config = RequestTraceConfig {
            max_body_bytes: 8,
            ..Default::default()
        };
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer secret"));
        let capture =
            PayloadCapture::new(headers.clone(), &config, Arc::new(AtomicUsize::new(100)));
        let text = "中文提示词".as_bytes();
        capture.push(&text[..2]);
        capture.push(&text[2..]);
        capture.end(None);
        let snapshot = capture.snapshot(&header_secrets(&headers));
        assert_eq!(snapshot.body, "中文");
        assert!(snapshot.truncated);
        assert_eq!(snapshot.body_bytes, text.len() as u64);
        assert_eq!(snapshot.headers[0].value, REDACTED);
        assert!(!redact_url(
            "https://user:pass@example.com/v1?api_key=secret&mode=fast",
            &vec!["secret".into()]
        )
        .contains("secret"));
    }
    #[test]
    fn trace_stream_errors_are_detected_after_capture_budget_is_full() {
        let config = RequestTraceConfig {
            max_body_bytes: 4,
            ..Default::default()
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            "content-type",
            HeaderValue::from_static("text/event-stream"),
        );
        let capture = PayloadCapture::new(headers, &config, Arc::new(AtomicUsize::new(4)));
        capture.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n");
        capture.push(b"event: error\ndata: {\"error\":{\"message\":\"blocked prompt\"}}\n\n");
        assert_eq!(capture.error().as_deref(), Some("blocked prompt"));
        assert!(capture.snapshot(&[]).truncated);
    }
}
