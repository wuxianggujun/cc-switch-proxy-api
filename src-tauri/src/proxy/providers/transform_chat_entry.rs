//! Chat Completions 入口 ⇄ Responses 规范形式转换
//!
//! 场景：客户端用 OpenAI Chat Completions 协议（`/v1/chat/completions`）请求，
//! 但选线命中的上游是 Responses 或 Anthropic 网关。
//!
//! 本模块把 Chat 入口归一成 Responses 表示，之后复用既有的 Codex 全链
//! （`transform_codex_anthropic` / `transform_codex_chat`）；响应侧再从
//! Responses 还原回 Chat。
//!
//! 方向与 `transform_codex_chat.rs` 严格互逆：
//! - `transform_codex_chat.rs`: Responses 请求 → Chat 请求，Chat 响应 → Responses 响应
//! - 本模块:                    Chat 请求 → Responses 请求，Responses 响应 → Chat 响应

use super::codex_chat_common::{extract_reasoning_field_text, split_leading_think_block};
use super::transform_codex_chat::{
    normalize_function_parameters, responses_function_call_to_chat_tool_call, CodexToolContext,
};
use crate::proxy::error::ProxyError;
use crate::proxy::json_canonical::{canonical_json_string, canonicalize_tool_arguments};
use serde_json::{json, Map, Value};

/// Chat 专属的推理开关字段：它们在本转换里已被折叠进 Responses 的 `reasoning`
/// 对象，若同时原样保留会让严格上游看到两套互相冲突的推理声明（openclaw#24119
/// 记录过 `reasoning` 与 `reasoning_effort` 并存触发 400）。因此这些键在
/// 「其余字段整体透传」阶段被剔除——不是丢弃语义，而是已翻译到目标字段。
const CHAT_ONLY_REASONING_FIELDS: &[&str] = &[
    "reasoning_effort",
    "thinking",
    "enable_thinking",
    "reasoning_split",
];

/// 已由本转换显式处理的键，透传阶段跳过，避免在 Responses 体上留下同义重复字段。
const CHAT_TRANSLATED_FIELDS: &[&str] = &[
    "messages",
    "tools",
    "tool_choice",
    "max_tokens",
    "max_completion_tokens",
    "reasoning",
    "n",
    "stream_options",
    "response_format",
    "functions",
    "function_call",
];

/// Reject options that the selected protocol cannot represent instead of silently
/// changing generation behavior or sending known-invalid fields upstream.
pub fn chat_request_for_upstream(mut body: Value, anthropic: bool) -> Result<Value, ProxyError> {
    if let Some(object) = body.as_object_mut() {
        for key in [
            "seed",
            "frequency_penalty",
            "presence_penalty",
            "logit_bias",
            "logprobs",
            "top_logprobs",
            "audio",
            "modalities",
        ] {
            if let Some(value) = object.remove(key) {
                let neutral = value.is_null()
                    || value == json!(false)
                    || (matches!(
                        key,
                        "frequency_penalty" | "presence_penalty" | "top_logprobs"
                    ) && value.as_f64() == Some(0.0))
                    || (key == "logit_bias" && value.as_object().is_some_and(|map| map.is_empty()))
                    || (key == "modalities" && value == json!(["text"]));
                if !neutral {
                    return Err(ProxyError::InvalidRequest(format!(
                        "{key} has no lossless representation in the selected {} protocol; use a Chat Completions upstream",
                        if anthropic { "Anthropic" } else { "Responses" }
                    )));
                }
            }
        }
        if !anthropic {
            if let Some(stop) = object.remove("stop") {
                if !stop.is_null() && !stop.as_array().is_some_and(|items| items.is_empty()) {
                    return Err(ProxyError::InvalidRequest("stop is not supported by the Responses protocol; use a Chat or Anthropic upstream".into()));
                }
            }
        }
    }
    chat_request_to_responses(body)
}

pub fn chat_tool_context(body: &Value) -> CodexToolContext {
    let tools = body
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .map(chat_tool_to_responses_tool)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    super::transform_codex_chat::build_codex_tool_context_from_request(&json!({"tools":tools}))
}

/// Chat Completions 请求 → Responses 请求。
///
/// `responses_to_chat_completions` 的逆向：`messages[]` 折叠为 `input[]`，
/// `max_tokens` → `max_output_tokens`，`tools[].function` 展平为 Responses 工具形状。
pub fn chat_request_to_responses(body: Value) -> Result<Value, ProxyError> {
    let Some(chat) = body.as_object() else {
        return Err(ProxyError::InvalidRequest(
            "chat request body must be a JSON object".to_string(),
        ));
    };

    let messages = chat
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ProxyError::InvalidRequest("chat request is missing messages[]".to_string())
        })?;

    if chat
        .get("n")
        .is_some_and(|n| !n.is_null() && n.as_u64() != Some(1))
    {
        return Err(ProxyError::InvalidRequest(
            "Cross-protocol Chat routing supports n=1 only; select a Chat upstream for multiple choices".into(),
        ));
    }

    let mut result = Map::new();

    // 未被本转换显式翻译的键整体透传（含 model / temperature / top_p / stream /
    // metadata / user / 以及客户端自带的 Responses 扩展）。宁可让严格 Responses
    // 上游对某个 Chat 专属字段报 400，也不静默吞掉——静默丢字段会让上游行为
    // 无声改变，比一个可见的错误难查得多。
    for (key, value) in chat {
        if CHAT_TRANSLATED_FIELDS.contains(&key.as_str())
            || CHAT_ONLY_REASONING_FIELDS.contains(&key.as_str())
        {
            continue;
        }
        result.insert(key.clone(), value.clone());
    }

    let ChatMessagesAsInput {
        instructions,
        input,
    } = chat_messages_to_responses_input(messages);
    if let Some(instructions) = instructions {
        result.insert("instructions".to_string(), json!(instructions));
    }
    result.insert("input".to_string(), Value::Array(input));

    // Chat 的两个 token 上限字段都归一到 max_output_tokens；显式的
    // max_completion_tokens（o-series 形态）优先，与正向转换的写入顺序互逆。
    if let Some(max_tokens) = chat
        .get("max_completion_tokens")
        .or_else(|| chat.get("max_tokens"))
    {
        result.insert("max_output_tokens".to_string(), max_tokens.clone());
    }

    if let Some(reasoning) = chat_reasoning_to_responses_reasoning(chat) {
        result.insert("reasoning".to_string(), reasoning);
    }

    if let Some(tools) = chat.get("tools").and_then(Value::as_array) {
        let tools: Vec<Value> = tools.iter().map(chat_tool_to_responses_tool).collect();
        if !tools.is_empty() {
            result.insert("tools".to_string(), Value::Array(tools));
        }
    } else if let Some(functions) = chat.get("functions").and_then(Value::as_array) {
        result.insert(
            "tools".into(),
            Value::Array(
                functions
                    .iter()
                    .map(|function| {
                        chat_tool_to_responses_tool(&json!({"type":"function","function":function}))
                    })
                    .collect(),
            ),
        );
    }

    if let Some(tool_choice) = chat.get("tool_choice") {
        result.insert(
            "tool_choice".to_string(),
            chat_tool_choice_to_responses(tool_choice),
        );
    } else if let Some(choice) = chat.get("function_call") {
        result.insert(
            "tool_choice".into(),
            if choice.is_string() {
                choice.clone()
            } else {
                json!({"type":"function","name":choice["name"]})
            },
        );
    }

    if let Some(format) = chat.get("response_format").filter(|value| !value.is_null()) {
        let format = match format.get("type").and_then(Value::as_str) {
            Some("json_schema") => {
                let mut schema = format
                    .get("json_schema")
                    .and_then(Value::as_object)
                    .cloned()
                    .ok_or_else(|| {
                        ProxyError::InvalidRequest(
                            "response_format.json_schema must be an object".into(),
                        )
                    })?;
                schema.insert("type".into(), json!("json_schema"));
                Value::Object(schema)
            }
            Some("json_object" | "text") => format.clone(),
            _ => {
                return Err(ProxyError::InvalidRequest(
                    "Unsupported response_format type".into(),
                ))
            }
        };
        let text = result
            .entry("text".to_string())
            .or_insert_with(|| json!({}));
        let text = text
            .as_object_mut()
            .ok_or_else(|| ProxyError::InvalidRequest("text must be an object".into()))?;
        text.insert("format".into(), format);
    }

    Ok(Value::Object(result))
}

