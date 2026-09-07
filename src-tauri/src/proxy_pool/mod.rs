//! 代理池：把大量 HTTP/SOCKS5 代理节点聚合成带粘性路由的统一出口
//!
//! ```text
//! proxy_pool/
//! ├── types.rs   - 节点/健康/订阅/租约模型
//! ├── parser.rs  - 订阅内容解析（URI 行 / Clash / sing-box）
//! └── pool.rs    - 节点池 + 粘性租约 + P2C 选路 + 熔断
//! ```
//!
//! 只支持 HTTP/HTTPS/SOCKS5 —— 这三种 reqwest 原生可用。vmess/vless/trojan
//! 等需要完整协议栈实现，解析时会跳过并计数。

pub mod parser;
pub mod pool;
pub mod probe;
pub mod service;
pub mod types;

pub use service::ProxyPoolService;

use once_cell::sync::OnceCell;

/// 进程级单例，供 forwarder 在请求路径上取用。
///
/// 用全局而非穿 ProxyState → ProxyServer → RequestForwarder 五层构造：
/// forwarder 在同一位置本来就调 `http_client::get_current_proxy_url()`，
/// 那也是 OnceCell 全局，这里保持对称。
static GLOBAL_POOL: OnceCell<ProxyPoolService> = OnceCell::new();

/// 注册全局代理池。重复调用会被忽略（首次生效）。
pub fn init_global(service: ProxyPoolService) {
    if GLOBAL_POOL.set(service).is_err() {
        log::warn!("[ProxyPool] 全局实例已注册，忽略重复注册");
    }
}

/// 取全局代理池。None 表示尚未注册（早于 setup 的调用，或测试环境）。
pub fn global() -> Option<&'static ProxyPoolService> {
    GLOBAL_POOL.get()
}
