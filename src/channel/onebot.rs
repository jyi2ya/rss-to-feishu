//! onebot 渠道：OneBot 11 HTTP API（go-cqhttp / NapCat / LLOneBot 的 http 模式），
//! 发群聊与私聊消息；access_token 可选，从环境变量读取。

use anyhow::{Context, Result};
use garde::Validate;
use serde::Deserialize;

use super::{Channel, ChannelImpl};

/// onebot 渠道的配置。
#[derive(Debug, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct OnebotConfig {
    #[garde(length(min = 1))]
    pub name: String,
    /// OneBot HTTP API 地址，如 `http://127.0.0.1:5700`。
    #[garde(skip)]
    pub url: url::Url,
    /// 要推送的群号列表。
    #[serde(default)]
    #[garde(skip)]
    pub groups: Vec<i64>,
    /// 要推送的 QQ 号列表（私聊）。
    #[serde(default)]
    #[garde(skip)]
    pub privates: Vec<i64>,
    /// 存 access_token 的环境变量名；本地 http 服务未设鉴权时可省略。
    #[serde(default)]
    #[garde(length(min = 1))]
    pub token_env: Option<String>,
    /// 标签白名单：非空时只推送带这些标签（任一命中）的条目。
    #[serde(default)]
    #[garde(inner(length(min = 1)))]
    pub tags: Vec<String>,
}

impl super::Config for OnebotConfig {
    fn name(&self) -> &str {
        &self.name
    }

    fn tags(&self) -> &[String] {
        &self.tags
    }

    fn build(&self) -> Result<Channel> {
        if self.groups.is_empty() && self.privates.is_empty() {
            anyhow::bail!("onebot 渠道 {} 没有配置 groups/privates", self.name);
        }
        let token = self
            .token_env
            .as_deref()
            .map(super::env_credential)
            .transpose()?;
        Ok(Channel::from(OnebotChannel::new(self, token.as_deref())?))
    }
}

/// onebot 渠道的运行时实现：持有目标群/私聊列表与 HTTP API 客户端。
///
/// 多个目标逐个发送，任一失败即报错；重推时已成功的目标会重复收到。
pub struct OnebotChannel {
    groups: Vec<i64>,
    privates: Vec<i64>,
    client: crate::clients::onebot::Client,
}

impl OnebotChannel {
    fn new(config: &OnebotConfig, token: Option<&str>) -> Result<Self> {
        Ok(Self {
            groups: config.groups.clone(),
            privates: config.privates.clone(),
            client: crate::clients::onebot_client(&config.url, token)?,
        })
    }

    async fn send_group(&self, group_id: i64, text: &str) -> Result<()> {
        let response = self
            .client
            .send_group_msg(&crate::clients::onebot::types::SendGroupMsgRequest {
                group_id,
                message: text.to_string(),
                auto_escape: true,
            })
            .await
            .with_context(|| format!("onebot send_group_msg to {group_id}"))?;
        let response = response.into_inner();
        if response.retcode != 0 || response.status != "ok" {
            anyhow::bail!(
                "onebot send_group_msg to {group_id} rejected: retcode {}, status {}, message {}",
                response.retcode,
                response.status,
                response
                    .message
                    .as_deref()
                    .or(response.wording.as_deref())
                    .unwrap_or("(无)")
            );
        }
        Ok(())
    }

    async fn send_private(&self, user_id: i64, text: &str) -> Result<()> {
        let response = self
            .client
            .send_private_msg(&crate::clients::onebot::types::SendPrivateMsgRequest {
                user_id,
                message: text.to_string(),
                auto_escape: true,
            })
            .await
            .with_context(|| format!("onebot send_private_msg to {user_id}"))?;
        let response = response.into_inner();
        if response.retcode != 0 || response.status != "ok" {
            anyhow::bail!(
                "onebot send_private_msg to {user_id} rejected: retcode {}, status {}, message {}",
                response.retcode,
                response.status,
                response
                    .message
                    .as_deref()
                    .or(response.wording.as_deref())
                    .unwrap_or("(无)")
            );
        }
        Ok(())
    }
}

impl ChannelImpl for OnebotChannel {
    async fn send(&self, text: &str) -> Result<()> {
        for group_id in &self.groups {
            self.send_group(*group_id, text).await?;
        }
        for user_id in &self.privates {
            self.send_private(*user_id, text).await?;
        }
        Ok(())
    }
}