struct ChatMessagesAsInput {
    instructions: Option<String>,
    input: Vec<Value>,
}

/// `messages[]` → `instructions` + `input[]`。
///
/// 正向转换把 instructions 与全部历史 system/developer 合并成首条 system 消息
/// （`collapse_system_messages_to_head`），故逆向只把「开头连续的纯文本
/// system/developer」还原为 instructions。带多模态内容的 system 消息不折叠，
/// 否则其中的图片/文件会被 `instruction_text` 拍平成空串而丢失。
fn chat_messages_to_responses_input(messages: &[Value]) -> ChatMessagesAsInput {
    let mut instruction_chunks: Vec<String> = Vec::new();
    let mut input: Vec<Value> = Vec::new();
    let mut at_head = true;

    let mut legacy_calls = std::collections::HashMap::<String, String>::new();
    for (index, message) in messages.iter().enumerate() {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");

        if at_head && matches!(role, "system" | "developer") {
            if let Some(text) = message.get("content").and_then(Value::as_str) {
                if !text.trim().is_empty() {
                    instruction_chunks.push(text.to_string());
                }
                continue;
            }
        }
        at_head = false;

        // Legacy functions carry only a name in their result message. Allocate
        // a unique call ID and use it on both sides instead of unrelated fallbacks.
        let mut normalized = message.clone();
        if role == "assistant" && message.get("tool_calls").is_none() {
            if let Some(function) = normalized
                .get_mut("function_call")
                .and_then(Value::as_object_mut)
            {
                let name = function
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let id = format!("call_legacy_{index}");
                function.insert("id".into(), json!(id));
                legacy_calls.insert(name, id);
            }
        } else if role == "function" {
            if let Some(id) = message
                .get("name")
                .and_then(Value::as_str)
                .and_then(|name| legacy_calls.remove(name))
            {
                normalized["tool_call_id"] = json!(id);
            }
        }
        append_chat_message_as_responses_items(&normalized, role, &mut input);
    }

    let custom_ids: std::collections::HashSet<String> = input
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("custom_tool_call"))
        .filter_map(|item| {
            item.get("call_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    for item in &mut input {
        if item.get("type").and_then(Value::as_str) == Some("function_call_output")
            && item
                .get("call_id")
                .and_then(Value::as_str)
                .is_some_and(|id| custom_ids.contains(id))
        {
            item["type"] = json!("custom_tool_call_output");
        }
    }

    ChatMessagesAsInput {
        instructions: (!instruction_chunks.is_empty()).then(|| instruction_chunks.join("\n\n")),
        input,
    }
}

fn append_chat_message_as_responses_items(message: &Value, role: &str, input: &mut Vec<Value>) {
    // 工具结果先落地：Responses 用独立的 function_call_output item 承载，
    // 不挂在任何 message 上。
    if matches!(role, "tool" | "function") {
        append_chat_tool_message(message, input);
        return;
    }

    if role == "assistant" {
        append_chat_assistant_message(message, input);
        return;
    }

    // user / system / developer / 其他自定义 role 一律保留原 role：中途出现的
    // system/developer 不能降级成 user，否则指令优先级被静默改变；下游
    // （codex_anthropic / codex_chat）本身就按 role 识别这些 item。
    let content = chat_content_to_responses_content(message.get("content"), role);
    input.push(json!({
        "role": role,
        "content": content
    }));
}

fn append_chat_assistant_message(message: &Value, input: &mut Vec<Value>) {
    // reasoning 在 Responses 里是 assistant 之前的独立 item，故先发。
    if let Some(reasoning_item) = chat_assistant_reasoning_item(message) {
        input.push(reasoning_item);
    }

    // `<think>…</think>` 内联思考已在上面进入 reasoning item，正文只留答案部分，
    // 避免同一段思考在 reasoning 与 content 里出现两次。
    let content = match message.get("content") {
        Some(Value::String(text)) => {
            let answer = split_leading_think_block(text)
                .map(|(_reasoning, answer)| answer)
                .unwrap_or_else(|| text.clone());
            (!answer.is_empty()).then(|| {
                Value::Array(vec![json!({
                    "type": "output_text",
                    "text": answer
                })])
            })
        }
        other => {
            let content = chat_content_to_responses_content(other, "assistant");
            content
                .as_array()
                .is_some_and(|parts| !parts.is_empty())
                .then_some(content)
        }
    };

    if let Some(content) = content {
        input.push(json!({
            "role": "assistant",
            "content": content
        }));
    }

    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for tool_call in tool_calls {
            input.push(chat_tool_call_to_responses_item(tool_call));
        }
    } else if let Some(function_call) = message.get("function_call") {
        // 废弃的单函数调用形态：合成一个 function_call item，call_id 缺失时给
        // 稳定占位，保证后续 function_call_output 能配对。
        let call_id = function_call
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or("call_0");
        input.push(json!({
            "type": "function_call",
            "call_id": call_id,
            "name": function_call.get("name").and_then(Value::as_str).unwrap_or(""),
            "arguments": canonicalize_tool_arguments(function_call.get("arguments"))
        }));
    }
}

