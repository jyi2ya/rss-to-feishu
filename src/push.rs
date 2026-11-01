//! `push` 子命令：把 pending 条目经 LLM 摘要后推到配置的飞书渠道。
//!
//! 发送全部 pending 条目（可用 `--limit` 限量）。失败按出错点落入不同状态，
//! 状态字符串由 strum 从 `ItemError` variant 名生成（snake_case）。

use anyhow::{Context, Result};
use sqlx::SqlitePool;
use strum::AsRefStr;
use thiserror::Error;
use tracing::{info, warn};

use crate::{
    channel::{Channel, ChannelConfig},
    // 只为了方法解析（accepts/send），不占名字。
    channel::{ChannelImpl as _, Config as _},
    clients,
    config::Config,
    db::{self, PendingItem},
    llm::{Preview, Summarizer},
};

/// push 过程中的失败类型：variant 名（snake_case）加 `failed_` 前缀
/// 就是落库的状态字符串。
#[derive(Debug, Error, AsRefStr)]
#[strum(serialize_all = "snake_case", prefix = "failed_")]
pub enum ItemError {
    /// 正文抓取失败（fetch 条目经 Fetch 工具抓全文）。
    #[error("content fetch failed: {0:#}")]
    Fetch(anyhow::Error),
    /// LLM 结构化提取失败。
    #[error("llm structured extraction failed: {0:#}")]
    Llm(anyhow::Error),
    /// 摘要成功但渠道投递失败。
    #[error("channel delivery failed: {0:#}")]
    Send(anyhow::Error),
    /// 其余失败（消息编码、无匹配渠道、落库错误等）。
    #[error("{0:#}")]
    Other(anyhow::Error),
}

pub async fn run(config: &Config, limit: u32) -> Result<()> {
    let pool = db::connect(&config.database_path).await;
    let summarizer = Summarizer::new(&config.llm)?;
    let search = clients::search_client(config)?;
    // 启动时把每个渠道配置 build 成运行时实现；tenant 的 token 在这里读取一次。
    let prepared = prepare_channels(config)?;

    let items = db::pending_items(&pool, limit).await;

    if items.is_empty() {
        info!("no pending items");
        return Ok(());
    }
    info!("pushing {} pending items", items.len());

    let mut sent = 0usize;
    let mut failed = 0usize;
    for item in items {
        match process_item(&pool, &summarizer, &search, &prepared, &item).await {
            Ok(()) => sent += 1,
            Err(e) => {
                failed += 1;
                // 按失败类型分别记日志；落库状态由 strum 从 variant 名生成。
                match &e {
                    ItemError::Fetch(source) => {
                        warn!("item {} fetch failed: {source:#}", item.id)
                    }
                    ItemError::Llm(source) => {
                        warn!("item {} llm failed: {source:#}", item.id)
                    }
                    ItemError::Send(source) => {
                        warn!("item {} send failed: {source:#}", item.id)
                    }
                    ItemError::Other(source) => {
                        warn!("item {} failed: {source:#}", item.id)
                    }
                }
                db::mark_failed(&pool, &item.id, e.as_ref(), &format!("{e:#}")).await;
            }
        }
    }
    info!("push done: {sent} sent, {failed} failed");
    if failed > 0 {
        anyhow::bail!("push finished with {failed} failed item(s)");
    }
    Ok(())
}

/// 配置与启动时构建好的运行时实现成对出现。
struct PreparedChannel<'a> {
    config: &'a ChannelConfig,
    channel: Channel,
}

/// 把每个渠道配置 build 成运行时实现。
fn prepare_channels(config: &Config) -> Result<Vec<PreparedChannel<'_>>> {
    config
        .channels
        .iter()
        .map(|channel_config| {
            Ok(PreparedChannel {
                config: channel_config,
                channel: channel_config
                    .build()
                    .with_context(|| format!("build channel {}", channel_config.name()))?,
            })
        })
        .collect()
}

