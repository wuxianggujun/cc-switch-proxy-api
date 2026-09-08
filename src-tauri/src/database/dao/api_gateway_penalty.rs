//! 选线失败判罚
//!
//! 把上游错误映射成对密钥的处置：临时冷却（到期自动恢复）、硬状态（踢出候选集
//! 直到人工或额度重置清除）、或不判罚（问题在调用方，换 key 也没用）。
//!
//! 判罚粒度是**单个 key**，不是接入点。同一接入点下 A key 被限流不影响 B key。

use super::api_gateway_types::HardState;

/// 429 缺 `retry-after` 时的兜底冷却。
const DEFAULT_RATE_LIMIT_COOLDOWN_SECS: i64 = 300;
/// 上游过载（529）通常是瞬时抖动，冷却给短。
const OVERLOAD_COOLDOWN_SECS: i64 = 30;
/// 5xx / 网络层失败的冷却。够长以跳过一次故障窗口，又不至于让 key 长时间闲置。
const TRANSIENT_COOLDOWN_SECS: i64 = 60;
/// 403 未命中封号词时的冷却。可能是地域或临时策略限制。
const FORBIDDEN_COOLDOWN_SECS: i64 = 300;
/// 冷却上限。防止上游给出畸形的超大 retry-after 把 key 永久闲置。
const MAX_COOLDOWN_SECS: i64 = 3600;

/// 对一个密钥的处置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPenalty {
    /// 不判罚。错误源于调用方请求本身，换 key 无用，也不该污染 key 的健康度。
    None,
    /// 临时冷却，到期自动回到候选集。
    Cooldown(i64),
    /// 硬状态，需外部信号才恢复。
    Hard(HardState),
}

