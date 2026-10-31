//! TOML 配置：所有配置项（包括订阅源列表）都从这里读，必填字段缺失时启动即报错。

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// sqlite 数据库文件路径。
    pub database_path: PathBuf,
    /// 订阅源：URL -> 标签列表。URL 带 `direct+` 前缀表示正文直读。
    pub feeds: BTreeMap<String, Vec<String>>,
    /// 飞书推送渠道数组。
    #[serde(default)]
    pub channels: Vec<Channel>,
    pub llm: Llm,
    pub zhuazhua: Zhuazhua,
    #[serde(default)]
    pub sync: Sync,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Llm {
    /// OpenAI 兼容 chat completions 的 base URL。
    pub api_base: String,
    pub api_key: String,
    /// 后端模型名。
    pub model: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Zhuazhua {
    /// zhuazhua 服务地址，如 `http://127.0.0.1:18620`。
    pub url: String,
    /// 服务端配置了 SCRAPER_API_TOKEN 时需要的鉴权头。
    #[serde(default)]
    pub api_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sync {
    /// 并发抓取的订阅源数量。
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    /// 单个订阅源的抓取超时（秒）。
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    /// HTTP 代理；缺省时回退到 `https_proxy`/`http_proxy` 环境变量。
    #[serde(default)]
    pub proxy: Option<String>,
}

impl Default for Sync {
    fn default() -> Self {
        Self {
            concurrency: default_concurrency(),
            timeout_secs: default_timeout_secs(),
            proxy: None,
        }
    }
}

fn default_concurrency() -> usize {
    8
}

fn default_timeout_secs() -> u64 {
    60
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Channel {
    /// 自建应用，通过 im/v1/messages 发送。
    Tenant {
        name: String,
        app_id: String,
        app_secret: String,
        chat_id: String,
        /// 标签白名单：非空时只推送带这些标签（任一命中）的条目。
        #[serde(default)]
        tags: Vec<String>,
        /// 飞书开放平台 base URL，缺省 `https://open.feishu.cn`。
        #[serde(default)]
        base_url: Option<String>,
    },
    /// 群自定义机器人 webhook。
    Webhook {
        name: String,
        /// webhook 完整地址，形如 `https://open.feishu.cn/open-apis/bot/v2/hook/<token>`。
        url: String,
        #[serde(default)]
        tags: Vec<String>,
    },
}

impl Channel {
    pub fn name(&self) -> &str {
        match self {
            Channel::Tenant { name, .. } | Channel::Webhook { name, .. } => name,
        }
    }

    pub fn tags(&self) -> &[String] {
        match self {
            Channel::Tenant { tags, .. } | Channel::Webhook { tags, .. } => tags,
        }
    }
}

/// 一份展开后的订阅源条目：`direct+` 前缀已拆出。
pub struct FeedEntry<'a> {
    pub url: &'a str,
    pub direct: bool,
    pub tags: &'a [String],
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("read config {}", path.display()))?;
        let config: Config =
            toml::from_str(&raw).with_context(|| format!("parse config {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.llm.api_base.trim().is_empty()
            || self.llm.api_key.trim().is_empty()
            || self.llm.model.trim().is_empty()
        {
            anyhow::bail!("llm.api_base / api_key / model must not be empty");
        }
        if self.zhuazhua.url.trim().is_empty() {
            anyhow::bail!("zhuazhua.url must not be empty");
        }
        if self.sync.concurrency == 0 {
            anyhow::bail!("sync.concurrency must be at least 1");
        }
        if self.sync.timeout_secs == 0 {
            anyhow::bail!("sync.timeout_secs must be at least 1");
        }
        for channel in &self.channels {
            match channel {
                Channel::Tenant {
                    app_id,
                    app_secret,
                    chat_id,
                    ..
                } => {
                    if app_id.trim().is_empty()
                        || app_secret.trim().is_empty()
                        || chat_id.trim().is_empty()
                    {
                        anyhow::bail!(
                            "channel {}: app_id / app_secret / chat_id must not be empty",
                            channel.name()
                        );
                    }
                }
                Channel::Webhook { url, .. } => {
                    if url.trim().is_empty() {
                        anyhow::bail!("channel {}: url must not be empty", channel.name());
                    }
                }
            }
        }
        Ok(())
    }

    pub fn feed_entries(&self) -> Vec<FeedEntry<'_>> {
        self.feeds
            .iter()
            .map(|(url, tags)| match url.strip_prefix("direct+") {
                Some(rest) => FeedEntry {
                    url: rest,
                    direct: true,
                    tags,
                },
                None => FeedEntry {
                    url,
                    direct: false,
                    tags,
                },
            })
            .collect()
    }
}