fn append_chat_tool_message(message: &Value, input: &mut Vec<Value>) {
    // 废弃的 `role: "function"` 形态没有 tool_call_id，用 name 兜底配对。
    let call_id = message
        .get("tool_call_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .or_else(|| message.get("name").and_then(Value::as_str))
        .unwrap_or("");

    // output 原样保留：字符串与结构化内容下游都能处理
    // （`tool_result_content_from_responses_item` / `plan_chat_tool_output_media`），
    // 这里再做一次 JSON 化反而会双重转义。
    let output = message.get("content").cloned().unwrap_or_else(|| json!(""));

    input.push(json!({
        "type": "function_call_output",
        "call_id": call_id,
        "output": output
    }));
}

/// assistant 消息上的思考 → Responses `reasoning` item。
///
/// 文本取自 `reasoning_content` / `reasoning` / `reasoning_details` / 内联
/// `<think>` 块；`reasoning.id` 与 `reasoning.encrypted_content` 是响应侧回写的
/// 不透明载荷，必须原样带回，否则 Codex/Anthropic 的多轮思考回放会断链。
fn chat_assistant_reasoning_item(message: &Value) -> Option<Value> {
    let text = extract_reasoning_field_text(message).or_else(|| {
        message
            .get("content")
            .and_then(Value::as_str)
            .and_then(split_leading_think_block)
            .map(|(reasoning, _answer)| reasoning)
            .filter(|reasoning| !reasoning.is_empty())
    });

    let id = message
        .pointer("/reasoning/id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let encrypted_content = message
        .pointer("/reasoning/encrypted_content")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());

    if text.is_none() && encrypted_content.is_none() {
        return None;
    }

    let mut item = Map::new();
    if let Some(id) = id {
        item.insert("id".to_string(), json!(id));
    }
    item.insert("type".to_string(), json!("reasoning"));
    item.insert(
        "summary".to_string(),
        match text.as_deref().filter(|text| !text.is_empty()) {
            Some(text) => json!([{ "type": "summary_text", "text": text }]),
            None => json!([]),
        },
    );
    if let Some(encrypted_content) = encrypted_content {
        item.insert("encrypted_content".to_string(), json!(encrypted_content));
    }

    Some(Value::Object(item))
}

/// Chat `content` → Responses `content[]`。
///
/// `input_text` / `output_text` 按 role 区分（正向转换据此判断历史消息归属）。
/// 无法识别的 part 原样透传：形状不认识不等于内容无意义，静默丢弃会让上游看到
/// 被削减的上下文。
fn chat_content_to_responses_content(content: Option<&Value>, role: &str) -> Value {
    let text_type = if role == "assistant" {
        "output_text"
    } else {
        "input_text"
    };

    match content {
        Some(Value::String(text)) => json!([{ "type": text_type, "text": text }]),
        Some(Value::Array(parts)) => Value::Array(
            parts
                .iter()
                .map(|part| chat_content_part_to_responses_part(part, text_type))
                .collect(),
        ),
        Some(Value::Null) | None => Value::Array(Vec::new()),
        Some(other) => json!([{ "type": text_type, "text": canonical_json_string(other) }]),
    }
}

fn chat_content_part_to_responses_part(part: &Value, text_type: &str) -> Value {
    match part.get("type").and_then(Value::as_str) {
        Some("text") => json!({
            "type": text_type,
            "text": part.get("text").and_then(Value::as_str).unwrap_or("")
        }),
        Some("image_url") => {
            let image = part.get("image_url").unwrap_or(&Value::Null);
            let mut result = json!({"type":"input_image", "image_url":
                image.get("url").unwrap_or(image)});
            if let Some(detail) = image.get("detail") {
                result["detail"] = detail.clone();
            }
            result
        }
        Some("file") => {
            let mut file_part = Map::new();
            file_part.insert("type".to_string(), json!("input_file"));
            if let Some(file) = part.get("file").and_then(Value::as_object) {
                for (key, value) in file {
                    file_part.insert(key.clone(), value.clone());
                }
            }
            Value::Object(file_part)
        }
        Some("input_audio") => json!({
            "type": "input_audio",
            "input_audio": part.get("input_audio").cloned().unwrap_or(Value::Null)
        }),
        _ => part.clone(),
    }
}

fn chat_tool_call_to_responses_item(tool_call: &Value) -> Value {
    let call_id = tool_call
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default();

    // OpenAI 新增的 Chat 自定义工具形态：`{"type":"custom","custom":{name,input}}`
    // 直接还原成 Responses custom_tool_call，input 保持原始字符串不做 JSON 包装。
    if tool_call.get("type").and_then(Value::as_str) == Some("custom") {
        let custom = tool_call.get("custom").unwrap_or(&Value::Null);
        return json!({
            "type": "custom_tool_call",
            "call_id": call_id,
            "name": custom.get("name").and_then(Value::as_str).unwrap_or(""),
            "input": custom.get("input").cloned().unwrap_or_else(|| json!(""))
        });
    }

    let function = tool_call.get("function").unwrap_or(&Value::Null);
    json!({
        "type": "function_call",
        "call_id": call_id,
        "name": function.get("name").and_then(Value::as_str).unwrap_or(""),
        "arguments": canonicalize_tool_arguments(function.get("arguments"))
    })
}

/// Chat 嵌套工具形状 → Responses 扁平工具形状（`responses_function_tool_to_chat_tool` 的逆）。
fn chat_tool_to_responses_tool(tool: &Value) -> Value {
    match tool.get("type").and_then(Value::as_str) {
        Some("function") => {
            let function = tool.get("function").unwrap_or(&Value::Null);
            let mut result = Map::new();
            result.insert("type".to_string(), json!("function"));
            // function 对象内的其余键（description / strict / 厂商扩展）先整体带过来，
            // 再覆写 name / parameters，避免逆向时丢掉正向没动过的字段。
            if let Some(obj) = function.as_object() {
                for (key, value) in obj {
                    result.insert(key.clone(), value.clone());
                }
            }
            result.insert(
                "name".to_string(),
                json!(function.get("name").and_then(Value::as_str).unwrap_or("")),
            );
            result.insert(
                "parameters".to_string(),
                normalize_function_parameters(function.get("parameters")),
            );
            Value::Object(result)
        }
        Some("custom") => {
            let custom = tool.get("custom").unwrap_or(&Value::Null);
            let mut result = Map::new();
            result.insert("type".to_string(), json!("custom"));
            if let Some(obj) = custom.as_object() {
                for (key, value) in obj {
                    result.insert(key.clone(), value.clone());
                }
            }
            Value::Object(result)
        }
        // 托管工具（web_search 等）与未知类型原样透传，交由下游按能力裁剪。
        _ => tool.clone(),
    }
}

/// Chat `tool_choice` → Responses `tool_choice`（`responses_tool_choice_to_chat` 的逆）。
fn chat_tool_choice_to_responses(tool_choice: &Value) -> Value {
    let Some(obj) = tool_choice.as_object() else {
        // "auto" / "none" / "required" 两侧同名同义，字符串直接透传。
        return tool_choice.clone();
    };

    match obj.get("type").and_then(Value::as_str) {
        Some("function") => json!({
            "type": "function",
            "name": tool_choice
                .pointer("/function/name")
                .and_then(Value::as_str)
                .or_else(|| obj.get("name").and_then(Value::as_str))
                .unwrap_or("")
        }),
        Some("custom") => json!({
            "type": "custom",
            "name": tool_choice
                .pointer("/custom/name")
                .and_then(Value::as_str)
                .or_else(|| obj.get("name").and_then(Value::as_str))
                .unwrap_or("")
        }),
        _ => tool_choice.clone(),
    }
}

/// Chat 的推理开关 → Responses `reasoning` 对象。
///
/// 覆盖 `reasoning_effort`（OpenAI/DeepSeek 顶层字段）、`reasoning`（OpenRouter
/// 归一化对象）与 `thinking` / `enable_thinking`（Zhipu、Qwen 等 toggle 形态）。
/// 只有 toggle 而没有档位时不臆造 effort：发一个空 `reasoning` 对象表达"要思考、
/// 档位未指定"，`reasoning_requested` 据此判定为开启。
fn chat_reasoning_to_responses_reasoning(chat: &Map<String, Value>) -> Option<Value> {
    let mut reasoning = chat
        .get("reasoning")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    if let Some(effort) = chat.get("reasoning_effort") {
        reasoning
            .entry("effort".to_string())
            .or_insert(effort.clone());
    }

    let toggle = chat
        .get("enable_thinking")
        .and_then(Value::as_bool)
        .or_else(|| chat.get("reasoning_split").and_then(Value::as_bool))
        .or_else(|| {
            chat.get("thinking")
                .and_then(|thinking| thinking.get("type"))
                .and_then(Value::as_str)
                .map(|kind| !matches!(kind, "disabled" | "none" | "off"))
        });

    match toggle {
        // 显式关闭必须忠实表达：effort=none 是 reasoning_requested 识别的关闭形态，
        // 缺这一步下游会因"不带 reasoning 字段"而无法关掉默认开思考的模型。
        Some(false) => {
            reasoning.insert("effort".to_string(), json!("none"));
        }
        Some(true) => {}
        None if reasoning.is_empty() => return None,
        None => {}
    }

    Some(Value::Object(reasoning))
}

/// Responses 响应 → Chat Completions 响应（非流式）。
///
/// `chat_completion_to_response` 的逆向：`output[]` 收敛为单个 `choices[0].message`，
/// `usage` 字段名还原为 `prompt_tokens` / `completion_tokens` / `total_tokens`。
pub fn responses_response_to_chat(body: Value) -> Result<Value, ProxyError> {
    // Responses 的 failed/cancelled 与 error 信封在 Chat 里没有对应状态，
    // 只能作为错误体返回，否则会把一次失败伪装成 finish_reason=stop 的成功回合。
    if is_responses_error_envelope(&body) {
        return Ok(responses_error_to_chat_error(Some(&body)));
    }

    let output = body
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| ProxyError::TransformError("No output in responses response".to_string()))?;

    let collected = collect_responses_output(output);
    let status = body.get("status").and_then(Value::as_str);
    let incomplete_reason = body
        .pointer("/incomplete_details/reason")
        .and_then(Value::as_str);

    let mut message = Map::new();
    message.insert("role".to_string(), json!("assistant"));
    message.insert(
        "content".to_string(),
        if collected.text.is_empty() {
            // 只有工具调用（或只有拒答）时 content 必须是 null，与 OpenAI 一致；
            // 空字符串会被部分客户端当成"模型回了空话"。
            if collected.tool_calls.is_empty() && collected.refusal.is_none() {
                json!("")
            } else {
                Value::Null
            }
        } else {
            json!(collected.text.join("\n"))
        },
    );
    if let Some(refusal) = &collected.refusal {
        message.insert("refusal".to_string(), json!(refusal));
    }
    if let Some(reasoning) = &collected.reasoning_text {
        message.insert("reasoning_content".to_string(), json!(reasoning));
    }
    // reasoning item 的 id 与 encrypted_content 是上游的不透明载荷，回写到 Chat
    // 消息上；客户端下一轮把它带回来时 chat_request_to_responses 能原样复原，
    // 保证跨轮思考链不断。
    if collected.reasoning_id.is_some() || collected.reasoning_encrypted.is_some() {
        let mut reasoning = Map::new();
        if let Some(id) = &collected.reasoning_id {
            reasoning.insert("id".to_string(), json!(id));
        }
        if let Some(encrypted) = &collected.reasoning_encrypted {
            reasoning.insert("encrypted_content".to_string(), json!(encrypted));
        }
        message.insert("reasoning".to_string(), Value::Object(reasoning));
    }
    let has_tool_calls = !collected.tool_calls.is_empty();
    if has_tool_calls {
        message.insert("tool_calls".to_string(), Value::Array(collected.tool_calls));
    }

    let mut choice = Map::new();
    choice.insert("index".to_string(), json!(0));
    choice.insert("message".to_string(), Value::Object(message));
    choice.insert(
        "finish_reason".to_string(),
        json!(finish_reason_from_response_status(
            status,
            has_tool_calls,
            incomplete_reason
        )),
    );
    choice.insert("logprobs".to_string(), Value::Null);

    let mut result = Map::new();
    result.insert(
        "id".to_string(),
        json!(chat_id_from_response_id(
            body.get("id").and_then(Value::as_str)
        )),
    );
    result.insert("object".to_string(), json!("chat.completion"));
    result.insert(
        "created".to_string(),
        body.get("created_at").cloned().unwrap_or_else(|| json!(0)),
    );
    result.insert(
        "model".to_string(),
        body.get("model").cloned().unwrap_or_else(|| json!("")),
    );
    result.insert("choices".to_string(), json!([Value::Object(choice)]));
    result.insert(
        "usage".to_string(),
        responses_usage_to_chat_usage(body.get("usage")),
    );

    // Chat 协议无法表达的 output item（web_search_call、image_generation_call 等）
    // 不静默丢弃：留在非标准字段里并告警，便于排查"上游干了活但客户端没看到"。
    if !collected.unmapped.is_empty() {
        log::warn!(
            "[ChatEntry] {} responses output item(s) have no Chat representation: {:?}",
            collected.unmapped.len(),
            collected
                .unmapped
                .iter()
                .filter_map(|item| item.get("type").and_then(Value::as_str))
                .collect::<Vec<_>>()
        );
        result.insert(
            "ccswitch_unmapped_output".to_string(),
            Value::Array(collected.unmapped),
        );
    }

    Ok(Value::Object(result))
}

