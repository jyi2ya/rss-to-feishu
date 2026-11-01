//! tenant 渠道：用现成的 tenant_access_token 调飞书 im/v1/messages 发群消息。
//! 凭据不入库、不调登录接口：token 由外部轮换，运行时从环境变量读取。

use anyhow::{Context, Result};
use garde::Validate;
use serde::Deserialize;

use super::{Channel, ChannelImpl};

/// tenant 渠道的配置。
#[derive(Debug, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct TenantConfig {
    #[garde(length(min = 1))]
    pub name: String,
    /// 存 tenant_access_token 的环境变量名。
    #[garde(length(min = 1))]
    pub token_env: String,
    #[garde(length(min = 1))]
    pub chat_id: String,
    /// 标签白名单：非空时只推送带这些标签（任一命中）的条目。
    #[serde(default)]
    #[garde(inner(length(min = 1)))]
    pub tags: Vec<String>,
    /// 飞书开放平台 base URL，缺省 `https://open.feishu.cn`。
    #[serde(default)]
    #[garde(skip)]
    pub base_url: Option<url::Url>,
}

impl super::Config for TenantConfig {
    fn name(&self) -> &str {
        &self.name
    }

    fn tags(&self) -> &[String] {
        &self.tags
    }

    fn build(&self) -> Result<Channel> {
        Ok(Channel::from(TenantChannel::new(self)?))
    }
}

/// tenant 渠道的运行时实现：持有群消息客户端与目标 chat_id。
pub struct TenantChannel {
    chat_id: String,
    client: crate::clients::feishu::Client,
}

impl TenantChannel {
    fn new(config: &TenantConfig) -> Result<Self> {
        let token = super::env_credential(&config.token_env)?;
        let base_url = config.base_url.clone().unwrap_or_else(|| {
            url::Url::parse("https://open.feishu.cn").expect("官方 base URL 是合法的常量")
        });
        Ok(Self {
            chat_id: config.chat_id.clone(),
            client: crate::clients::feishu_client(&base_url, &token)?,
        })
    }
}

impl ChannelImpl for TenantChannel {
    async fn send(&self, text: &str) -> Result<()> {
        let content = serde_json::to_string(&TextContent { text })?;
        let body = crate::clients::feishu::types::CreateMessageRequest {
            receive_id: self.chat_id.clone(),
            msg_type: "text".to_string(),
            content,
        };
        let response = self
            .client
            .create_message(
                crate::clients::feishu::types::CreateMessageReceiveIdType::ChatId,
                &body,
            )
            .await
            .context("feishu im send")?;
        let response = response.into_inner();
        if response.code != 0 {
            anyhow::bail!(
                "feishu im send rejected: code {}, msg {}",
                response.code,
                response.msg
            );
        }
        Ok(())
    }
}

/// 飞书 text 消息的 content 字段。
#[derive(serde::Serialize)]
struct TextContent<'a> {
    text: &'a str,
}
