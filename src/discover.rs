//! `discover` 子命令：从网页 HTML 里发现 RSS/Atom/JSON feed 候选
//! （feedfinder：`<link>`/`<a>` 标签 + 常见建站程序猜测），
//! 逐个抓取并验证是否能解析成 feed，最后列出结果。

use anyhow::{Context, Result};
use futures::stream::StreamExt;

use crate::{config::Config, sync};

pub async fn run(config: &Config, url: &str) -> Result<()> {
    let url = url::Url::parse(url).with_context(|| format!("parse page url {url}"))?;
    let client = sync::http_client(config)?;

    let html = client
        .get(url.clone())
        .send()
        .await
        .with_context(|| format!("fetch {url}"))?
        .error_for_status()
        .with_context(|| format!("fetch {url}"))?
        .text()
        .await
        .with_context(|| format!("read {url}"))?;

    let candidates =
        feedfinder::detect_feeds(&url, &html).with_context(|| format!("detect feeds in {url}"))?;
    if candidates.is_empty() {
        println!("{url} 里没有发现 feed 候选");
        return Ok(());
    }

    println!("在 {url} 发现 {} 个候选，逐个验证：", candidates.len());
    let results = futures::stream::iter(&candidates)
        .map(|candidate| verify_feed(&client, candidate))
        .buffer_unordered(config.sync.concurrency)
        .collect::<Vec<_>>()
        .await;
    for (feed, result) in results {
        match result {
            Ok(()) => println!(
                "✓ {} ({:?}) {}",
                feed.title().unwrap_or_default(),
                feed.feed_type(),
                feed.url()
            ),
            Err(e) => println!(
                "✗ {} ({:?}) {} — {e:#}",
                feed.title().unwrap_or_default(),
                feed.feed_type(),
                feed.url()
            ),
        }
    }
    Ok(())
}

/// 抓取候选并验证能否解析成 feed。
async fn verify_feed<'a>(
    client: &reqwest::Client,
    feed: &'a feedfinder::Feed,
) -> (&'a feedfinder::Feed, Result<()>) {
    let result = async {
        let response = client
            .get(feed.url().clone())
            .send()
            .await
            .with_context(|| format!("fetch {}", feed.url()))?;
        let status = response.status();
        let response = response
            .error_for_status()
            .with_context(|| format!("fetch {}: http {status}", feed.url()))?;
        let bytes = response
            .bytes()
            .await
            .with_context(|| format!("read {}", feed.url()))?;
        feed_rs::parser::parse(&*bytes)
            .map(|_| ())
            .with_context(|| format!("{} 不是可解析的 feed", feed.url()))
    }
    .await;
    (feed, result)
}
