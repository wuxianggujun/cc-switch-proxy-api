//! Responses SSE → Chat Completions SSE 桥
//!
//! 供 Chat Completions 入口（`/v1/chat/completions`）使用：上游是 Responses 或
//! Anthropic 网关时，既有链路会先产出 Responses SSE，本模块再还原成客户端
//! 期待的 Chat Completions SSE（`chat.completion.chunk` + `data: [DONE]`）。
//!
//! 方向与 `streaming_codex_chat.rs` 互逆：
//! - `streaming_codex_chat.rs`: Chat SSE → Responses SSE
//! - 本模块:                    Responses SSE → Chat SSE
//!
//! 失败语义是本模块的核心约束：上游把失败塞进 HTTP 200 的 SSE 里是网关常态，
//! 一旦被还原成"正常收尾"，客户端会把半截或空回答当成完整答案。因此所有终止
//! 路径都必须区分「完成」与「失败」，宁可多报一次错，也不补 finish_reason。

use super::codex_chat_common::extract_reasoning_summary_text;
use super::transform_codex_chat::flatten_namespace_tool_name;
use crate::proxy::sse::{append_utf8_safe, strip_sse_field, take_sse_block};
use bytes::Bytes;
use futures::stream::{Stream, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Chat Completions 流式块的 `object` 字段。
const CHAT_CHUNK_OBJECT: &str = "chat.completion.chunk";

/// 上游没给 id 时的兜底：客户端普遍要求 `id` 非空。
const DEFAULT_CHAT_ID: &str = "chatcmpl_ccswitch";

/// SSE 收尾标记。
fn done_marker() -> Bytes {
    Bytes::from_static(b"data: [DONE]\n\n")
}

/// 只有 `data:` 行的 Chat 块（Chat Completions 流不用命名事件）。
fn sse_data(payload: &Value) -> Bytes {
    Bytes::from(format!(
        "data: {}\n\n",
        serde_json::to_string(payload).unwrap_or_default()
    ))
}

/// 带 `event:` 行的块，仅用于错误：命名事件能让只看事件名的中间层也识别失败。
fn sse_named_event(event: &str, payload: &Value) -> Bytes {
    Bytes::from(format!(
        "event: {event}\ndata: {}\n\n",
        serde_json::to_string(payload).unwrap_or_default()
    ))
}

/// 一个 Chat 侧 `tool_calls[]` 槽位。
///
/// Responses 用 `item_id` / `call_id` 定位工具调用，Chat 用连续的 `index`，
/// 因此必须显式维护映射。`header_sent` 之前不下发任何 arguments 分片：Chat
/// 客户端在拿到 `id` + `function.name` 之前无法把分片归属到任何调用，抢跑只会
/// 制造一个匿名幽灵调用（与 `streaming_codex_chat` 的 `flush_ready_tool_calls`
/// 同一取向：宁可晚发，不可发出没有身份的调用）。
#[derive(Debug, Default)]
struct ToolSlot {
    index: usize,
    call_id: String,
    name: String,
    /// 尚未下发的 arguments 分片（header 未发出时暂存）。
    pending_args: String,
    /// `custom_tool_call` 的裸文本入参，收尾时才包成 JSON。
    custom_input: String,
    is_custom: bool,
    header_sent: bool,
    /// 是否已向客户端下发过任何 arguments 分片。终止时据此决定要不要用
    /// 终态 item 的完整 arguments 补发，避免重复累加。
    args_streamed: bool,
}

#[derive(Debug, Default)]
struct ResponsesToChatState {
    chat_id: String,
    model: String,
    created: u64,
    /// 首块 delta 必须带 `role":"assistant"`。
    role_sent: bool,
    /// 已发出终止块（finish_reason 或错误），此后一律不再产出任何事件。
    terminated: bool,
    /// 终止是以失败形式发生的：用于禁止后续补正常完成事件。
    failed: bool,
    /// 见过任何实质输出（文本 / 推理 / 工具调用），决定流被掐断时
    /// 报 `length` 截断还是报错。
    has_output: bool,
    text_streamed: bool,
    reasoning_streamed: bool,
    reasoning_payload_sent: bool,
    refusal_streamed: bool,
    slots: Vec<ToolSlot>,
    /// `item_id` / `call_id` → 槽位下标。两者都登记，因为上游各家事件里
    /// 只保证其中之一出现。
    slot_by_key: HashMap<String, usize>,
    last_slot: Option<usize>,
    usage: Option<Value>,
    /// 因始终拿不到函数名而被丢弃的工具调用数。
    dropped_tool_calls: usize,
}
impl ResponsesToChatState {
    fn envelope(&self) -> Value {
        json!({
            "id": if self.chat_id.is_empty() { DEFAULT_CHAT_ID } else { self.chat_id.as_str() },
            "object": CHAT_CHUNK_OBJECT,
            "created": self.created,
            "model": self.model,
        })
    }

    /// 普通增量块。首块自动补 `role`。
    fn delta_chunk(&mut self, mut delta: Value) -> Bytes {
        if !self.role_sent {
            if let Some(obj) = delta.as_object_mut() {
                obj.insert("role".to_string(), json!("assistant"));
            }
            self.role_sent = true;
        }

        let mut chunk = self.envelope();
        chunk["choices"] = json!([{
            "index": 0,
            "delta": delta,
            "finish_reason": Value::Null,
        }]);
        sse_data(&chunk)
    }

    /// 终止块：`finish_reason` + usage（若上游给过）。
    ///
    /// 空 delta 也要补 `role`：上游只发终态、一个增量都没发时，客户端拿到的
    /// 首块就是这一块，缺 role 会让严格解析的 SDK 报错。
    fn finish_chunk(&mut self, finish_reason: &str) -> Bytes {
        let mut delta = json!({});
        if !self.role_sent {
            delta["role"] = json!("assistant");
            self.role_sent = true;
        }

        let mut chunk = self.envelope();
        chunk["choices"] = json!([{
            "index": 0,
            "delta": delta,
            "finish_reason": finish_reason,
        }]);
        if let Some(usage) = self.usage.clone() {
            chunk["usage"] = usage;
        }
        sse_data(&chunk)
    }

    /// 错误块。发出后 `terminated` + `failed` 同时置位，从此不再产出
    /// finish_reason——上游失败绝不能被还原成正常收尾。
    fn error_chunk(&mut self, message: String, error_type: &str) -> Vec<Bytes> {
        self.terminated = true;
        self.failed = true;
        let payload = json!({
            "error": {
                "message": message,
                "type": error_type,
                "code": Value::Null,
                "param": Value::Null,
            }
        });
        // `[DONE]` 仍然补发：客户端据它关闭连接，不补会让等终止标记的
        // SDK 一直挂着。它不是"完成事件"——错误块已经把本回合判成失败。
        vec![sse_named_event("error", &payload), done_marker()]
    }
}
impl ResponsesToChatState {
    /// 按 `item_id` / `call_id` 找槽位，找不到就新建。
    ///
    /// 找不到时**新建**而不是丢弃：孤立的 arguments 分片（上游漏发
    /// `output_item.added`）如果被丢掉，客户端会收到一个没有入参的工具调用，
    /// 那是比报错更难诊断的失败。
    fn slot_for(&mut self, keys: &[&str]) -> usize {
        for key in keys.iter().filter(|key| !key.is_empty()) {
            if let Some(index) = self.slot_by_key.get(*key) {
                return *index;
            }
        }

        let index = self.slots.len();
        self.slots.push(ToolSlot {
            index,
            ..ToolSlot::default()
        });
        for key in keys.iter().filter(|key| !key.is_empty()) {
            self.slot_by_key.insert((*key).to_string(), index);
        }
        self.last_slot = Some(index);
        index
    }

    fn register_slot_keys(&mut self, index: usize, keys: &[&str]) {
        for key in keys.iter().filter(|key| !key.is_empty()) {
            self.slot_by_key.insert((*key).to_string(), index);
        }
    }

    /// 尝试下发槽位 header + 已缓存的 arguments。名字未知时什么都不发。
    fn flush_slot(&mut self, index: usize) -> Vec<Bytes> {
        let Some(slot) = self.slots.get(index) else {
            return Vec::new();
        };
        if slot.name.trim().is_empty() {
            return Vec::new();
        }

        let mut events = Vec::new();
        if !slot.header_sent {
            let is_custom = slot.is_custom;
            let (chat_index, call_id, name) = (slot.index, slot.call_id.clone(), slot.name.clone());
            // call_id 缺失时按槽位下标合成：空 id 会破坏客户端的
            // tool_call_id ↔ tool 结果回程。
            let call_id = if call_id.trim().is_empty() {
                format!("call_{chat_index}")
            } else {
                call_id
            };
            if let Some(slot) = self.slots.get_mut(index) {
                slot.call_id.clone_from(&call_id);
                slot.header_sent = true;
            }
            let call = if is_custom {
                json!({"index":chat_index,"id":call_id,"type":"custom","custom":{"name":name,"input":""}})
            } else {
                json!({"index":chat_index,"id":call_id,"type":"function","function":{"name":name,"arguments":""}})
            };
            events.push(self.delta_chunk(json!({"tool_calls":[call]})));
        }

        let pending = self
            .slots
            .get_mut(index)
            .map(|slot| std::mem::take(&mut slot.pending_args))
            .unwrap_or_default();
        if !pending.is_empty() {
            let chat_index = self.slots[index].index;
            if let Some(slot) = self.slots.get_mut(index) {
                slot.args_streamed = true;
            }
            let call = if self.slots[index].is_custom {
                json!({"index":chat_index,"custom":{"input":pending}})
            } else {
                json!({"index":chat_index,"function":{"arguments":pending}})
            };
            events.push(self.delta_chunk(json!({"tool_calls":[call]})));
        }

        events
    }
}
impl ResponsesToChatState {
    /// Match the non-streaming Chat custom tool shape; never disguise it as a function.
    fn flush_custom_input(&mut self, index: usize) -> Vec<Bytes> {
        let Some(slot) = self.slots.get(index) else {
            return Vec::new();
        };
        if !slot.is_custom || slot.args_streamed || slot.custom_input.is_empty() {
            return Vec::new();
        }

        let input = self.slots[index].custom_input.clone();
        if let Some(slot) = self.slots.get_mut(index) {
            slot.pending_args.push_str(&input);
        }
        self.flush_slot(index)
    }

    /// 收尾所有槽位：补 header / arguments，丢弃始终没有函数名的调用。
    fn finalize_tools(&mut self) -> Vec<Bytes> {
        let mut events = Vec::new();
        for index in 0..self.slots.len() {
            events.extend(self.flush_custom_input(index));
            events.extend(self.flush_slot(index));

            let Some(slot) = self.slots.get(index) else {
                continue;
            };
            if !slot.name.trim().is_empty() {
                continue;
            }
            // 没有函数名的调用对应不到客户端声明的任何工具，发出去只会让
            // 客户端拿到一个无法执行的调用。丢弃并计数，由调用方决定报错。
            let has_payload = !slot.pending_args.trim().is_empty()
                || !slot.custom_input.trim().is_empty()
                || !slot.call_id.trim().is_empty();
            if has_payload {
                self.dropped_tool_calls += 1;
                // 只记结构信息：arguments 可能含用户代码，日志出口是 allowlist
                // 脱敏，新字段不进白名单就不会被处理，因此只输出字节数。
                log::warn!(
                    "[ChatEntry] dropped streaming tool call: model={} slot={} \
                     call_id_empty={} args_bytes={} slots_total={}",
                    self.model,
                    index,
                    slot.call_id.trim().is_empty(),
                    slot.pending_args.len() + slot.custom_input.len(),
                    self.slots.len()
                );
            }
        }
        events
    }

    fn has_emitted_tool_call(&self) -> bool {
        self.slots.iter().any(|slot| slot.header_sent)
    }
}
impl ResponsesToChatState {
    fn reasoning_metadata(&mut self, item: &Value) -> Vec<Bytes> {
        if self.reasoning_payload_sent {
            return Vec::new();
        }
        let Some(encrypted) = item
            .get("encrypted_content")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            return Vec::new();
        };
        let mut reasoning = json!({"encrypted_content":encrypted});
        if let Some(id) = item.get("id") {
            reasoning["id"] = id.clone();
        }
        self.reasoning_payload_sent = true;
        vec![self.delta_chunk(json!({"reasoning":reasoning}))]
    }

    /// 用终态 `response.output[]` 补齐流里没出现过的内容。
    ///
    /// 存在的理由：不少兼容网关只发 `response.completed`（增量事件一个不发），
    /// 或发了工具调用的 `output_item.added` 却从不发 arguments 增量。少了这次
    /// 兜底，客户端会拿到一个"成功但空"的回答——本模块最需要避免的形态。
    fn reconcile_terminal_output(&mut self, response: &Value) -> Vec<Bytes> {
        let Some(output) = response.get("output").and_then(Value::as_array) else {
            return Vec::new();
        };

        let mut reasoning = String::new();
        let mut text = String::new();
        let mut refusal = String::new();
        let mut metadata_events = Vec::new();

        for item in output {
            match item.get("type").and_then(Value::as_str).unwrap_or("") {
                "message" => {
                    if !self.text_streamed {
                        text.push_str(&message_item_text(item));
                    }
                    if !self.refusal_streamed {
                        if let Some(parts) = item.get("content").and_then(Value::as_array) {
                            for part in parts {
                                if let Some(value) = part.get("refusal").and_then(Value::as_str) {
                                    refusal.push_str(value);
                                }
                            }
                        }
                    }
                }
                "reasoning" => {
                    metadata_events.extend(self.reasoning_metadata(item));
                    if !self.reasoning_streamed {
                        if let Some(summary) = extract_reasoning_summary_text(item) {
                            reasoning.push_str(&summary);
                        }
                    }
                }
                "function_call" | "custom_tool_call" | "tool_search_call" => {
                    self.absorb_tool_item(item);
                }
                _ => {}
            }
        }

        let mut events = Vec::new();
        // 推理先于正文：与 Responses 的 output 顺序一致，也是 Chat 客户端
        // 渲染思考过程的期待顺序。
        if !reasoning.is_empty() {
            self.has_output = true;
            events.push(self.delta_chunk(json!({ "reasoning_content": reasoning })));
        }
        if !text.is_empty() {
            self.has_output = true;
            events.push(self.delta_chunk(json!({ "content": text })));
        }
        if !refusal.is_empty() {
            self.has_output = true;
            events.push(self.delta_chunk(json!({"refusal":refusal})));
        }
        events.extend(metadata_events);
        events
    }

    /// 把一个 Responses 工具调用 item 并入槽位（不下发事件，由 `finalize_tools` 统一发）。
    fn absorb_tool_item(&mut self, item: &Value) {
        let item_id = item.get("id").and_then(Value::as_str).unwrap_or("");
        let call_id = item.get("call_id").and_then(Value::as_str).unwrap_or("");
        let index = self.slot_for(&[item_id, call_id]);
        self.register_slot_keys(index, &[item_id, call_id]);

        let is_custom = item.get("type").and_then(Value::as_str) == Some("custom_tool_call");
        let name = tool_item_chat_name(item);
        let arguments = tool_item_arguments(item);

        let Some(slot) = self.slots.get_mut(index) else {
            return;
        };
        if slot.call_id.trim().is_empty() && !call_id.is_empty() {
            slot.call_id = call_id.to_string();
        }
        if slot.name.trim().is_empty() {
            if let Some(name) = name {
                slot.name = name;
            }
        }
        slot.is_custom |= is_custom;
        // 已经流过分片就不能再累加终态全量，否则 arguments 翻倍成非法 JSON。
        if !slot.args_streamed && slot.pending_args.is_empty() {
            if is_custom {
                if slot.custom_input.is_empty() {
                    slot.custom_input = arguments;
                }
            } else if !arguments.is_empty() {
                slot.pending_args = arguments;
            }
        }
        self.has_output = true;
    }
}
impl ResponsesToChatState {
    /// 从任意事件里吸收信封字段（id / model / created / usage）。
    ///
    /// 每个事件都做：`response.created` 可能整个缺失（部分网关直接从增量开始），
    /// 而 usage 通常只在终态出现。
    fn ingest_envelope(&mut self, data: &Value) {
        let response = data.get("response").unwrap_or(data);

        if self.chat_id.is_empty() {
            if let Some(id) = response
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
            {
                self.chat_id = chat_id_from_response_id(id);
            }
        }
        if self.model.is_empty() {
            if let Some(model) = response
                .get("model")
                .and_then(Value::as_str)
                .filter(|model| !model.is_empty())
            {
                self.model = model.to_string();
            }
        }
        if self.created == 0 {
            if let Some(created) = response.get("created_at").and_then(Value::as_u64) {
                self.created = created;
            }
        }
        if let Some(usage) = responses_usage_to_chat_usage(response.get("usage")) {
            self.usage = Some(usage);
        }
    }

