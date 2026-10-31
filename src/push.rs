//! `push` 子命令：把 pending 条目经 LLM 摘要后推到配置的飞书渠道。
//!
//! 发送全部 pending 条目（可用 `--limit` 限量）。失败按阶段落入不同状态：
//! LLM 提取失败 -> `failed_llm`，摘要成功但渠道投递失败 -> `failed_send`，
//! 其余（正文抓取失败、无匹配渠道等）-> `failed`。

use anyhow::{Context, Result};
use larksuite_oapi_sdk_rs::{LarkClient, RequestOption, service::im::v1::CreateMessageReqBody};
use sqlx::SqlitePool;
use tracing::{info, warn};

use crate::{
    clients,
    config::{Channel, Config},
    db::{
        self, PendingItem, STATUS_FAILED, STATUS_FAILED_LLM, STATUS_FAILED_SEND, STATUS_PENDING,
        STATUS_SENT,
    },
    llm::{Preview, Summarizer},
};

/// push 过程中失败发生的阶段，决定条目落入哪个失败状态。
enum FailureStage {
    Llm,
    Send,
    Other,
}

impl FailureStage {
    fn status(&self) -> &'static str {
        match self {
            FailureStage::Llm => STATUS_FAILED_LLM,
            FailureStage::Send => STATUS_FAILED_SEND,
            FailureStage::Other => STATUS_FAILED,
        }
    }
}

/// 带失败阶段的错误，run 里据此写库。
struct ItemError {
    stage: FailureStage,
    source: anyhow::Error,
}

impl ItemError {
    fn llm(e: anyhow::Error) -> Self {
        Self {
            stage: FailureStage::Llm,
            source: e,
        }
    }

    fn send(e: anyhow::Error) -> Self {
        Self {
            stage: FailureStage::Send,
            source: e,
        }
    }

    fn other(e: anyhow::Error) -> Self {
        Self {
            stage: FailureStage::Other,
            source: e,
        }
    }
}

pub async fn run(config: &Config, limit: Option<usize>) -> Result<()> {
    let pool = db::connect(&config.database_path).await?;
    let summarizer = Summarizer::new(&config.llm)?;
    let zhuazhua = clients::zhuazhua_client(config)?;

    let items: Vec<PendingItem> = match limit {
        Some(n) => sqlx::query_as::<_, PendingItem>(
            "SELECT * FROM items WHERE status = ? ORDER BY sort_key, enqueued_at LIMIT ?",
        )
        .bind(STATUS_PENDING)
        .bind(n as i64)
        .fetch_all(&pool)
        .await
        .context("load pending items")?,
        None => sqlx::query_as::<_, PendingItem>(
            "SELECT * FROM items WHERE status = ? ORDER BY sort_key, enqueued_at",
        )
        .bind(STATUS_PENDING)
        .fetch_all(&pool)
        .await
        .context("load pending items")?,
    };

    if items.is_empty() {
        info!("no pending items");
        return Ok(());
    }
    info!("pushing {} pending items", items.len());

    let mut sent = 0usize;
    let mut failed = 0usize;
    for item in items {
        match process_item(&pool, &summarizer, &zhuazhua, config, &item).await {
            Ok(()) => sent += 1,
            Err(e) => {
                failed += 1;
                warn!("item {} failed: {:#}", item.id, e.source);
                mark_failed(
                    &pool,
                    &item.id,
                    e.stage.status(),
                    &format!("{:#}", e.source),
                )
                .await?;
            }
        }
    }
    info!("push done: {sent} sent, {failed} failed");
    if failed > 0 {
        anyhow::bail!("push finished with {failed} failed item(s)");
    }
    Ok(())
}

async fn process_item(
    pool: &SqlitePool,
    summarizer: &Summarizer,
    zhuazhua: &clients::zhuazhua::Client,
    config: &Config,
    item: &PendingItem,
) -> Result<(), ItemError> {
    let content = build_content(zhuazhua, item)
        .await
        .map_err(ItemError::other)?;
    let preview = summarizer.preview(&content).await.map_err(ItemError::llm)?;
    let rendered = render_message(&preview, item);
    let content_json = serde_json::to_string(&serde_json::json!({ "text": rendered }))
        .map_err(|e| ItemError::other(anyhow::anyhow!(e)))?;

    // 渠道按配置顺序取首个白名单命中的；白名单为空的渠道兜底。
    let channel = config
        .channels
        .iter()
        .find(|channel| tags_match(channel.tags(), &item.source_tags))
        .ok_or_else(|| {
            ItemError::other(anyhow::anyhow!(
                "no channel matches tags {:?}",
                item.source_tags
            ))
        })?;

    send_to_channel(channel, &content_json)
        .await
        .map_err(ItemError::send)?;
    info!("item {} sent to {}", item.id, channel.name());

    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE items SET status = ?, sent_at = ?, llm_title = ?, llm_summary = ?, llm_tags = ?, error = NULL WHERE id = ?",
    )
    .bind(STATUS_SENT)
    .bind(&now)
    .bind(&preview.title)
    .bind(&preview.preview)
    .bind(serde_json::to_string(&preview.tags).map_err(|e| ItemError::other(anyhow::anyhow!(e)))?)
    .bind(&item.id)
    .execute(pool)
    .await
    .map_err(|e| ItemError::other(anyhow::anyhow!(e)))?;
    Ok(())
}

