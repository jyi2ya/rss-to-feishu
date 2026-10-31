//! `sync` 子命令：抓取配置里的全部订阅源，把新条目写入 sqlite。

use std::time::Duration;

use anyhow::{Context, Result};
use futures::stream::StreamExt;
use md5::{Digest, Md5};
use tracing::{info, warn};

use crate::{
    config::Config,
    db::{self, Item, STATUS_PENDING},
};

pub async fn run(config: &Config) -> Result<()> {
    let pool = db::connect(&config.database_path).await?;
    info!("db connected");
    let client = http_client(config)?;
    info!("http client built");
    let feeds = config.feed_entries();
    info!("{} feeds to fetch", feeds.len());

    let results = futures::stream::iter(feeds)
        .map(|entry| fetch_feed(&client, entry.url, entry.direct, entry.tags))
        .buffer_unordered(config.sync.concurrency)
        .collect::<Vec<_>>()
        .await;
    info!("all feeds settled");

    let mut fetched = 0usize;
    let mut new_items = Vec::new();
    for result in results {
        match result {
            Ok(items) => {
                fetched += items.len();
                new_items.extend(items);
            }
            Err(e) => warn!("{e:#}"),
        }
    }

    let existing = db::all_ids(&pool).await?;
    let fresh: Vec<Item> = new_items
        .into_iter()
        .filter(|item| !existing.contains(&item.id))
        .collect();
    let inserted = db::insert_items(&pool, &fresh).await?;

    info!("sync done: {fetched} entries fetched, {inserted} new enqueued");
    Ok(())
}

fn http_client(config: &Config) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .user_agent(concat!("rss-to-feishu/", env!("CARGO_PKG_VERSION")))
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
        let Some(description_text) = entry_text(&entry) else {
            warn!("{url}: entry without content, skip");
            continue;
        };
        let description_text = description_text.trim().to_string();
        if description_text.is_empty() {
            warn!("{url}: entry with empty content, skip");
            continue;
        }
        let title = entry
            .title
            .as_ref()
            .map(|text| text.content.trim().to_string())
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| channel_title.clone());
        let link = entry
            .links
            .first()
            .map(|link| link.href.clone())
            .unwrap_or_else(|| "[NO LINK]".to_string());
        // id 沿用旧版规则：md5("频道标题/条目id")，保证迁移后不重复入队。
        let entry_id = if entry.id.trim().is_empty() {
            link.clone()
        } else {
            entry.id.clone()
        };
        let id = format!("{:x}", Md5::digest(format!("{channel_title}/{entry_id}")));
        let pub_date = entry
            .published
            .or(entry.updated)
            .map(|date| date.to_rfc3339())
            .unwrap_or_default();
        let enqueued_at = chrono::Utc::now().to_rfc3339();
        let sort_key = db::compute_sort_key(&id, &pub_date, &enqueued_at);

        items.push(Item {
            id,
            source: channel_title.clone(),
            title,
            link,
            description_text,
            pub_date,
            is_direct: direct,
            source_tags: tags.join("|"),
            enqueued_at,
            sort_key,
            status: STATUS_PENDING.to_string(),
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
