//! webhook 渠道：群自定义机器人，无鉴权纯 POST，可选 HMAC 签名。

use anyhow::{Context, Result};
use garde::Validate;
use serde::Deserialize;

use super::{Channel, ChannelImpl};

/// webhook 渠道的配置。
#[derive(Debug, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct WebhookConfig {
    #[garde(length(min = 1))]
    pub name: String,
    /// webhook 完整地址，形如 `https://open.feishu.cn/open-apis/bot/v2/hook/<token>`。
    #[garde(skip)]
    pub url: url::Url,
    /// 机器人安全设置的签名密钥；配置后按飞书签名算法追加 timestamp/sign。
    #[serde(default)]
    #[garde(length(min = 1))]
    pub secret: Option<String>,
    /// 标签白名单：非空时只推送带这些标签（任一命中）的条目。
    #[serde(default)]
    #[garde(inner(length(min = 1)))]
    pub tags: Vec<String>,
}

impl super::Config for WebhookConfig {
    fn name(&self) -> &str {
        &self.name
    }

    fn tags(&self) -> &[String] {
        &self.tags
    }

    fn build(&self) -> Result<Channel> {
        Ok(Channel::from(WebhookChannel::new(self)))
    }
}

/// webhook 渠道的运行时实现：持有签名后的完整地址与 HTTP 客户端。
pub struct WebhookChannel {
    url: url::Url,
    http: reqwest::Client,
}

impl WebhookChannel {
    fn new(config: &WebhookConfig) -> Self {
        let url = match &config.secret {
            Some(secret) => sign_webhook(&config.url, secret),
            None => config.url.clone(),
        };
        Self {
            url,
            http: reqwest::Client::new(),
        }
    }
}

impl ChannelImpl for WebhookChannel {
    async fn send(&self, content_json: &str) -> Result<()> {
        let response = self
            .http
            .post(self.url.as_str())
            .json(&WebhookMessage {
                msg_type: "text",
                content: content_json,
            })
            .send()
            .await
            .context("feishu webhook send")?;
        if !response.status().is_success() {
            anyhow::bail!("feishu webhook http {}", response.status());
        }
        let body: WebhookResponse = response.json().await.context("parse webhook response")?;
        if body.code != 0 {
            anyhow::bail!(
                "feishu webhook rejected: code {}, msg {}",
                body.code,
                body.msg
            );
        }
        Ok(())
    }
}

/// 按飞书自定义机器人签名算法给 webhook 地址追加 timestamp/sign 查询参数：
/// HMAC-SHA256 的 key 是 `{timestamp}\n{secret}`，消息体为空字符串。
fn sign_webhook(url: &url::Url, secret: &str) -> url::Url {
    use base64::Engine;
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;

    let timestamp = chrono::Utc::now().timestamp();
    let string_to_sign = format!("{timestamp}\n{secret}");
    let mut mac =
        Hmac::<Sha256>::new_from_slice(string_to_sign.as_bytes()).expect("HMAC 接受任意长度的 key");
    mac.update(b"");
    let sign = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
    let mut signed = url.clone();
    // query_pairs_mut 会按表单规则编码 base64 里的 +/= 字符。
    let _serializer = signed
        .query_pairs_mut()
        .append_pair("timestamp", &timestamp.to_string())
        .append_pair("sign", &sign);
    signed
}

/// 自定义机器人 webhook 请求体。
#[derive(serde::Serialize)]
struct WebhookMessage<'a> {
    msg_type: &'a str,
    content: &'a str,
}

/// 自定义机器人 webhook 响应体。
#[derive(serde::Deserialize)]
struct WebhookResponse {
    code: i64,
    msg: String,
}
