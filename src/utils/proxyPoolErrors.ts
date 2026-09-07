/**
 * 代理池后端错误 → i18n 文案
 *
 * 后端用 AppError::localized 带了 key（proxyPool.error.*），但 Display 实现是
 * "{zh} ({en})"，key 并不会随 invoke 的错误字符串下发。所以这里沿用
 * translateMcpBackendError 的做法：按消息子串匹配。
 * 中英各匹配一次，避免后端将来只回其中一种语言时失配。
 *
 * 无法识别时返回空字符串，交由调用方回退到原始消息或默认文案。
 */

type Translate = (key: string, options?: Record<string, unknown>) => string;

const PATTERNS: Array<{ key: string; needles: string[] }> = [
  {
    key: "proxyPool.error.nameRequired",
    needles: ["订阅名称不能为空", "Subscription name is required"],
  },
  {
    key: "proxyPool.error.urlRequired",
    needles: ["远程订阅必须填写 URL", "Remote subscription requires a URL"],
  },
  {
    key: "proxyPool.error.contentRequired",
    needles: [
      "手动订阅内容不能为空",
      "Inline subscription content is required",
    ],
  },
  {
    key: "proxyPool.error.subscriptionNotFound",
    needles: ["订阅不存在", "Subscription not found"],
  },
];

export function translateProxyPoolError(message: string, t: Translate): string {
  if (!message) return "";
  const msg = String(message).trim();

  for (const { key, needles } of PATTERNS) {
    if (needles.some((needle) => msg.includes(needle))) {
      return t(key);
    }
  }

  // 未识别的错误（网络失败、解析失败等）原样透出，比泛化文案更有用
  return msg;
}