    fn handle_event(&mut self, event_name: &str, data: &Value) -> Vec<Bytes> {
        self.ingest_envelope(data);

        match event_name {
            "response.output_text.delta" | "response.refusal.delta" => {
                let Some(delta) = data.get("delta").and_then(Value::as_str) else {
                    return Vec::new();
                };
                if delta.is_empty() {
                    return Vec::new();
                }
                self.has_output = true;
                if event_name == "response.refusal.delta" {
                    self.refusal_streamed = true;
                    vec![self.delta_chunk(json!({ "refusal": delta }))]
                } else {
                    self.text_streamed = true;
                    vec![self.delta_chunk(json!({ "content": delta }))]
                }
            }

            "response.reasoning_summary_text.delta"
            | "response.reasoning_text.delta"
            | "response.reasoning.delta" => {
                let Some(delta) = data.get("delta").and_then(Value::as_str) else {
                    return Vec::new();
                };
                if delta.is_empty() {
                    return Vec::new();
                }
                self.has_output = true;
                self.reasoning_streamed = true;
                vec![self.delta_chunk(json!({ "reasoning_content": delta }))]
            }

            "response.output_item.added" | "response.output_item.done" => {
                let Some(item) = data.get("item") else {
                    return Vec::new();
                };
                match item.get("type").and_then(Value::as_str).unwrap_or("") {
                    "reasoning" => self.reasoning_metadata(item),
                    "function_call" | "custom_tool_call" | "tool_search_call" => {
                        self.absorb_tool_item(item);
                        let index = self.slot_for(&[
                            item.get("id").and_then(Value::as_str).unwrap_or(""),
                            item.get("call_id").and_then(Value::as_str).unwrap_or(""),
                        ]);
                        self.last_slot = Some(index);
                        // done 事件才收口 custom 入参：added 时入参还没到齐。
                        if event_name == "response.output_item.done" {
                            let mut events = self.flush_custom_input(index);
                            events.extend(self.flush_slot(index));
                            events
                        } else {
                            self.flush_slot(index)
                        }
                    }
                    _ => Vec::new(),
                }
            }
            "response.function_call_arguments.delta" => {
                let Some(delta) = data.get("delta").and_then(Value::as_str) else {
                    return Vec::new();
                };
                if delta.is_empty() {
                    return Vec::new();
                }
                let index = self.resolve_event_slot(data);
                self.has_output = true;
                if let Some(slot) = self.slots.get_mut(index) {
                    slot.pending_args.push_str(delta);
                }
                self.flush_slot(index)
            }

            "response.function_call_arguments.done" => {
                let index = self.resolve_event_slot(data);
                let arguments = data
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                self.has_output = true;
                // 只在一个分片都没发过时用全量补：发过就已经是完整拼接，
                // 再追加会把 arguments 变成两份 JSON 拼接的非法串。
                if let Some(slot) = self.slots.get_mut(index) {
                    if !slot.args_streamed && slot.pending_args.is_empty() && !arguments.is_empty()
                    {
                        slot.pending_args = arguments;
                    }
                }
                self.flush_slot(index)
            }

            "response.custom_tool_call_input.delta" => {
                let Some(delta) = data.get("delta").and_then(Value::as_str) else {
                    return Vec::new();
                };
                let index = self.resolve_event_slot(data);
                self.has_output = true;
                // Retain input until its terminal item establishes a complete custom call.
                if let Some(slot) = self.slots.get_mut(index) {
                    slot.is_custom = true;
                    slot.custom_input.push_str(delta);
                }
                Vec::new()
            }

            "response.custom_tool_call_input.done" => {
                let index = self.resolve_event_slot(data);
                let input = data
                    .get("input")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                self.has_output = true;
                if let Some(slot) = self.slots.get_mut(index) {
                    slot.is_custom = true;
                    if slot.custom_input.is_empty() {
                        slot.custom_input = input;
                    }
                }
                self.flush_custom_input(index)
            }

            "response.completed" | "response.incomplete" => self.handle_terminal(event_name, data),

            "response.failed" | "error" => {
                let (message, error_type) = responses_error_details(
                    data,
                    if event_name == "response.failed" {
                        "Responses upstream reported response.failed"
                    } else {
                        "Responses upstream emitted an error event"
                    },
                );
                self.error_chunk(message, &error_type)
            }
            _ => Vec::new(),
        }
    }
}
impl ResponsesToChatState {
    /// arguments 类事件的槽位定位：`item_id` / `call_id` 优先，退回最后一个
    /// 活跃槽位（上游漏发 item 身份时的常见形态）。
    fn resolve_event_slot(&mut self, data: &Value) -> usize {
        let item_id = data.get("item_id").and_then(Value::as_str).unwrap_or("");
        let call_id = data.get("call_id").and_then(Value::as_str).unwrap_or("");

        for key in [item_id, call_id] {
            if key.is_empty() {
                continue;
            }
            if let Some(index) = self.slot_by_key.get(key) {
                let index = *index;
                self.last_slot = Some(index);
                return index;
            }
        }
        if item_id.is_empty() && call_id.is_empty() {
            if let Some(index) = self.last_slot {
                return index;
            }
        }

        let index = self.slot_for(&[item_id, call_id]);
        // 上游只在增量事件里带 name 时（无 output_item.added）也要能立住身份。
        if let Some(name) = data
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
        {
            if let Some(slot) = self.slots.get_mut(index) {
                slot.name = name.to_string();
            }
        }
        if !call_id.is_empty() {
            if let Some(slot) = self.slots.get_mut(index) {
                slot.call_id = call_id.to_string();
            }
        }
        index
    }

