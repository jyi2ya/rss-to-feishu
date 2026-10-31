//! 由 openapi/ 下的 spec 经 progenitor 宏在编译期生成的 HTTP 客户端。
//! 飞书接口走 larksuite-oapi-sdk-rs（见 push.rs），这里只生成 zhuazhua。

pub mod zhuazhua {
    progenitor::generate_api!("openapi/zhuazhua.yaml");
}

use anyhow::{Context, Result};

use crate::config::Config;

/// 用自定义 reqwest client 构造 zhuazhua 客户端：
/// 生成客户端的默认总超时只有 15 秒，网页抓取动辄分钟级，这里不设总超时；
/// 可选 api token 以默认请求头注入。
pub fn zhuazhua_client(config: &Config) -> Result<zhuazhua::Client> {
    let mut builder = reqwest::Client::builder();
    if let Some(token) = config.zhuazhua.api_token.as_deref() {
        let mut headers = reqwest::header::HeaderMap::new();
        let value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
            .context("invalid zhuazhua api_token")?;
        headers.insert(reqwest::header::AUTHORIZATION, value);
        builder = builder.default_headers(headers);
    }
    let http = builder.build().context("build zhuazhua http client")?;
    Ok(zhuazhua::Client::new_with_client(
        &config.zhuazhua.url,
        http,
    ))
}
