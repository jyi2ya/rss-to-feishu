//! `sync` 子命令：抓取配置里的全部订阅源，把新条目写入 sqlite。

use std::time::Duration;

use anyhow::{Context, Result};
use futures::stream::StreamExt;
use md5::{Digest, Md5};
use tracing::{info, warn};

use crate::{
    config::Config,
    db::{self, Item},
};

pub async fn run(config: &Config) -> Result<()> {
    let pool = db::connect(&config.database_path).await;
    info!("db connected");
    let client = http_client(config)?;
    info!("http client built");
    let feeds = config.feed_entries();
    info!("{} feeds to fetch", feeds.len());

    // 并发抓取全部订阅源（buffer_unordered 限流），摊平收集所有条目；
    // 抓取失败的源打日志后丢弃。
    let items: Vec<Item> = futures::stream::iter(feeds)
        .map(|entry| fetch_feed(&client, entry.url, entry.direct, entry.tags))
        .buffer_unordered(config.sync.concurrency)
        .inspect(|result| {
            if let Err(e) = result {
                warn!("{e:#}");
            }
        })
        .filter_map(|result| async move { result.ok() })
        .flat_map(futures::stream::iter)
        .collect()
        .await;

    let fetched = items.len();

    // 全部条目一次性批量入库（单事务、多行 upsert）；
    // 新增数量不单独统计，需要时用 status 子命令从库里读。
    db::insert_items(&pool, &items).await;

    info!("sync done: {fetched} entries fetched");
    Ok(())
}

/// 抓订阅源时用的 User-Agent，供各 HTTP 客户端复用。
const USER_AGENT: &str = concat!("rss-to-feishu/", env!("CARGO_PKG_VERSION"));

fn http_client(config: &Config) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(config.sync.timeout_secs));
    if let Some(proxy) = config
        .sync
        .proxy
        .clone()
        .or_else(|| std::env::var("https_proxy").ok())
        .or_else(|| std::env::var("http_proxy").ok())
        .filter(|s| !s.trim().is_empty())
    {
        builder = builder.proxy(reqwest::Proxy::all(&proxy).with_context(|| "build proxy")?);
    }
    builder.build().context("build http client")
}

async fn fetch_feed(
    client: &reqwest::Client,
    url: &str,
    direct: bool,
    tags: &[String],
) -> Result<Vec<Item>> {
    info!("fetching {url}");
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("fetch {url}"))?
        .error_for_status()
        .with_context(|| format!("fetch {url}"))?;
    info!("{url}: got response");
    let bytes = response
        .bytes()
        .await
        .with_context(|| format!("read {url}"))?;
    let feed = feed_rs::parser::parse(&*bytes).with_context(|| format!("parse feed {url}"))?;

    let channel_title = feed
        .title
        .map(|text| text.content)
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| "[NO TITLE]".to_string());

    let mut items = Vec::new();
    for entry in feed.entries {
        // direct 源的条目正文在 push 时经 zhuazhua 抓网页，正文为空也入队；
        // 普通源没有正文就没东西可摘要，跳过。
        let description_text = entry_text(&entry).unwrap_or_default().trim().to_string();
        if !direct && description_text.is_empty() {
            warn!("{url}: entry with empty content, skip");
            continue;
        }
        let title = entry
            .title
            .as_ref()
            .map(|text| text.content.trim().to_string())
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| channel_title.clone());
        let link = entry.links.first().map(|link| link.href.clone());
        // id 沿用旧版规则：md5("频道标题/条目id")，保证迁移后不重复入队。
        // feed-rs 保证 entry.id 非空（缺省时用首条链接哈希兜底）。
        let id = format!("{:x}", Md5::digest(format!("{channel_title}/{}", entry.id)));
        let pub_date = db::sane_pub_date(entry.published.or(entry.updated));
        let enqueued_at = chrono::Utc::now();
        let sort_key = db::compute_sort_key(&id, pub_date, enqueued_at);

        items.push(Item {
            id,
            source: channel_title.clone(),
            title,
            link,
            description_text,
            pub_date,
            is_direct: direct,
            source_tags: db::Tags(tags.to_vec()),
            enqueued_at,
            sort_key,
            status: db::Status::Pending,
            llm_title: None,
            llm_summary: None,
            llm_tags: None,
            sent_at: None,
            error: None,
        });
    }
    info!("{url}: {} entries", items.len());
    Ok(items)
}

/// 条目正文：优先 content.body（feed-rs 已剥掉 HTML），回退 summary。
fn entry_text(entry: &feed_rs::model::Entry) -> Option<String> {
    entry
        .content
        .as_ref()
        .and_then(|content| content.body.clone())
        .or_else(|| entry.summary.as_ref().map(|text| text.content.clone()))
}