async fn process_item(
    pool: &SqlitePool,
    summarizer: &Summarizer,
    search: &clients::search::Client,
    prepared: &[PreparedChannel<'_>],
    item: &PendingItem,
) -> Result<(), ItemError> {
    let content = build_content(search, item)
        .await
        .map_err(ItemError::Fetch)?;
    let preview = summarizer.preview(&content).await.map_err(ItemError::Llm)?;
    let rendered = render_message(&preview, item);

    // 渠道按配置顺序取首个白名单命中的；白名单为空的渠道兜底。
    let prepared = prepared
        .iter()
        .find(|prepared| prepared.config.accepts(&item.source_tags))
        .ok_or_else(|| {
            ItemError::Other(anyhow::anyhow!(
                "no channel matches tags {:?}",
                item.source_tags.0
            ))
        })?;

    prepared
        .channel
        .send(&rendered)
        .await
        .map_err(ItemError::Send)?;
    info!("item {} sent to {}", item.id, prepared.config.name());

    let tags_json = serde_json::to_string(&preview.tags)
        .map_err(|e| ItemError::Other(anyhow::Error::new(e)))?;
    db::mark_sent(
        pool,
        &item.id,
        chrono::Utc::now(),
        &preview.title,
        &preview.preview,
        &tags_json,
    )
    .await;
    Ok(())
}

/// 组装交给 LLM 的文本：fetch 条目读网页正文，其余用旧版模板拼原文。
async fn build_content(search: &clients::search::Client, item: &PendingItem) -> Result<String> {
    if item.fetch {
        let Some(link) = item.link.as_deref() else {
            return Err(ItemError::Fetch(anyhow::anyhow!("fetch item without link")).into());
        };
        let request = clients::search::types::FetchRequest {
            url: link.to_string(),
        };
        let response = search.fetch(&request).await.map_err(search_error)?;
        let content = response.into_inner().content;
        if content.trim().is_empty() {
            anyhow::bail!("search returned empty content for {link}");
        }
        Ok(content)
    } else {
        let pub_date = item.pub_date.unwrap_or(item.enqueued_at);
        Ok(format!(
            "【{}】{}\n发布于 {}\n\n{}",
            item.source,
            item.title,
            pub_date.to_rfc3339(),
            item.description_text
        ))
    }
}

fn search_error(e: clients::search::Error<clients::search::types::ErrorResponse>) -> anyhow::Error {
    match e {
        clients::search::Error::ErrorResponse(response) => {
            let err = response.into_inner();
            anyhow::anyhow!("search: {} ({})", err.error.message, err.error.code)
        }
        other => anyhow::anyhow!("search request failed: {other}"),
    }
}

/// 渲染最终推送文本，格式沿用旧版 push.pl。
fn render_message(preview: &Preview, item: &PendingItem) -> String {
    let tags = preview
        .tags
        .iter()
        .map(|tag| format!("#{tag}"))
        .collect::<Vec<_>>()
        .join(" ");
    let pub_date = item.pub_date.unwrap_or(item.enqueued_at);
    let link = item.link.as_deref().unwrap_or("[NO LINK]");
    format!(
        "{}\n\n{}\n\n{}\n\n{} @{}\n\n{}",
        preview.title,
        preview.preview,
        tags,
        item.source,
        pub_date.to_rfc3339(),
        link
    )
}

/// ItemError 的状态字符串必须与 Status 枚举一致，防止两处漂移。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_variant_maps_to_status() {
        assert_eq!(
            ItemError::Fetch(anyhow::anyhow!("x")).as_ref(),
            db::Status::FailedFetch.as_ref()
        );
        assert_eq!(
            ItemError::Llm(anyhow::anyhow!("x")).as_ref(),
            db::Status::FailedLlm.as_ref()
        );
        assert_eq!(
            ItemError::Send(anyhow::anyhow!("x")).as_ref(),
            db::Status::FailedSend.as_ref()
        );
        assert_eq!(
            ItemError::Other(anyhow::anyhow!("x")).as_ref(),
            db::Status::FailedOther.as_ref()
        );
    }
}