/// 上游 body 是否表示额度耗尽（而非普通限流）。
///
/// 二者都表现为 429，但处置完全不同：限流等几分钟就恢复，额度耗尽等到下个
/// 计费周期，继续轮询它纯属浪费。关键词取各家上游的实际错误文案。
fn indicates_quota_exhaustion(body: &str) -> bool {
    let lowered = body.to_ascii_lowercase();
    [
        "insufficient_quota",
        "insufficient quota",
        "billing_hard_limit_reached",
        "credit balance is too low",
        "exceeded your current quota",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
}

/// 上游 body 是否表示账号被封禁/停用。
fn indicates_ban(body: &str) -> bool {
    let lowered = body.to_ascii_lowercase();
    [
        "account_deactivated",
        "account deactivated",
        "account has been disabled",
        "account is suspended",
        "banned",
        "organization has been disabled",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
}

/// Retry-After can be a non-negative delay or an HTTP-date.
fn parse_retry_after(retry_after: Option<&str>) -> Option<i64> {
    let value = retry_after?.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(seconds.min(MAX_COOLDOWN_SECS as u64) as i64);
    }
    chrono::DateTime::parse_from_rfc2822(value)
        .ok()
        .map(|date| {
            date.signed_duration_since(chrono::Utc::now())
                .num_seconds()
                .max(0)
        })
}

fn clamp_cooldown(secs: i64) -> i64 {
    secs.clamp(1, MAX_COOLDOWN_SECS)
}

/// 按上游状态码与响应体决定处置。
///
/// `retry_after` 取自响应头，仅 429/503 采纳——其他状态码带这个头通常没有
/// 限流语义，误采会让 key 闲置过久。
pub fn classify_upstream_status(
    status: u16,
    body: Option<&str>,
    retry_after: Option<&str>,
) -> KeyPenalty {
    let body = body.unwrap_or("");

    match status {
        // 调用方请求自身的问题。换 key 结果一样，不判罚。
        400 | 404 | 405 | 406 | 413 | 414 | 415 | 422 | 501 => KeyPenalty::None,

        // 鉴权失败：key 失效或被吊销，轮询它只会持续失败。
        401 => KeyPenalty::Hard(HardState::AuthInvalid),

        // 余额不足按额度耗尽处理，等充值或周期重置。
        402 => KeyPenalty::Hard(HardState::QuotaExhausted),

        // 403 需分辨：封号是硬状态，其余（地域、策略）给冷却。
        403 => {
            if indicates_ban(body) {
                KeyPenalty::Hard(HardState::Banned)
            } else if indicates_quota_exhaustion(body) {
                KeyPenalty::Hard(HardState::QuotaExhausted)
            } else {
                KeyPenalty::Cooldown(FORBIDDEN_COOLDOWN_SECS)
            }
        }

        // 429 的分岔点：额度耗尽 → 踢出；普通限流 → 按 retry-after 冷却。
        429 => {
            if indicates_quota_exhaustion(body) {
                KeyPenalty::Hard(HardState::QuotaExhausted)
            } else {
                KeyPenalty::Cooldown(clamp_cooldown(
                    parse_retry_after(retry_after).unwrap_or(DEFAULT_RATE_LIMIT_COOLDOWN_SECS),
                ))
            }
        }

        // 上游过载，瞬时性强。
        529 => KeyPenalty::Cooldown(OVERLOAD_COOLDOWN_SECS),

        // 503 可能带 retry-after，采纳它。
        503 => KeyPenalty::Cooldown(clamp_cooldown(
            parse_retry_after(retry_after).unwrap_or(TRANSIENT_COOLDOWN_SECS),
        )),

        // 其余 5xx 与未归类状态：短冷却，换下一条。
        status if status >= 500 => KeyPenalty::Cooldown(TRANSIENT_COOLDOWN_SECS),

        // 其余 4xx 保守处理：可能是上游特有语义，给短冷却而非踢出。
        status if status >= 400 => KeyPenalty::Cooldown(TRANSIENT_COOLDOWN_SECS),

        // 2xx/3xx 不该走到这里。
        _ => KeyPenalty::None,
    }
}

/// 网络层失败（连接不上、超时、TLS 失败）。没有状态码可依据，一律短冷却。
pub fn classify_transport_failure() -> KeyPenalty {
    KeyPenalty::Cooldown(TRANSIENT_COOLDOWN_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_side_errors_do_not_penalize_key() {
        for status in [400, 404, 405, 413, 422, 501] {
            assert_eq!(
                classify_upstream_status(status, None, None),
                KeyPenalty::None,
                "status {status} 不应判罚 key"
            );
        }
    }

    #[test]
    fn unauthorized_marks_key_invalid() {
        assert_eq!(
            classify_upstream_status(401, None, None),
            KeyPenalty::Hard(HardState::AuthInvalid)
        );
    }

    #[test]
    fn rate_limit_honors_retry_after() {
        assert_eq!(
            classify_upstream_status(429, Some("slow down"), Some("42")),
            KeyPenalty::Cooldown(42)
        );
    }

    #[test]
    fn rate_limit_falls_back_when_retry_after_absent_or_invalid() {
        assert_eq!(
            classify_upstream_status(429, None, None),
            KeyPenalty::Cooldown(DEFAULT_RATE_LIMIT_COOLDOWN_SECS)
        );
        // Malformed values use the fallback.
        assert_eq!(
            classify_upstream_status(429, None, Some("invalid")),
            KeyPenalty::Cooldown(DEFAULT_RATE_LIMIT_COOLDOWN_SECS)
        );
        // Negative delays are invalid; zero is a valid immediate retry hint.
        assert_eq!(
            classify_upstream_status(429, None, Some("-1")),
            KeyPenalty::Cooldown(DEFAULT_RATE_LIMIT_COOLDOWN_SECS)
        );
    }

    #[test]
    fn retry_after_is_clamped() {
        assert_eq!(
            classify_upstream_status(429, None, Some("999999")),
            KeyPenalty::Cooldown(MAX_COOLDOWN_SECS)
        );
    }

    #[test]
    fn quota_exhaustion_beats_rate_limit_on_429() {
        assert_eq!(
            classify_upstream_status(429, Some(r#"{"error":"insufficient_quota"}"#), Some("30")),
            KeyPenalty::Hard(HardState::QuotaExhausted)
        );
        assert_eq!(
            classify_upstream_status(429, Some("RESOURCE_EXHAUSTED: quota exceeded"), None),
            KeyPenalty::Cooldown(DEFAULT_RATE_LIMIT_COOLDOWN_SECS)
        );
    }

    #[test]
    fn retry_after_accepts_http_dates_and_zero_delay() {
        let date = (chrono::Utc::now() + chrono::Duration::seconds(45)).to_rfc2822();
        assert!(matches!(
            classify_upstream_status(503, None, Some(&date)),
            KeyPenalty::Cooldown(43..=45)
        ));
        assert_eq!(
            classify_upstream_status(429, None, Some("0")),
            KeyPenalty::Cooldown(1)
        );
    }

    #[test]
    fn gemini_per_minute_quota_does_not_permanently_disable_a_key() {
        assert_eq!(
            classify_upstream_status(
                429,
                Some(
                    r#"{"error":{"status":"RESOURCE_EXHAUSTED","message":"Quota exceeded for requests per minute"}}"#
                ),
                Some("12")
            ),
            KeyPenalty::Cooldown(12)
        );
    }

    #[test]
    fn forbidden_distinguishes_ban_from_transient() {
        assert_eq!(
            classify_upstream_status(403, Some("account_deactivated"), None),
            KeyPenalty::Hard(HardState::Banned)
        );
        assert_eq!(
            classify_upstream_status(403, Some("region not supported"), None),
            KeyPenalty::Cooldown(FORBIDDEN_COOLDOWN_SECS)
        );
    }

    #[test]
    fn server_errors_get_short_cooldown() {
        assert_eq!(
            classify_upstream_status(500, None, None),
            KeyPenalty::Cooldown(TRANSIENT_COOLDOWN_SECS)
        );
        assert_eq!(
            classify_upstream_status(529, None, None),
            KeyPenalty::Cooldown(OVERLOAD_COOLDOWN_SECS)
        );
        assert_eq!(
            classify_upstream_status(503, None, Some("15")),
            KeyPenalty::Cooldown(15)
        );
    }

    #[test]
    fn transport_failure_cools_down() {
        assert_eq!(
            classify_transport_failure(),
            KeyPenalty::Cooldown(TRANSIENT_COOLDOWN_SECS)
        );
    }
}
