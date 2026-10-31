//! TOML 配置：figment 负责加载，garde 负责声明式校验。
//! 必填字段缺失由 serde 在反序列化时报错，取值约束全部用 garde 属性表达。

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use figment::{
    Figment,
    providers::{Format, Toml},
};
use garde::Validate;
use serde::Deserialize;

#[derive(Debug, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// sqlite 数据库文件路径。
    #[garde(skip)]
    pub database_path: PathBuf,
    /// 订阅源：URL -> 标签列表。URL 带 `direct+` 前缀表示正文直读。
    #[garde(skip)]
    pub feeds: BTreeMap<String, Vec<String>>,
    /// 飞书推送渠道数组。
    #[serde(default)]
    #[garde(dive)]
    pub channels: Vec<Channel>,
    #[garde(dive)]
    pub llm: Llm,
    #[garde(dive)]
    pub zhuazhua: Zhuazhua,
    #[serde(default)]
    #[garde(dive)]
    pub sync: Sync,
}

#[derive(Debug, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct Llm {
    /// OpenAI 兼容 chat completions 的 base URL。
    #[garde(length(min = 1))]
    pub api_base: String,
    #[garde(length(min = 1))]
    pub api_key: String,
    /// 后端模型名。
    #[garde(length(min = 1))]
    pub model: String,
}

#[derive(Debug, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct Zhuazhua {
    /// zhuazhua 服务地址，如 `http://127.0.0.1:18620`。
    #[garde(length(min = 1))]
    pub url: String,
    /// 服务端配置了 SCRAPER_API_TOKEN 时需要的鉴权头。
    #[serde(default)]
    #[garde(length(min = 1))]
    pub api_token: Option<String>,
}

#[derive(Debug, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct Sync {
    /// 并发抓取的订阅源数量。
    #[serde(default = "default_concurrency")]
    #[garde(range(min = 1))]
    pub concurrency: usize,
    /// 单个订阅源的抓取超时（秒）。
    #[serde(default = "default_timeout_secs")]
    #[garde(range(min = 1))]
    pub timeout_secs: u64,
    /// HTTP 代理；缺省时回退到 `https_proxy`/`http_proxy` 环境变量。
    #[serde(default)]
    #[garde(length(min = 1))]
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

#[derive(Debug, Deserialize, Validate)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Channel {
    /// 自建应用，通过 im/v1/messages 发送。
    Tenant {
        #[garde(length(min = 1))]
        name: String,
        #[garde(length(min = 1))]
        app_id: String,
        #[garde(length(min = 1))]
        app_secret: String,
        #[garde(length(min = 1))]
        chat_id: String,
        /// 标签白名单：非空时只推送带这些标签（任一命中）的条目。
        #[serde(default)]
        #[garde(inner(length(min = 1)))]
        tags: Vec<String>,
        /// 飞书开放平台 base URL，缺省 `https://open.feishu.cn`。
        #[serde(default)]
        #[garde(length(min = 1))]
        base_url: Option<String>,
    },
    /// 群自定义机器人 webhook。
    Webhook {
        #[garde(length(min = 1))]
        name: String,
        /// webhook 完整地址，形如 `https://open.feishu.cn/open-apis/bot/v2/hook/<token>`。
        #[garde(length(min = 1))]
        url: String,
        #[serde(default)]
        #[garde(inner(length(min = 1)))]
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
        let config: Config = Figment::new()
            .merge(Toml::file(path))
            .extract()
            .with_context(|| format!("load config {}", path.display()))?;
        config
            .validate()
            .map_err(|report| anyhow::anyhow!("invalid config {}:\n{report}", path.display()))?;
        Ok(config)
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
