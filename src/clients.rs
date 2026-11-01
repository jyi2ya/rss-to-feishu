//! 由 openapi/ 下的 spec 经 progenitor 宏在编译期生成的 HTTP 客户端。
//! 这里生成两个客户端：
//! - `feishu`：飞书开放平台的群消息接口，鉴权用现成的 tenant_access_token；
//! - `search`：DeepSeek Search API 的 URL-to-Markdown Fetch 工具，
//!   base_url 可指向官方 API 或本地兼容实现。

pub mod feishu {
    progenitor::generate_api!("openapi/feishu.yaml");
}

pub mod search {
    progenitor::generate_api!("openapi/search.yaml");
}

use anyhow::{Context, Result};

use crate::config::Config;

/// 用自定义 reqwest client 构造 fetch 客户端：
/// 生成客户端的默认总超时只有 15 秒，网页抓取动辄分钟级，这里不设总超时；
/// 配置了 api_key 时以 Bearer 头注入，与官方鉴权一致。
pub fn search_client(config: &Config) -> Result<search::Client> {
    let mut builder = reqwest::Client::builder();
    if let Some(api_key) = config.fetch.api_key.as_deref() {
        let mut headers = reqwest::header::HeaderMap::new();
        let value = reqwest::header::HeaderValue::from_str(&format!("Bearer {api_key}"))
            .context("invalid fetch api_key")?;
        // 不能用 debug_assert!(insert(...))：release 编译会整个丢掉插入动作。
        let previous = headers.insert(reqwest::header::AUTHORIZATION, value);
        debug_assert!(previous.is_none());
        builder = builder.default_headers(headers);
    }
    let http = builder.build().context("build fetch http client")?;
    Ok(search::Client::new_with_client(
        config.fetch.base_url.as_str(),
        http,
    ))
}

/// 用现成的 tenant_access_token 构造飞书群消息客户端：
/// token 以 Bearer 头注入，本工具不调登录接口（token 由外部轮换）。
pub fn feishu_client(base_url: &url::Url, token: &str) -> Result<feishu::Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    let value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
        .context("invalid tenant token")?;
    // 不能用 debug_assert!(insert(...))：release 编译会整个丢掉插入动作。
    let previous = headers.insert(reqwest::header::AUTHORIZATION, value);
    debug_assert!(previous.is_none());
    let http = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .context("build feishu http client")?;
    Ok(feishu::Client::new_with_client(base_url.as_str(), http))
}
