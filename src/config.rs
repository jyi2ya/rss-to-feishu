//! TOML 配置：figment 负责加载，garde 负责声明式校验。
//! 必填字段缺失由 serde 在反序列化时报错，取值约束全部用 garde 属性表达。

use std::path::{Path, PathBuf};

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
    /// 订阅源列表。
    #[serde(default)]
    #[garde(dive)]
    pub feeds: Vec<Feed>,
    /// 飞书推送渠道数组。
    #[serde(default)]
    #[garde(dive)]
    pub channels: Vec<crate::channel::ChannelConfig>,
    #[garde(dive)]
    pub llm: Llm,
    #[garde(dive)]
    pub fetch: Fetch,
    #[serde(default)]
    #[garde(dive)]
    pub sync: Sync,
}

/// 一个订阅源。
#[derive(Debug, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct Feed {
    /// feed 地址（serde 反序列化时校验 URL 合法性）。
    #[garde(skip)]
    pub url: url::Url,
    /// 正文直读：push 时经 Fetch 工具抓网页正文，feed 里的摘要可缺失。
    #[serde(default)]
    #[garde(skip)]
    pub fetch: bool,
    #[serde(default)]
    #[garde(inner(length(min = 1)))]
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct Llm {
    /// OpenAI 兼容 chat completions 的 base URL。
    #[garde(skip)]
    pub api_base: url::Url,
    /// API key；本地网关不需要鉴权时留空。
    #[serde(default)]
    #[garde(length(min = 1))]
    pub api_key: Option<String>,
    /// 后端模型名。
    #[garde(length(min = 1))]
    pub model: String,
}

/// 网页抓取服务：DeepSeek Search API 的 Fetch 工具。
/// base_url 可指向官方 API，也可指向本地兼容实现。
#[derive(Debug, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct Fetch {
    /// 服务 base URL：官方为 `https://api.deepseek.com`，
    /// 本地兼容实现形如 `http://127.0.0.1:18620`。
    #[garde(skip)]
    pub base_url: url::Url,
    /// 官方 API 需要的 DeepSeek API key；本地实现缺省不需要。
    #[serde(default)]
    #[garde(length(min = 1))]
    pub api_key: Option<String>,
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
    /// HTTP 代理；缺省时走系统代理（`http_proxy`/`https_proxy` 环境变量）。
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

const fn default_concurrency() -> usize {
    8
}

const fn default_timeout_secs() -> u64 {
    60
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
}