async fn mark_failed(pool: &SqlitePool, id: &str, status: &str, error: &str) -> Result<()> {
    sqlx::query("UPDATE items SET status = ?, error = ? WHERE id = ?")
        .bind(status)
        .bind(error)
        .bind(id)
        .execute(pool)
        .await
        .with_context(|| format!("mark item {id} failed"))?;
    Ok(())
}

/// 组装交给 LLM 的文本：direct 条目读网页正文，其余用旧版模板拼原文。
async fn build_content(zhuazhua: &clients::zhuazhua::Client, item: &PendingItem) -> Result<String> {
    if item.is_direct {
        let request = clients::zhuazhua::types::FetchRequest {
            url: item.link.clone(),
        };
        let response = zhuazhua.fetch_url(&request).await.map_err(zhuazhua_error)?;
        let content = response.into_inner().content;
        if content.trim().is_empty() {
            anyhow::bail!("zhuazhua returned empty content for {}", item.link);
        }
        Ok(content)
    } else {
        let pub_date = if item.pub_date.is_empty() {
            &item.enqueued_at
        } else {
            &item.pub_date
        };
        Ok(format!(
            "【{}】{}\n发布于 {pub_date}\n\n{}",
            item.source, item.title, item.description_text
        ))
    }
}

fn zhuazhua_error(
    e: clients::zhuazhua::Error<clients::zhuazhua::types::ErrorResponse>,
) -> anyhow::Error {
    match e {
        clients::zhuazhua::Error::ErrorResponse(response) => {
            let err = response.into_inner();
            anyhow::anyhow!(
                "zhuazhua: {} ({})",
                err.error.message,
                err.error.code.unwrap_or_default()
            )
        }
        other => anyhow::anyhow!("zhuazhua request failed: {other}"),
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
    let pub_date = if item.pub_date.is_empty() {
        &item.enqueued_at
    } else {
        &item.pub_date
    };
    format!(
        "{}\n\n{}\n\n{}\n\n{} @{}\n\n{}",
        preview.title, preview.preview, tags, item.source, pub_date, item.link
    )
}

/// 渠道标签白名单匹配：白名单为空表示接收全部。
fn tags_match(channel_tags: &[String], source_tags: &str) -> bool {
    if channel_tags.is_empty() {
        return true;
    }
    let tags: std::collections::HashSet<&str> = source_tags
        .split('|')
        .filter(|tag| !tag.is_empty())
        .collect();
    channel_tags.iter().any(|tag| tags.contains(tag.as_str()))
}

async fn send_to_channel(channel: &Channel, content_json: &str) -> Result<()> {
    match channel {
        Channel::Tenant {
            app_id,
            app_secret,
            chat_id,
            base_url,
            ..
        } => {
            let mut builder = LarkClient::builder(app_id, app_secret).max_retries(0);
            if let Some(base) = base_url {
                builder = builder.base_url(base);
            }
            let client = builder.build().context("build lark client")?;
            let body = CreateMessageReqBody {
                receive_id: Some(chat_id.clone()),
                msg_type: Some("text".to_string()),
                content: Some(content_json.to_string()),
                uuid: None,
            };
            let response = client
                .im()
                .message
                .create("chat_id", &body, &RequestOption::default())
                .await
                .context("feishu im send")?;
            if !response.success() {
                anyhow::bail!("feishu im send rejected");
            }
            Ok(())
        }
        // 群机器人 webhook 是无鉴权的纯 POST；lark SDK 强制要求非空
        // app_id，发不了这种请求，这里直接用 reqwest 打完整 webhook 地址。
        Channel::Webhook { url, .. } => {
            let response = reqwest::Client::new()
                .post(url)
                .json(&serde_json::json!({ "msg_type": "text", "content": content_json }))
                .send()
                .await
                .context("feishu webhook send")?;
            if !response.status().is_success() {
                anyhow::bail!("feishu webhook http {}", response.status());
            }
            let body: serde_json::Value =
                response.json().await.context("parse webhook response")?;
            let code = body
                .get("code")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(-1);
            if code != 0 {
                anyhow::bail!(
                    "feishu webhook rejected: code {code}, msg {}",
                    body.get("msg")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                );
            }
            Ok(())
        }
    }
}