#[derive(Default)]
struct CollectedOutput {
    text: Vec<String>,
    refusal: Option<String>,
    reasoning_text: Option<String>,
    reasoning_id: Option<String>,
    reasoning_encrypted: Option<String>,
    tool_calls: Vec<Value>,
    unmapped: Vec<Value>,
}
fn collect_responses_output(output: &[Value]) -> CollectedOutput {
    let mut collected = CollectedOutput::default();

    for item in output {
        match item.get("type").and_then(Value::as_str) {
            Some("reasoning") => {
                if let Some(text) = super::codex_chat_common::extract_reasoning_summary_text(item) {
                    append_reasoning_text(&mut collected.reasoning_text, &text);
                }
                if let Some(id) = item
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                {
                    collected.reasoning_id.get_or_insert(id.to_string());
                }
                if let Some(encrypted) = item
                    .get("encrypted_content")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                {
                    collected
                        .reasoning_encrypted
                        .get_or_insert(encrypted.to_string());
                }
            }
            Some("function_call") => {
                collect_item_reasoning(item, &mut collected);
                collected
                    .tool_calls
                    .push(responses_function_call_to_chat_tool_call(
                        item,
                        &CodexToolContext::default(),
                    ));
            }
            Some("custom_tool_call") => {
                collect_item_reasoning(item, &mut collected);
                collected
                    .tool_calls
                    .push(responses_custom_tool_call_to_chat_custom_call(item));
            }
            Some("tool_search_call") => {
                collect_item_reasoning(item, &mut collected);
                // Chat 客户端无法声明 tool_search，只可能出现在混合链路里；
                // 退化成同名 function 调用而不是丢弃。
                collected.tool_calls.push(json!({
                    "id": item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .cloned()
                        .unwrap_or_else(|| json!("")),
                    "type": "function",
                    "function": {
                        "name": "tool_search",
                        "arguments": canonicalize_tool_arguments(item.get("arguments"))
                    }
                }));
            }
            // message item，以及不带 type 但带 content 的裸消息。
            _ if item.get("content").is_some() => {
                collect_message_content(item, &mut collected);
            }
            _ => collected.unmapped.push(item.clone()),
        }
    }

    collected
}

fn collect_message_content(item: &Value, collected: &mut CollectedOutput) {
    match item.get("content") {
        Some(Value::String(text)) if !text.is_empty() => collected.text.push(text.clone()),
        Some(Value::Array(parts)) => {
            for part in parts {
                match part.get("type").and_then(Value::as_str) {
                    Some("output_text" | "input_text" | "text") => {
                        if let Some(text) = part
                            .get("text")
                            .and_then(Value::as_str)
                            .filter(|text| !text.is_empty())
                        {
                            collected.text.push(text.to_string());
                        }
                    }
                    Some("refusal") => {
                        if let Some(refusal) = part
                            .get("refusal")
                            .and_then(Value::as_str)
                            .filter(|text| !text.is_empty())
                        {
                            collected.refusal = Some(refusal.to_string());
                        }
                    }
                    _ => collected.unmapped.push(part.clone()),
                }
            }
        }
        _ => {}
    }
}

/// 工具调用 item 上可能挂着 `reasoning_content`（Codex 链路为 kimi/DeepSeek 等
/// 附加），一并收进 assistant 消息，避免思考随工具调用丢失。
fn collect_item_reasoning(item: &Value, collected: &mut CollectedOutput) {
    if let Some(text) = extract_reasoning_field_text(item) {
        append_reasoning_text(&mut collected.reasoning_text, &text);
    }
}