    /// `response.completed` / `response.incomplete`。
    fn handle_terminal(&mut self, event_name: &str, data: &Value) -> Vec<Bytes> {
        let response = data.get("response").unwrap_or(data);

        // 终态里夹带失败：`status=failed/cancelled` 或非空 `error`。事件名叫
        // completed 不代表成功，据此判完成会把失败伪装成正常回答。
        let status = response.get("status").and_then(Value::as_str);
        if matches!(status, Some("failed" | "cancelled"))
            || response.get("error").is_some_and(|error| !error.is_null())
        {
            let (message, error_type) = responses_error_details(
                data,
                "Responses upstream returned a failed terminal response",
            );
            return self.error_chunk(message, &error_type);
        }

        let mut events = self.reconcile_terminal_output(response);
        events.extend(self.finalize_tools());

        let status = status.unwrap_or(match event_name {
            "response.incomplete" => "incomplete",
            _ => "completed",
        });

        // 丢弃过工具调用、且最终一个都没剩下时，客户端会收到"成功但什么都没做"
        // 的一轮，agent loop 静默收尾。此时如实报错而不是谎报成功。
        // 只对本应 completed 的回合生效：incomplete 有自己正当的终止解释
        // （截断），报成 dropped 会给出错误归因。
        if status == "completed" && self.dropped_tool_calls > 0 && !self.has_emitted_tool_call() {
            let dropped = self.dropped_tool_calls;
            let error = self.error_chunk(
                format!(
                    "Upstream returned {dropped} tool call(s) without a function name, \
                     leaving no usable tool call in this turn"
                ),
                "upstream_tool_call_dropped",
            );
            events.extend(error);
            return events;
        }

        let finish_reason = chat_finish_reason(
            status,
            self.has_emitted_tool_call(),
            response
                .pointer("/incomplete_details/reason")
                .and_then(Value::as_str),
        );
        events.push(self.finish_chunk(finish_reason));
        events.push(done_marker());
        self.terminated = true;
        events
    }

