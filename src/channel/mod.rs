//! 推送渠道：每种渠道一个文件，配置与运行时实现各有一套 enum_dispatch 包装。
//!
//! - [`ChannelConfig`]：TOML 里反序列化出的渠道配置（serde 按 `type` 分派）；
//! - [`Channel`]：启动时由配置 build 出的运行时实现（负责真正发消息）。

pub mod tenant;
pub mod webhook;

/// 渠道配置的公共接口：由各渠道的 Config 结构体实现。
#[enum_dispatch::enum_dispatch]
pub trait Config {
    fn name(&self) -> &str;

    /// 标签白名单：空表示接收全部。
    fn tags(&self) -> &[String];

    /// 条目标签是否命中白名单。
    fn accepts(&self, item_tags: &[String]) -> bool {
        self.tags().is_empty() || self.tags().iter().any(|tag| item_tags.contains(tag))
    }

    /// 由配置构造运行时渠道实现。
    fn build(&self) -> anyhow::Result<Channel>;
}

/// 渠道配置的 enum_dispatch 包装：serde 按 `type` 字段分派到具体渠道。
#[derive(Debug, serde::Deserialize, garde::Validate)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
#[enum_dispatch::enum_dispatch(Config)]
pub enum ChannelConfig {
    Tenant(#[garde(dive)] tenant::TenantConfig),
    Webhook(#[garde(dive)] webhook::WebhookConfig),
}

/// 渠道运行时实现的公共接口。
#[enum_dispatch::enum_dispatch]
pub trait ChannelImpl {
    /// 发送一条文本消息（content 是飞书 text 消息的 content JSON 字符串）。
    async fn send(&self, content_json: &str) -> anyhow::Result<()>;
}

/// 渠道运行时实现的 enum_dispatch 包装。
#[enum_dispatch::enum_dispatch(ChannelImpl)]
pub enum Channel {
    Tenant(tenant::TenantChannel),
    Webhook(webhook::WebhookChannel),
}