fn append_reasoning_text(target: &mut Option<String>, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    match target {
        Some(existing) if existing.contains(text) => {}
        Some(existing) => {
            existing.push_str("\n\n");
            existing.push_str(text);
        }
        None => *target = Some(text.to_string()),
    }
}

/// Responses `custom_tool_call` → Chat 自定义工具调用。
///
/// 与 `transform_codex_chat` 的同名转换方向不同：那里的目标是「把 Codex 自定义
/// 工具伪装成 function 发给只认 function 的上游」，这里的对端是真正声明了 custom
/// 工具的 Chat 客户端，必须还原 `{"type":"custom","custom":{name,input}}`。
fn responses_custom_tool_call_to_chat_custom_call(item: &Value) -> Value {
    json!({
        "id": item
            .get("call_id")
            .or_else(|| item.get("id"))
            .cloned()
            .unwrap_or_else(|| json!("")),
        "type": "custom",
        "custom": {
            "name": item.get("name").and_then(Value::as_str).unwrap_or(""),
            "input": responses_custom_tool_call_input(item)
        }
    })
}
/// custom_tool_call 的 input 取值。
///
/// The upstream-to-Responses converter has already removed any function wrapper.
/// Native custom input is opaque text, even when that text itself is JSON.
fn responses_custom_tool_call_input(item: &Value) -> Value {
    match item.get("input") {
        Some(Value::String(text)) => json!(text),
        Some(other) => json!(canonical_json_string(other)),
        None => json!(""),
    }
}

/// `response_status_from_finish_reason` 的逆向。
///
/// 正向是有损的（除 `length` 外全部映射到 `completed`），逆向据
/// `incomplete_details.reason` 与是否有工具调用重建；截断优先于工具调用，
/// 与 `map_responses_stop_reason` 的判定顺序一致。
fn finish_reason_from_response_status(
    status: Option<&str>,
    has_tool_calls: bool,
    incomplete_reason: Option<&str>,
) -> &'static str {
    match status {
        Some("incomplete") => match incomplete_reason {
            Some("content_filter") => "content_filter",
            // reason 缺失时按截断处理：Responses 只在没跑完时才置 incomplete。
            _ => "length",
        },
        _ if has_tool_calls => "tool_calls",
        _ => "stop",
    }
}

/// `response_id_from_chat_id` 的逆向：剥掉 `resp_` 前缀还原 Chat id。
fn chat_id_from_response_id(id: Option<&str>) -> String {
    match id.filter(|value| !value.is_empty()) {
        Some(id) => id.strip_prefix("resp_").unwrap_or(id).to_string(),
        None => "chatcmpl-ccswitch".to_string(),
    }
}