    /// 流结束但没收到任何终态事件。
    fn finalize_truncated(&mut self) -> Vec<Bytes> {
        let mut events = self.finalize_tools();

        if !self.has_output {
            // 一个字都没收到就断了：报错。补 finish_reason 会让客户端以为
            // 模型选择了沉默。
            events.extend(self.error_chunk(
                "Upstream Responses stream ended before sending a terminal event".to_string(),
                "stream_truncated",
            ));
            return events;
        }

        // A disconnected transport is not a model token limit. Preserve the
        // partial output but surface an error, never synthesize a success finish.
        events.extend(self.error_chunk(
            "Upstream Responses stream ended before sending a terminal event".into(),
            "stream_truncated",
        ));
        events
    }
}
/// `response_id_from_chat_id` 的逆向：剥掉 `resp_` 前缀。
fn chat_id_from_response_id(id: &str) -> String {
    match id.strip_prefix("resp_") {
        Some(rest) if !rest.is_empty() => rest.to_string(),
        _ => id.to_string(),
    }
}

/// Responses 终态 status → Chat `finish_reason`。
fn chat_finish_reason(
    status: &str,
    has_tool_calls: bool,
    incomplete_reason: Option<&str>,
) -> &'static str {
    match status {
        "completed" if has_tool_calls => "tool_calls",
        "incomplete" => match incomplete_reason {
            Some("content_filter") => "content_filter",
            // reason 缺失时也按 token 上限：Responses 的 incomplete 绝大多数
            // 是 max_output_tokens，且 `length` 比 `stop` 保守。
            _ => "length",
        },
        _ => "stop",
    }
}

/// Responses usage → Chat usage（`chat_usage_to_responses_usage` 的逆向）。
///
/// 返回 `None` 表示"上游没给可用的 usage"，此时不能往块里塞一个全 0 的 usage：
/// 客户端会把它当成真实计量记账。
fn responses_usage_to_chat_usage(usage: Option<&Value>) -> Option<Value> {
    let usage = usage.filter(|value| value.is_object())?;

    let input_tokens = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(Value::as_u64);
    let output_tokens = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(Value::as_u64);
    let total_tokens = usage.get("total_tokens").and_then(Value::as_u64);

    // 三个字段全缺：这是个空壳 usage（部分网关每个事件都带 `usage:{}`），
    // 不能据此覆盖掉之前拿到的真实值。
    if input_tokens.is_none() && output_tokens.is_none() && total_tokens.is_none() {
        return None;
    }

    let input_tokens = input_tokens.unwrap_or(0);
    let output_tokens = output_tokens.unwrap_or(0);
    let mut chat_usage = json!({
        "prompt_tokens": input_tokens,
        "completion_tokens": output_tokens,
        "total_tokens": total_tokens.unwrap_or(input_tokens + output_tokens),
    });

    if let Some(cached) = usage
        .pointer("/input_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
    {
        chat_usage["prompt_tokens_details"] = json!({ "cached_tokens": cached });
    }
    if let Some(reasoning) = usage
        .pointer("/output_tokens_details/reasoning_tokens")
        .and_then(Value::as_u64)
    {
        chat_usage["completion_tokens_details"] = json!({ "reasoning_tokens": reasoning });
    }

    Some(chat_usage)
}

/// Responses 工具 item → Chat 可见的函数名。
fn tool_item_chat_name(item: &Value) -> Option<String> {
    if item.get("type").and_then(Value::as_str) == Some("tool_search_call") {
        return Some("tool_search".to_string());
    }

    let name = item
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())?;

    // 带 namespace 的 item 要还原成 Chat 侧的扁平名，否则客户端对不上
    // 自己声明的工具名（与 `flatten_namespace_tool_name` 同一算法）。
    match item
        .get("namespace")
        .and_then(Value::as_str)
        .filter(|namespace| !namespace.is_empty())
    {
        Some(namespace) => Some(flatten_namespace_tool_name(namespace, name)),
        None => Some(name.to_string()),
    }
}

/// Responses 工具 item → Chat arguments 字符串。
///
/// `arguments` 在 Responses 里既可能是字符串也可能是对象（`tool_search_call`
/// 用对象），`custom_tool_call` 则用 `input` 承载裸文本。
fn tool_item_arguments(item: &Value) -> String {
    let raw = item
        .get("arguments")
        .or_else(|| item.get("input"))
        .unwrap_or(&Value::Null);

    match raw {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Responses `message` item → 可见文本。
fn message_item_text(item: &Value) -> String {
    let Some(parts) = item.get("content").and_then(Value::as_array) else {
        return String::new();
    };
    parts
        .iter()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .concat()
}

/// 从 Responses 错误事件里提取 message / type。
fn responses_error_details(data: &Value, fallback: &str) -> (String, String) {
    let response = data.get("response").unwrap_or(data);
    let error = response.get("error").unwrap_or(response);
    let message = error
        .get("message")
        .or_else(|| error.get("detail"))
        .and_then(Value::as_str)
        .or_else(|| error.as_str())
        .filter(|message| !message.trim().is_empty())
        .unwrap_or(fallback)
        .to_string();
    let error_type = error
        .get("type")
        .or_else(|| error.get("code"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("upstream_error")
        .to_string();

    (message, error_type)
}
/// 把上游的 Responses SSE 流转换为 Chat Completions SSE 流。
///
/// 错误事件必须映射为 Chat 侧的错误块，且不得在 `failed` 之后再补 `[DONE]`
/// 之外的完成事件——参照 `streaming_codex_chat.rs` 的 `failed` 处理。
pub fn create_chat_sse_stream_from_responses<E: std::error::Error + Send + 'static>(
    stream: impl Stream<Item = Result<Bytes, E>> + Send + 'static,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static {
    async_stream::stream! {
        let mut buffer = String::new();
        let mut utf8_remainder: Vec<u8> = Vec::new();
        let mut state = ResponsesToChatState::default();

        // 追加 EOF 哨兵：最后一个事件可能没有结尾空行（非规范上游 / 被掐断的
        // 连接），少了这一步尾部的 response.completed 会被整块丢掉。布尔位用来
        // 区分哨兵和上游真的发来的空 chunk。
        let stream = stream
            .map(|result| (result, false))
            .chain(futures::stream::once(async {
                (Ok::<Bytes, E>(Bytes::new()), true)
            }));
        tokio::pin!(stream);

        while let Some((chunk, is_eof)) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    // SSE 块可能跨 TCP chunk 到达，且多字节字符可能被切断，
                    // 必须走 UTF-8 安全累积。
                    append_utf8_safe(&mut buffer, &mut utf8_remainder, &bytes);

                    if is_eof && !buffer.trim().is_empty() {
                        buffer.push_str("\n\n");
                    }

                    while let Some(block) = take_sse_block(&mut buffer) {
                        if block.trim().is_empty() {
                            continue;
                        }

                        let mut event_name: Option<String> = None;
                        let mut data_parts: Vec<String> = Vec::new();
                        for line in block.lines() {
                            if let Some(event) = strip_sse_field(line, "event") {
                                event_name = Some(event.trim().to_string());
                            } else if let Some(data) = strip_sse_field(line, "data") {
                                data_parts.push(data.to_string());
                            }
                        }

                        if data_parts.is_empty() {
                            continue;
                        }

                        let data_str = data_parts.join("\n");
                        // 上游自己的 `[DONE]`：Responses 规范里没有，但兼容网关会带。
                        // 当结束信号处理，真正的收尾判定留给流末尾统一逻辑。
                        if data_str.trim() == "[DONE]" {
                            continue;
                        }

                        // 非 JSON 噪声行（keepalive 注释、半截数据）直接跳过，
                        // 不能 panic 也不能据此判完成。
                        let data: Value = match serde_json::from_str(&data_str) {
                            Ok(value) => value,
                            Err(_) => continue,
                        };

                        // 终止之后到达的迟到事件一律忽略：错误已经发出，
                        // 再处理任何事件都可能补出一个正常完成块。
                        if state.terminated {
                            continue;
                        }

                        // 官方流同时带 `event:` 与 payload 的 `type`，兼容网关常常
                        // 只有其中之一，两边都取。
                        let resolved = event_name
                            .as_deref()
                            .filter(|name| !name.is_empty())
                            .or_else(|| data.get("type").and_then(Value::as_str))
                            .unwrap_or("")
                            .to_string();

                        // 只有 `data:` 没有 `event:` 的错误体（`{"error":{...}}`）：
                        // 事件名认不出来，但绝不能当噪声跳过——那正是把上游失败
                        // 伪装成成功的路径。
                        let is_bare_error = !matches!(resolved.as_str(), "response.failed" | "error")
                            && data
                                .get("error")
                                .is_some_and(|error| !error.is_null());
                        let resolved = if is_bare_error { "error".to_string() } else { resolved };

                        for event in state.handle_event(&resolved, &data) {
                            yield Ok(event);
                        }
                    }

                    if state.terminated {
                        break;
                    }
                }
                Err(error) => {
                    // 传输层中断：如实报错，不补完成事件。
                    for event in state.error_chunk(
                        format!("Stream error: {error}"),
                        "stream_error",
                    ) {
                        yield Ok(event);
                    }
                    break;
                }
            }
        }

        if !state.terminated {
            for event in state.finalize_truncated() {
                yield Ok(event);
            }
        }
    }
}