fn is_responses_error_envelope(body: &Value) -> bool {
    if matches!(
        body.get("status").and_then(Value::as_str),
        Some("failed" | "cancelled")
    ) {
        return true;
    }
    body.get("error").is_some_and(|error| !error.is_null())
}
/// `chat_usage_to_responses_usage` 的逆向：字段名还原成 Chat 侧命名，
/// cache token 回到 `prompt_tokens_details` / `completion_tokens_details` 之下。
fn responses_usage_to_chat_usage(usage: Option<&Value>) -> Value {
    let Some(usage) = usage.filter(|value| value.is_object()) else {
        return json!({
            "prompt_tokens": 0,
            "completion_tokens": 0,
            "total_tokens": 0,
            "prompt_tokens_details": { "cached_tokens": 0 },
            "completion_tokens_details": { "reasoning_tokens": 0 }
        });
    };

    let prompt_tokens = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let completion_tokens = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total_tokens = usage
        .get("total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(prompt_tokens + completion_tokens);

    let mut result = Map::new();
    result.insert("prompt_tokens".to_string(), json!(prompt_tokens));
    result.insert("completion_tokens".to_string(), json!(completion_tokens));
    result.insert("total_tokens".to_string(), json!(total_tokens));

    let cached = usage
        .pointer("/input_tokens_details/cached_tokens")
        .or_else(|| usage.get("cache_read_input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_write = usage
        .pointer("/input_tokens_details/cache_write_tokens")
        .or_else(|| usage.get("cache_creation_input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    let mut prompt_details = Map::new();
    prompt_details.insert("cached_tokens".to_string(), json!(cached));
    if cache_write > 0 {
        prompt_details.insert("cache_write_tokens".to_string(), json!(cache_write));
    }
    result.insert(
        "prompt_tokens_details".to_string(),
        Value::Object(prompt_details),
    );
    // Anthropic 风格的直读 cache 字段在正向里被镜像到 usage 顶层，逆向一并保留，
    // 计费/缓存命中统计不因为多经过一次 Chat 还原而降精度。
    if let Some(cache_read) = usage.get("cache_read_input_tokens").cloned() {
        result.insert("cache_read_input_tokens".to_string(), cache_read);
    }
    if let Some(cache_creation) = usage.get("cache_creation_input_tokens").cloned() {
        result.insert("cache_creation_input_tokens".to_string(), cache_creation);
    }

    match usage
        .get("output_tokens_details")
        .filter(|value| value.is_object())
    {
        Some(details) => {
            let mut details = details.clone();
            if details.get("reasoning_tokens").is_none() {
                details["reasoning_tokens"] = json!(0);
            }
            result.insert("completion_tokens_details".to_string(), details);
        }
        None => {
            result.insert(
                "completion_tokens_details".to_string(),
                json!({ "reasoning_tokens": 0 }),
            );
        }
    }

    Value::Object(result)
}

/// `chat_error_to_response_error` 的逆向：把 Responses 风格的错误体规整成
/// Chat Completions 客户端识别的 `{"error": {message, type, code, param}}`。
///
/// 兼容裸字符串、顶层 `message` / `detail`，以及 `status=failed` 时错误信息只
/// 出现在 `incomplete_details` 里的形态。
// Used by the Chat response bridge when the upstream returns an error envelope.
#[allow(dead_code)]
pub fn responses_error_to_chat_error(body: Option<&Value>) -> Value {
    let Some(value) = body else {
        return json!({
            "error": {
                "message": "Upstream returned an empty error response",
                "type": "upstream_error",
                "code": Value::Null,
                "param": Value::Null,
            }
        });
    };

    if let Some(text) = value.as_str() {
        return json!({
            "error": {
                "message": text,
                "type": "upstream_error",
                "code": Value::Null,
                "param": Value::Null,
            }
        });
    }
    let source = value
        .get("error")
        .filter(|error| !error.is_null())
        .unwrap_or(value);

    let message = source
        .get("message")
        .or_else(|| source.get("detail"))
        .or_else(|| value.pointer("/incomplete_details/reason"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| source.as_str().map(ToString::to_string))
        .unwrap_or_else(|| {
            // 提取不出文本就把整个 JSON 回吐，便于用户排查。
            serde_json::to_string(source).unwrap_or_else(|_| "Upstream error".to_string())
        });

    json!({
        "error": {
            "message": message,
            "type": source
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("upstream_error"),
            "code": source.get("code").cloned().unwrap_or(Value::Null),
            "param": source.get("param").cloned().unwrap_or(Value::Null),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_json_text_and_result_types_survive_the_complete_round_trip() {
        let input_text = r#"{"input":"这是工具原文，不是代理包装"}"#;
        let response = responses_response_to_chat(json!({"id":"resp_custom","status":"completed","output":[{
            "type":"custom_tool_call","call_id":"call_custom","name":"raw_tool","input":input_text
        }]})).unwrap();
        let message = response["choices"][0]["message"].clone();
        assert_eq!(message["tool_calls"][0]["custom"]["input"], input_text);
        let next = chat_request_to_responses(json!({"messages":[message,
            {"role":"tool","tool_call_id":"call_custom","content":"工具结果"}]}))
        .unwrap();
        assert_eq!(next["input"][0]["input"], input_text);
        assert_eq!(next["input"][1]["type"], "custom_tool_call_output");
        assert_eq!(next["input"][1]["call_id"], "call_custom");
    }
    use crate::proxy::providers::transform_codex_chat::{
        chat_completion_to_response, responses_to_chat_completions,
    };

    /// Chat → Responses → Chat：走真实的正向转换回来，验证语义不丢。
    fn round_trip_request(chat: Value) -> Value {
        let responses = chat_request_to_responses(chat).unwrap();
        responses_to_chat_completions(responses).unwrap()
    }

    fn first_message(chat: &Value) -> &Value {
        &chat["choices"][0]["message"]
    }

    #[test]
    fn plain_text_request_round_trips_without_losing_semantics() {
        let chat = json!({
            "model": "gpt-5.1",
            "messages": [
                {"role": "system", "content": "Be terse."},
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello"},
                {"role": "user", "content": "again"}
            ],
            "max_tokens": 256,
            "temperature": 0.4,
            "stream": false
        });

        let responses = chat_request_to_responses(chat.clone()).unwrap();
        // 首条 system 折叠进 instructions；其余按序进 input。
        assert_eq!(responses["instructions"], "Be terse.");
        assert_eq!(responses["max_output_tokens"], 256);
        assert_eq!(responses["temperature"], 0.4);
        assert!(responses.get("max_tokens").is_none());
        let input = responses["input"].as_array().unwrap();
        assert_eq!(input.len(), 3);
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[1]["role"], "assistant");
        assert_eq!(input[1]["content"][0]["type"], "output_text");

        let back = round_trip_request(chat);
        let messages = back["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "Be terse.");
        assert_eq!(messages[1]["content"], "hi");
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[2]["content"], "hello");
        assert_eq!(messages[3]["content"], "again");
        assert_eq!(back["max_tokens"], 256);
    }

    #[test]
    fn tools_convert_to_flat_responses_shape_and_back() {
        let chat = json!({
            "model": "gpt-5.1",
            "messages": [{"role": "user", "content": "weather?"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Look up weather.",
                    "strict": true,
                    "parameters": {
                        "type": "object",
                        "properties": {"city": {"type": "string"}},
                        "required": ["city"]
                    }
                }
            }],
            "tool_choice": {"type": "function", "function": {"name": "get_weather"}}
        });

        let responses = chat_request_to_responses(chat.clone()).unwrap();
        let tool = &responses["tools"][0];
        // Responses 工具是扁平形状：name/description/parameters 直接挂在 tool 上。
        assert_eq!(tool["type"], "function");
        assert_eq!(tool["name"], "get_weather");
        assert_eq!(tool["description"], "Look up weather.");
        assert_eq!(tool["strict"], true);
        assert_eq!(tool["parameters"]["properties"]["city"]["type"], "string");
        assert!(tool.get("function").is_none());
        assert_eq!(
            responses["tool_choice"],
            json!({"type": "function", "name": "get_weather"})
        );

        let back = round_trip_request(chat);
        let tool = &back["tools"][0];
        assert_eq!(tool["type"], "function");
        assert_eq!(tool["function"]["name"], "get_weather");
        assert_eq!(tool["function"]["description"], "Look up weather.");
        assert_eq!(tool["function"]["strict"], true);
        assert_eq!(tool["function"]["parameters"]["required"], json!(["city"]));
        assert_eq!(
            back["tool_choice"],
            json!({"type": "function", "function": {"name": "get_weather"}})
        );
    }

    #[test]
    fn function_parameters_are_normalized_for_strict_upstreams() {
        let responses = chat_request_to_responses(json!({
            "messages": [{"role": "user", "content": "go"}],
            "tools": [{"type": "function", "function": {"name": "noop"}}]
        }))
        .unwrap();

        // parameters 缺失时补成显式空 object schema，与正向转换的规范化一致。
        assert_eq!(
            responses["tools"][0]["parameters"],
            json!({"type": "object", "properties": {}})
        );
    }

    #[test]
    fn tool_call_history_round_trips_with_stable_ids_and_arguments() {
        let chat = json!({
            "model": "gpt-5.1",
            "messages": [
                {"role": "user", "content": "weather?"},
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc123",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\":\"Paris\",\"unit\":\"c\"}"
                        }
                    }]
                },
                {"role": "tool", "tool_call_id": "call_abc123", "content": "18C"}
            ]
        });

        let responses = chat_request_to_responses(chat.clone()).unwrap();
        let input = responses["input"].as_array().unwrap();
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[1]["call_id"], "call_abc123");
        assert_eq!(input[1]["name"], "get_weather");
        assert_eq!(input[1]["arguments"], "{\"city\":\"Paris\",\"unit\":\"c\"}");
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(input[2]["call_id"], "call_abc123");
        assert_eq!(input[2]["output"], "18C");

        let back = round_trip_request(chat);
        let messages = back["messages"].as_array().unwrap();
        let call = &messages[1]["tool_calls"][0];
        assert_eq!(call["id"], "call_abc123");
        assert_eq!(call["function"]["name"], "get_weather");
        assert_eq!(
            call["function"]["arguments"],
            "{\"city\":\"Paris\",\"unit\":\"c\"}"
        );
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "call_abc123");
        assert_eq!(messages[2]["content"], "18C");
    }

    #[test]
    fn response_tool_calls_preserve_id_and_arguments() {
        let chat = responses_response_to_chat(json!({
            "id": "resp_xyz",
            "status": "completed",
            "model": "gpt-5.1",
            "created_at": 1700,
            "output": [{
                "id": "fc_call_1",
                "type": "function_call",
                "status": "completed",
                "call_id": "call_1",
                "name": "get_weather",
                "arguments": "{\"city\":\"Paris\"}"
            }]
        }))
        .unwrap();

        let message = first_message(&chat);
        assert_eq!(chat["id"], "xyz");
        assert_eq!(chat["object"], "chat.completion");
        assert_eq!(chat["created"], 1700);
        // 只有工具调用时 content 必须是 null，而不是空串。
        assert_eq!(message["content"], Value::Null);
        assert_eq!(chat["choices"][0]["finish_reason"], "tool_calls");
        let call = &message["tool_calls"][0];
        assert_eq!(call["id"], "call_1");
        assert_eq!(call["type"], "function");
        assert_eq!(call["function"]["name"], "get_weather");
        assert_eq!(call["function"]["arguments"], "{\"city\":\"Paris\"}");
    }

    #[test]
    fn custom_tool_call_keeps_raw_input_for_chat_clients() {
        let chat = responses_response_to_chat(json!({
            "id": "resp_1",
            "status": "completed",
            "output": [{
                "id": "ctc_call_9",
                "type": "custom_tool_call",
                "call_id": "call_9",
                "name": "run_patch",
                "input": "line one\nline two"
            }]
        }))
        .unwrap();

        let call = &first_message(&chat)["tool_calls"][0];
        assert_eq!(call["id"], "call_9");
        assert_eq!(call["type"], "custom");
        assert_eq!(call["custom"]["name"], "run_patch");
        // 原始字符串必须逐字保留，不能被 JSON 包装/转义。
        assert_eq!(call["custom"]["input"], "line one\nline two");
    }

    #[test]
    fn custom_tool_call_round_trips_through_request_direction() {
        let responses = chat_request_to_responses(json!({
            "messages": [{
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_9",
                    "type": "custom",
                    "custom": {"name": "run_patch", "input": "*** patch ***"}
                }]
            }]
        }))
        .unwrap();

        let item = &responses["input"][0];
        assert_eq!(item["type"], "custom_tool_call");
        assert_eq!(item["call_id"], "call_9");
        assert_eq!(item["name"], "run_patch");
        assert_eq!(item["input"], "*** patch ***");
    }

    #[test]
    fn reasoning_content_round_trips_through_both_directions() {
        let chat = json!({
            "model": "gpt-5.1",
            "messages": [
                {"role": "user", "content": "2+2?"},
                {
                    "role": "assistant",
                    "content": "4",
                    "reasoning_content": "Add two and two."
                },
                {"role": "user", "content": "and 3+3?"}
            ]
        });

        let responses = chat_request_to_responses(chat.clone()).unwrap();
        let input = responses["input"].as_array().unwrap();
        // reasoning 是 assistant 之前的独立 item。
        assert_eq!(input[1]["type"], "reasoning");
        assert_eq!(input[1]["summary"][0]["type"], "summary_text");
        assert_eq!(input[1]["summary"][0]["text"], "Add two and two.");
        assert_eq!(input[2]["role"], "assistant");
        assert_eq!(input[2]["content"][0]["text"], "4");

        let back = round_trip_request(chat);
        let assistant = &back["messages"][1];
        assert_eq!(assistant["role"], "assistant");
        assert_eq!(assistant["content"], "4");
        assert_eq!(assistant["reasoning_content"], "Add two and two.");
    }

    #[test]
    fn response_reasoning_becomes_reasoning_content_and_replays_encrypted_payload() {
        let chat = responses_response_to_chat(json!({
            "id": "resp_2",
            "status": "completed",
            "output": [
                {
                    "id": "rs_2",
                    "type": "reasoning",
                    "summary": [{"type": "summary_text", "text": "Think first."}],
                    "encrypted_content": "opaque-blob"
                },
                {
                    "id": "msg_2",
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "done", "annotations": []}]
                }
            ]
        }))
        .unwrap();

        let message = first_message(&chat);
        assert_eq!(message["content"], "done");
        assert_eq!(message["reasoning_content"], "Think first.");
        // 不透明载荷回写到 message.reasoning，供下一轮原样带回。
        assert_eq!(message["reasoning"]["id"], "rs_2");
        assert_eq!(message["reasoning"]["encrypted_content"], "opaque-blob");

        // 客户端把整条 assistant 消息带回来时，encrypted_content 必须复原。
        let replayed = chat_request_to_responses(json!({
            "messages": [
                {"role": "user", "content": "go"},
                message.clone()
            ]
        }))
        .unwrap();
        let reasoning = &replayed["input"][1];
        assert_eq!(reasoning["type"], "reasoning");
        assert_eq!(reasoning["id"], "rs_2");
        assert_eq!(reasoning["encrypted_content"], "opaque-blob");
        assert_eq!(reasoning["summary"][0]["text"], "Think first.");
    }

    #[test]
    fn inline_think_block_moves_into_reasoning_item_without_duplication() {
        let responses = chat_request_to_responses(json!({
            "messages": [{
                "role": "assistant",
                "content": "<think>weighing options</think>final answer"
            }]
        }))
        .unwrap();

        let input = responses["input"].as_array().unwrap();
        assert_eq!(input[0]["type"], "reasoning");
        assert_eq!(input[0]["summary"][0]["text"], "weighing options");
        // 正文只保留答案，思考不重复出现。
        assert_eq!(input[1]["content"][0]["text"], "final answer");
    }

    #[test]
    fn reasoning_effort_and_thinking_toggles_map_to_responses_reasoning() {
        let responses = chat_request_to_responses(json!({
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning_effort": "high"
        }))
        .unwrap();
        assert_eq!(responses["reasoning"], json!({"effort": "high"}));
        // Chat 专属字段已翻译，不再原样残留，避免与 reasoning 冲突触发 400。
        assert!(responses.get("reasoning_effort").is_none());

        let disabled = chat_request_to_responses(json!({
            "messages": [{"role": "user", "content": "hi"}],
            "thinking": {"type": "disabled"}
        }))
        .unwrap();
        // 显式关闭必须表达出来，否则默认开思考的模型关不掉。
        assert_eq!(disabled["reasoning"], json!({"effort": "none"}));
        assert!(disabled.get("thinking").is_none());

        let toggled = chat_request_to_responses(json!({
            "messages": [{"role": "user", "content": "hi"}],
            "enable_thinking": true
        }))
        .unwrap();
        // 只有开关没有档位：发空 reasoning 表达"要思考、档位未指定"，不臆造 effort。
        assert_eq!(toggled["reasoning"], json!({}));

        let none = chat_request_to_responses(json!({
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .unwrap();
        assert!(none.get("reasoning").is_none());
    }

    #[test]
    fn usage_field_names_map_in_both_directions_including_cache_tokens() {
        let responses_usage = json!({
            "input_tokens": 120,
            "output_tokens": 30,
            "total_tokens": 150,
            "input_tokens_details": {"cached_tokens": 100, "cache_write_tokens": 20},
            "output_tokens_details": {"reasoning_tokens": 12},
            "cache_read_input_tokens": 100,
            "cache_creation_input_tokens": 20
        });

        let chat_usage = responses_usage_to_chat_usage(Some(&responses_usage));
        assert_eq!(chat_usage["prompt_tokens"], 120);
        assert_eq!(chat_usage["completion_tokens"], 30);
        assert_eq!(chat_usage["total_tokens"], 150);
        assert_eq!(chat_usage["prompt_tokens_details"]["cached_tokens"], 100);
        assert_eq!(
            chat_usage["prompt_tokens_details"]["cache_write_tokens"],
            20
        );
        assert_eq!(
            chat_usage["completion_tokens_details"]["reasoning_tokens"],
            12
        );
        assert_eq!(chat_usage["cache_read_input_tokens"], 100);
        assert_eq!(chat_usage["cache_creation_input_tokens"], 20);

        // 再用生产正向函数转回去，确认字段名与数值双向一致。
        let round_tripped =
            super::super::transform_codex_chat::chat_usage_to_responses_usage(Some(&chat_usage));
        assert_eq!(round_tripped["input_tokens"], 120);
        assert_eq!(round_tripped["output_tokens"], 30);
        assert_eq!(round_tripped["total_tokens"], 150);
        assert_eq!(round_tripped["input_tokens_details"]["cached_tokens"], 100);
        assert_eq!(
            round_tripped["input_tokens_details"]["cache_write_tokens"],
            20
        );
        assert_eq!(
            round_tripped["output_tokens_details"]["reasoning_tokens"],
            12
        );
    }

    #[test]
    fn usage_defaults_to_zeroed_chat_shape_when_absent() {
        let usage = responses_usage_to_chat_usage(None);
        assert_eq!(usage["prompt_tokens"], 0);
        assert_eq!(usage["completion_tokens"], 0);
        assert_eq!(usage["total_tokens"], 0);
        assert_eq!(usage["prompt_tokens_details"]["cached_tokens"], 0);
        assert_eq!(usage["completion_tokens_details"]["reasoning_tokens"], 0);

        // total_tokens 缺失时由 input+output 推导，不静默归零。
        let derived = responses_usage_to_chat_usage(Some(&json!({
            "input_tokens": 7,
            "output_tokens": 5
        })));
        assert_eq!(derived["total_tokens"], 12);
    }

    #[test]
    fn finish_reason_maps_across_every_response_status() {
        // status=incomplete 一律是截断；reason 缺失也按截断处理。
        assert_eq!(
            finish_reason_from_response_status(
                Some("incomplete"),
                false,
                Some("max_output_tokens")
            ),
            "length"
        );
        assert_eq!(
            finish_reason_from_response_status(Some("incomplete"), false, None),
            "length"
        );
        assert_eq!(
            finish_reason_from_response_status(Some("incomplete"), false, Some("content_filter")),
            "content_filter"
        );
        // 截断优先于工具调用，与 map_responses_stop_reason 的判定顺序一致。
        assert_eq!(
            finish_reason_from_response_status(Some("incomplete"), true, Some("max_output_tokens")),
            "length"
        );
        assert_eq!(
            finish_reason_from_response_status(Some("completed"), true, None),
            "tool_calls"
        );
        assert_eq!(
            finish_reason_from_response_status(Some("completed"), false, None),
            "stop"
        );
        assert_eq!(
            finish_reason_from_response_status(None, false, None),
            "stop"
        );
    }

    #[test]
    fn finish_reason_survives_a_full_chat_response_round_trip() {
        for (finish_reason, expected) in [
            ("stop", "stop"),
            ("tool_calls", "tool_calls"),
            ("length", "length"),
        ] {
            let mut message = json!({"role": "assistant", "content": "text"});
            if finish_reason == "tool_calls" {
                message["content"] = Value::Null;
                message["tool_calls"] = json!([{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "noop", "arguments": "{}"}
                }]);
            }
            let responses = chat_completion_to_response(json!({
                "id": "chatcmpl-1",
                "model": "gpt-5.1",
                "created": 10,
                "choices": [{"index": 0, "message": message, "finish_reason": finish_reason}]
            }))
            .unwrap();

            let back = responses_response_to_chat(responses).unwrap();
            assert_eq!(
                back["choices"][0]["finish_reason"], expected,
                "finish_reason {finish_reason} did not survive the round trip"
            );
            assert_eq!(back["id"], "chatcmpl-1");
        }
    }

    #[test]
    fn incomplete_status_reports_length_with_truncated_text() {
        let chat = responses_response_to_chat(json!({
            "id": "resp_3",
            "status": "incomplete",
            "incomplete_details": {"reason": "max_output_tokens"},
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "partial"}]
            }]
        }))
        .unwrap();

        assert_eq!(chat["choices"][0]["finish_reason"], "length");
        assert_eq!(first_message(&chat)["content"], "partial");
    }

    #[test]
    fn error_bodies_convert_to_chat_error_shape() {
        // 标准 OpenAI 形状：字段逐个保留。
        let converted = responses_error_to_chat_error(Some(&json!({
            "error": {
                "message": "rate limited",
                "type": "rate_limit_error",
                "code": "429",
                "param": "model"
            }
        })));
        assert_eq!(converted["error"]["message"], "rate limited");
        assert_eq!(converted["error"]["type"], "rate_limit_error");
        assert_eq!(converted["error"]["code"], "429");
        assert_eq!(converted["error"]["param"], "model");

        // 裸字符串与空体都要有可读 message，不能吐出 null。
        assert_eq!(
            responses_error_to_chat_error(Some(&json!("boom")))["error"]["message"],
            "boom"
        );
        assert_eq!(
            responses_error_to_chat_error(None)["error"]["type"],
            "upstream_error"
        );

        // 顶层只有 message / detail 的最小错误。
        assert_eq!(
            responses_error_to_chat_error(Some(&json!({"detail": "bad gateway"})))["error"]
                ["message"],
            "bad gateway"
        );
    }

    #[test]
    fn failed_response_becomes_chat_error_instead_of_fake_success() {
        let chat = responses_response_to_chat(json!({
            "id": "resp_4",
            "status": "failed",
            "error": {"message": "upstream exploded", "type": "server_error"},
            "output": []
        }))
        .unwrap();

        // 失败绝不能伪装成 finish_reason=stop 的成功回合。
        assert!(chat.get("choices").is_none());
        assert_eq!(chat["error"]["message"], "upstream exploded");
        assert_eq!(chat["error"]["type"], "server_error");
    }

    #[test]
    fn refusal_is_surfaced_on_the_chat_message() {
        let chat = responses_response_to_chat(json!({
            "id": "resp_5",
            "status": "completed",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "refusal", "refusal": "I cannot help with that."}]
            }]
        }))
        .unwrap();

        let message = first_message(&chat);
        assert_eq!(message["refusal"], "I cannot help with that.");
        assert_eq!(message["content"], Value::Null);
        assert_eq!(chat["choices"][0]["finish_reason"], "stop");
    }

    #[test]
    fn missing_output_is_an_error_not_an_empty_completion() {
        assert!(responses_response_to_chat(json!({"id": "resp_6"})).is_err());
        assert!(chat_request_to_responses(json!({"model": "gpt-5.1"})).is_err());
    }

    #[test]
    fn multimodal_and_mid_conversation_system_messages_survive() {
        let responses = chat_request_to_responses(json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "what is this?"},
                    {"type": "image_url", "image_url": {"url": "https://x/y.png", "detail": "high"}}
                ]},
                {"role": "assistant", "content": "a chart"},
                {"role": "developer", "content": "Answer in French."},
                {"role": "user", "content": "and this?"}
            ]
        }))
        .unwrap();

        let input = responses["input"].as_array().unwrap();
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[0]["content"][1]["type"], "input_image");
        assert_eq!(input[0]["content"][1]["image_url"], "https://x/y.png");
        assert_eq!(input[0]["content"][1]["detail"], "high");
        // 中途的 developer 不降级成 user，指令优先级不被静默改写。
        assert_eq!(input[2]["role"], "developer");
        assert_eq!(input[2]["content"][0]["text"], "Answer in French.");
        // 只有开头的 system/developer 折叠进 instructions。
        assert!(responses.get("instructions").is_none());
    }

    #[test]
    fn unrecognized_top_level_fields_pass_through_instead_of_being_dropped() {
        let responses = chat_request_to_responses(json!({
            "model": "gpt-5.1",
            "messages": [{"role": "user", "content": "hi"}],
            "seed": 42,
            "response_format": {"type": "json_object"},
            "parallel_tool_calls": false,
            "vendor_specific_knob": "keep-me"
        }))
        .unwrap();

        // 不确定的字段宁可透传：静默丢弃会让上游行为无声改变。
        assert_eq!(responses["seed"], 42);
        assert_eq!(responses["text"]["format"], json!({"type": "json_object"}));
        assert!(responses.get("response_format").is_none());
        assert_eq!(responses["parallel_tool_calls"], false);
        assert_eq!(responses["vendor_specific_knob"], "keep-me");
    }

    #[test]
    fn unmappable_output_items_are_retained_not_silently_dropped() {
        let chat = responses_response_to_chat(json!({
            "id": "resp_7",
            "status": "completed",
            "output": [
                {"type": "web_search_call", "id": "ws_1", "status": "completed"},
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "answer"}]
                }
            ]
        }))
        .unwrap();

        assert_eq!(first_message(&chat)["content"], "answer");
        let unmapped = chat["ccswitch_unmapped_output"].as_array().unwrap();
        assert_eq!(unmapped.len(), 1);
        assert_eq!(unmapped[0]["type"], "web_search_call");
    }

    #[test]
    fn legacy_function_call_and_function_role_are_paired() {
        let responses = chat_request_to_responses(json!({
            "messages": [
                {
                    "role": "assistant",
                    "content": null,
                    "function_call": {"name": "lookup", "arguments": "{\"q\":\"x\"}"}
                },
                {"role": "function", "name": "lookup", "content": "result"}
            ]
        }))
        .unwrap();

        let input = responses["input"].as_array().unwrap();
        assert_eq!(input[0]["type"], "function_call");
        assert_eq!(input[0]["name"], "lookup");
        // 旧形态没有 id，用稳定占位保证与随后的 output 能配对。
        let call_id = input[0]["call_id"].as_str().unwrap();
        assert_eq!(input[1]["type"], "function_call_output");
        assert_eq!(input[1]["call_id"], call_id);
        assert!(!call_id.is_empty());
    }
}