/// OpenAI stream_options.include_usage uses a separate, choices:[] usage chunk.
/// Keep accounting data in the core converter and apply this only at the wire edge.
pub fn create_chat_sse_stream_from_responses_with_options<E: std::error::Error + Send + 'static>(
    stream: impl Stream<Item = Result<Bytes, E>> + Send + 'static,
    include_usage: bool,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static {
    async_stream::stream! {
        let stream = create_chat_sse_stream_from_responses(stream);
        tokio::pin!(stream);
        while let Some(chunk) = stream.next().await {
            let bytes = match chunk { Ok(bytes) => bytes, Err(error) => { yield Err(error); break; } };
            if let Some(payload) = bytes.strip_prefix(b"data: ") {
                if let Ok(mut value) = serde_json::from_slice::<Value>(payload) {
                    if let Some(usage) = value.as_object_mut().and_then(|obj| obj.remove("usage")) {
                        yield Ok(sse_data(&value));
                        if include_usage {
                            value["choices"] = json!([]);
                            value["usage"] = usage;
                            yield Ok(sse_data(&value));
                        }
                        continue;
                    }
                }
            }
            yield Ok(bytes);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use futures::{stream, StreamExt};

    async fn collect(chunks: Vec<&str>) -> String {
        let chunks: Vec<Result<Bytes, std::io::Error>> = chunks
            .into_iter()
            .map(|chunk| Ok(Bytes::copy_from_slice(chunk.as_bytes())))
            .collect();
        collect_stream(stream::iter(chunks)).await
    }

    async fn collect_stream(
        upstream: impl Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static,
    ) -> String {
        let converted = create_chat_sse_stream_from_responses(upstream);
        let bytes: Vec<Bytes> = converted.map(|item| item.unwrap()).collect().await;
        String::from_utf8(bytes.concat()).unwrap()
    }

    /// 解析出所有 `data:` 载荷（跳过 `[DONE]`）。
    fn parse_chunks(output: &str) -> Vec<Value> {
        output
            .split("\n\n")
            .filter_map(|block| {
                let data = block
                    .lines()
                    .find_map(|line| strip_sse_field(line, "data"))?
                    .trim();
                if data == "[DONE]" {
                    return None;
                }
                serde_json::from_str(data).ok()
            })
            .collect()
    }

    fn finish_reasons(chunks: &[Value]) -> Vec<String> {
        chunks
            .iter()
            .filter_map(|chunk| {
                chunk
                    .pointer("/choices/0/finish_reason")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })
            .collect()
    }

    /// 把所有 `delta.<field>` 拼起来，验证增量的累积结果。
    fn joined_delta(chunks: &[Value], field: &str) -> String {
        chunks
            .iter()
            .filter_map(|chunk| {
                chunk
                    .pointer(&format!("/choices/0/delta/{field}"))
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>()
            .concat()
    }
    #[tokio::test]
    async fn converts_text_deltas_with_role_on_first_chunk() {
        let output = collect(vec![
            "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_abc\",\"model\":\"gpt-5.4\",\"created_at\":123,\"status\":\"in_progress\"}}\n\n",
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"delta\":\"Hel\"}\n\n",
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"delta\":\"lo\"}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_abc\",\"model\":\"gpt-5.4\",\"created_at\":123,\"status\":\"completed\",\"output\":[]}}\n\n",
        ])
        .await;
        let chunks = parse_chunks(&output);

        assert_eq!(joined_delta(&chunks, "content"), "Hello");
        assert_eq!(chunks[0]["choices"][0]["delta"]["role"], "assistant");
        // 只有首块带 role。
        assert_eq!(
            chunks
                .iter()
                .filter(|chunk| chunk.pointer("/choices/0/delta/role").is_some())
                .count(),
            1
        );
        for chunk in &chunks {
            assert_eq!(chunk["object"], CHAT_CHUNK_OBJECT);
            assert_eq!(chunk["model"], "gpt-5.4");
            assert_eq!(chunk["created"], 123);
            // `resp_` 前缀被剥掉，还原成 Chat 侧的 id。
            assert_eq!(chunk["id"], "abc");
            assert_eq!(chunk["choices"][0]["index"], 0);
        }
        assert_eq!(finish_reasons(&chunks), vec!["stop"]);
        assert!(output.trim_end().ends_with("data: [DONE]"));
    }

    #[tokio::test]
    async fn accumulates_tool_call_argument_fragments() {
        let output = collect(vec![
            "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_tool\",\"model\":\"gpt-5.4\",\"created_at\":1,\"status\":\"in_progress\"}}\n\n",
            "event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"fc_call_1\",\"type\":\"function_call\",\"status\":\"in_progress\",\"call_id\":\"call_1\",\"name\":\"get_weather\",\"arguments\":\"\"}}\n\n",
            "event: response.function_call_arguments.delta\ndata: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_call_1\",\"output_index\":0,\"delta\":\"{\\\"city\\\":\"}\n\n",
            "event: response.function_call_arguments.delta\ndata: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_call_1\",\"output_index\":0,\"delta\":\"\\\"Tokyo\\\"}\"}\n\n",
            "event: response.function_call_arguments.done\ndata: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"fc_call_1\",\"output_index\":0,\"arguments\":\"{\\\"city\\\":\\\"Tokyo\\\"}\"}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_tool\",\"model\":\"gpt-5.4\",\"created_at\":1,\"status\":\"completed\",\"output\":[{\"id\":\"fc_call_1\",\"type\":\"function_call\",\"status\":\"completed\",\"call_id\":\"call_1\",\"name\":\"get_weather\",\"arguments\":\"{\\\"city\\\":\\\"Tokyo\\\"}\"}]}}\n\n",
        ])
        .await;
        let chunks = parse_chunks(&output);

        let tool_deltas: Vec<&Value> = chunks
            .iter()
            .filter_map(|chunk| chunk.pointer("/choices/0/delta/tool_calls/0"))
            .collect();
        assert!(!tool_deltas.is_empty());
        // 身份只在首个 tool_calls 块出现一次。
        assert_eq!(tool_deltas[0]["index"], 0);
        assert_eq!(tool_deltas[0]["id"], "call_1");
        assert_eq!(tool_deltas[0]["type"], "function");
        assert_eq!(tool_deltas[0]["function"]["name"], "get_weather");

        let arguments = tool_deltas
            .iter()
            .filter_map(|delta| delta.pointer("/function/arguments").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .concat();
        // `.done` 的全量不得在分片之后重复累加。
        assert_eq!(arguments, r#"{"city":"Tokyo"}"#);
        assert_eq!(finish_reasons(&chunks), vec!["tool_calls"]);
    }

    #[tokio::test]
    async fn maps_reasoning_summary_to_reasoning_content() {
        let output = collect(vec![
            "event: response.reasoning_summary_text.delta\ndata: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rs_1\",\"delta\":\"Need \"}\n\n",
            "event: response.reasoning_summary_text.delta\ndata: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rs_1\",\"delta\":\"context.\"}\n\n",
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"delta\":\"Done\"}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_r\",\"model\":\"deepseek-reasoner\",\"created_at\":2,\"status\":\"completed\",\"output\":[]}}\n\n",
        ])
        .await;
        let chunks = parse_chunks(&output);

        assert_eq!(joined_delta(&chunks, "reasoning_content"), "Need context.");
        assert_eq!(joined_delta(&chunks, "content"), "Done");
        assert_eq!(finish_reasons(&chunks), vec!["stop"]);
    }
    /// `response.failed` 必须变成错误块，且绝不能再补正常完成事件。
    /// 与 `streaming_codex_chat` 的 `chat_sse_error_event_emits_failed_without_completed`
    /// 互为镜像。
    #[tokio::test]
    async fn response_failed_emits_error_without_finish_reason() {
        let output = collect(vec![
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"delta\":\"partial\"}\n\n",
            "event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"id\":\"resp_f\",\"model\":\"gpt-5.4\",\"status\":\"failed\",\"error\":{\"message\":\"bad request\",\"type\":\"invalid_request_error\"}}}\n\n",
            // 迟到的完成事件也不许把失败翻成成功。
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_f\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"output\":[]}}\n\n",
        ])
        .await;
        let chunks = parse_chunks(&output);

        assert!(output.contains("event: error"));
        assert!(output.contains("bad request"));
        assert!(output.contains("invalid_request_error"));
        assert!(finish_reasons(&chunks).is_empty());
        // 已推给客户端的增量不受影响。
        assert_eq!(joined_delta(&chunks, "content"), "partial");
    }

    /// 只有 `data:` 的错误体（没有 `event:` 行）同样不许伪装成成功。
    #[tokio::test]
    async fn data_only_error_emits_error_without_finish_reason() {
        let output = collect(vec![
            "data: {\"error\":{\"message\":\"quota exceeded\",\"code\":\"rate_limit_exceeded\"}}\n\n",
            "data: [DONE]\n\n",
        ])
        .await;
        let chunks = parse_chunks(&output);

        assert!(output.contains("event: error"));
        assert!(output.contains("quota exceeded"));
        assert!(output.contains("rate_limit_exceeded"));
        assert!(finish_reasons(&chunks).is_empty());
    }

    /// `status=failed` 藏在 `response.completed` 里（网关常见形态）：
    /// 事件名叫 completed 也必须判失败。
    #[tokio::test]
    async fn completed_event_carrying_failed_status_emits_error() {
        let output = collect(vec![
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_x\",\"model\":\"gpt-5.4\",\"status\":\"failed\",\"error\":{\"message\":\"upstream overloaded\",\"type\":\"server_error\"},\"output\":[]}}\n\n",
        ])
        .await;
        let chunks = parse_chunks(&output);

        assert!(output.contains("event: error"));
        assert!(output.contains("upstream overloaded"));
        assert!(finish_reasons(&chunks).is_empty());
    }

    /// SSE 块跨 TCP chunk 分片（含被切断的多字节字符）时必须能正确重组。
    #[tokio::test]
    async fn reassembles_sse_blocks_split_across_chunks() {
        let full = concat!(
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"delta\":\"你好世界\"}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_split\",\"model\":\"gpt-5.4\",\"created_at\":9,\"status\":\"completed\",\"output\":[]}}\n\n",
        );
        // 逐字节喂入：既跨块切 SSE 分隔符，也切多字节 UTF-8 序列。
        let chunks: Vec<Result<Bytes, std::io::Error>> = full
            .as_bytes()
            .chunks(1)
            .map(|byte| Ok(Bytes::copy_from_slice(byte)))
            .collect();
        let output = collect_stream(stream::iter(chunks)).await;
        let parsed = parse_chunks(&output);

        assert_eq!(joined_delta(&parsed, "content"), "你好世界");
        assert_eq!(finish_reasons(&parsed), vec!["stop"]);
    }
    #[tokio::test]
    async fn usage_lands_on_final_chunk_with_chat_field_names() {
        let output = collect(vec![
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"delta\":\"hi\"}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_u\",\"model\":\"gpt-5.4\",\"created_at\":5,\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":4,\"output_tokens\":2,\"total_tokens\":6,\"input_tokens_details\":{\"cached_tokens\":3},\"output_tokens_details\":{\"reasoning_tokens\":1}}}}\n\n",
        ])
        .await;
        let chunks = parse_chunks(&output);
        let last = chunks.last().unwrap();

        assert_eq!(last["choices"][0]["finish_reason"], "stop");
        assert_eq!(last["usage"]["prompt_tokens"], 4);
        assert_eq!(last["usage"]["completion_tokens"], 2);
        assert_eq!(last["usage"]["total_tokens"], 6);
        assert_eq!(last["usage"]["prompt_tokens_details"]["cached_tokens"], 3);
        assert_eq!(
            last["usage"]["completion_tokens_details"]["reasoning_tokens"],
            1
        );
        // usage 只在末块出现，中间的增量块不带。
        assert_eq!(
            chunks
                .iter()
                .filter(|chunk| chunk.get("usage").is_some())
                .count(),
            1
        );
    }

    /// A truncated transport must not be disguised as a model token limit.
    #[tokio::test]
    async fn truncated_stream_with_output_reports_transport_error() {
        let output = collect(vec![
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"delta\":\"partial\"}\n\n",
        ])
        .await;
        let chunks = parse_chunks(&output);

        assert_eq!(joined_delta(&chunks, "content"), "partial");
        assert!(finish_reasons(&chunks).is_empty());
        assert!(output.contains("event: error"));
        assert!(output.contains("stream_truncated"));
    }

    /// 一个字都没收到就断开：报错，不能补出一个空的正常完成。
    #[tokio::test]
    async fn truncated_stream_without_output_emits_error() {
        let output = collect(vec![
            "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_empty\",\"model\":\"gpt-5.4\",\"status\":\"in_progress\"}}\n\n",
        ])
        .await;
        let chunks = parse_chunks(&output);

        assert!(output.contains("event: error"));
        assert!(output.contains("stream_truncated"));
        assert!(finish_reasons(&chunks).is_empty());
    }

    /// 传输层错误：如实报错，不补完成事件。
    #[tokio::test]
    async fn transport_error_emits_error_without_finish_reason() {
        let upstream = stream::iter(vec![Err::<Bytes, std::io::Error>(std::io::Error::other(
            "boom",
        ))]);
        let output = collect_stream(upstream).await;
        let chunks = parse_chunks(&output);

        assert!(output.contains("event: error"));
        assert!(output.contains("stream_error"));
        assert!(finish_reasons(&chunks).is_empty());
    }

    /// 非 JSON 噪声行、keepalive 注释不得 panic，也不得干扰正常收尾。
    #[tokio::test]
    async fn ignores_non_json_noise_lines() {
        let output = collect(vec![
            ": keepalive\n\n",
            "data: not-json-at-all\n\n",
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"delta\":\"ok\"}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_n\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"output\":[]}}\n\n",
        ])
        .await;
        let chunks = parse_chunks(&output);

        assert_eq!(joined_delta(&chunks, "content"), "ok");
        assert_eq!(finish_reasons(&chunks), vec!["stop"]);
    }
    /// 只发终态、增量一个不发的网关：内容必须从 `output[]` 里补出来，
    /// 否则客户端拿到"成功但空"的回答。
    #[tokio::test]
    async fn synthesizes_content_from_terminal_output_only_stream() {
        let output = collect(vec![
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_t\",\"model\":\"gpt-5.4\",\"created_at\":7,\"status\":\"completed\",\"output\":[{\"id\":\"rs_1\",\"type\":\"reasoning\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"thought\"}]},{\"id\":\"msg_1\",\"type\":\"message\",\"status\":\"completed\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"final answer\"}]},{\"id\":\"fc_call_9\",\"type\":\"function_call\",\"status\":\"completed\",\"call_id\":\"call_9\",\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"a.txt\\\"}\"}]}}\n\n",
        ])
        .await;
        let chunks = parse_chunks(&output);

        assert_eq!(joined_delta(&chunks, "reasoning_content"), "thought");
        assert_eq!(joined_delta(&chunks, "content"), "final answer");

        let tool_deltas: Vec<&Value> = chunks
            .iter()
            .filter_map(|chunk| chunk.pointer("/choices/0/delta/tool_calls/0"))
            .collect();
        assert_eq!(tool_deltas[0]["id"], "call_9");
        assert_eq!(tool_deltas[0]["function"]["name"], "read_file");
        let arguments = tool_deltas
            .iter()
            .filter_map(|delta| delta.pointer("/function/arguments").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .concat();
        assert_eq!(arguments, r#"{"path":"a.txt"}"#);
        assert_eq!(finish_reasons(&chunks), vec!["tool_calls"]);
    }

    /// 已经流过分片的工具调用，终态 item 的全量 arguments 不得重复累加。
    #[tokio::test]
    async fn terminal_item_does_not_duplicate_streamed_arguments() {
        let output = collect(vec![
            "event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"fc_call_1\",\"type\":\"function_call\",\"status\":\"in_progress\",\"call_id\":\"call_1\",\"name\":\"lookup\",\"arguments\":\"\"}}\n\n",
            "event: response.function_call_arguments.delta\ndata: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_call_1\",\"delta\":\"{\\\"a\\\":1}\"}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_d\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"output\":[{\"id\":\"fc_call_1\",\"type\":\"function_call\",\"status\":\"completed\",\"call_id\":\"call_1\",\"name\":\"lookup\",\"arguments\":\"{\\\"a\\\":1}\"}]}}\n\n",
        ])
        .await;
        let chunks = parse_chunks(&output);

        let arguments = chunks
            .iter()
            .filter_map(|chunk| {
                chunk
                    .pointer("/choices/0/delta/tool_calls/0/function/arguments")
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>()
            .concat();
        assert_eq!(arguments, r#"{"a":1}"#);
    }

    /// `response.incomplete` → `length`（token 截断），不是 `stop`。
    #[tokio::test]
    async fn incomplete_response_finishes_as_length() {
        let output = collect(vec![
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"delta\":\"cut\"}\n\n",
            "event: response.incomplete\ndata: {\"type\":\"response.incomplete\",\"response\":{\"id\":\"resp_i\",\"model\":\"gpt-5.4\",\"status\":\"incomplete\",\"incomplete_details\":{\"reason\":\"max_output_tokens\"},\"output\":[]}}\n\n",
        ])
        .await;
        let chunks = parse_chunks(&output);

        assert_eq!(finish_reasons(&chunks), vec!["length"]);
        assert!(!output.contains("event: error"));
    }

    /// 上游只给出没有函数名的工具调用：丢弃后本回合一个可用调用都不剩，
    /// 必须如实报错，而不是给客户端一个"成功但什么都没做"的回合。
    #[tokio::test]
    async fn dropped_only_tool_call_emits_error_without_finish_reason() {
        let output = collect(vec![
            "event: response.function_call_arguments.delta\ndata: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_unknown\",\"delta\":\"{}\"}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_drop\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"output\":[]}}\n\n",
        ])
        .await;
        let chunks = parse_chunks(&output);

        assert!(output.contains("event: error"));
        assert!(output.contains("upstream_tool_call_dropped"));
        assert!(finish_reasons(&chunks).is_empty());
    }

    /// Custom tools use the same wire shape in streaming and non-streaming replies.
    #[tokio::test]
    async fn custom_tool_input_preserves_chat_custom_shape() {
        let output = collect(vec![
            "event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"ctc_call_c\",\"type\":\"custom_tool_call\",\"status\":\"in_progress\",\"call_id\":\"call_c\",\"name\":\"exec\",\"input\":\"\"}}\n\n",
            "event: response.custom_tool_call_input.delta\ndata: {\"type\":\"response.custom_tool_call_input.delta\",\"item_id\":\"ctc_call_c\",\"delta\":\"ls -la\"}\n\n",
            "event: response.custom_tool_call_input.done\ndata: {\"type\":\"response.custom_tool_call_input.done\",\"item_id\":\"ctc_call_c\",\"input\":\"ls -la\"}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_c\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"output\":[{\"id\":\"ctc_call_c\",\"type\":\"custom_tool_call\",\"status\":\"completed\",\"call_id\":\"call_c\",\"name\":\"exec\",\"input\":\"ls -la\"}]}}\n\n",
        ])
        .await;
        let chunks = parse_chunks(&output);

        let arguments = chunks
            .iter()
            .filter_map(|chunk| {
                chunk
                    .pointer("/choices/0/delta/tool_calls/0/custom/input")
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>()
            .concat();
        assert_eq!(arguments, "ls -la");
        assert!(chunks
            .iter()
            .any(|chunk| chunk.pointer("/choices/0/delta/tool_calls/0/type")
                == Some(&json!("custom"))));
        assert_eq!(finish_reasons(&chunks), vec!["tool_calls"]);
    }
}
