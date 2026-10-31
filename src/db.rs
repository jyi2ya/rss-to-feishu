//! sqlite 存取：连接池、迁移、条目表读写与排序键计算。

use std::{path::Path, time::Duration};

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

/// 条目在库中的完整记录（插入/迁移用，不派生 Clone）。
#[derive(Debug)]
pub struct Item {
    pub id: String,
    pub source: String,
    pub title: String,
    pub link: String,
    pub description_text: String,
    pub pub_date: String,
    pub is_direct: bool,
    pub source_tags: String,
    pub enqueued_at: String,
    pub sort_key: i64,
    pub status: String,
    pub llm_title: Option<String>,
    pub llm_summary: Option<String>,
    pub llm_tags: Option<String>,
    pub sent_at: Option<String>,
    pub error: Option<String>,
}

/// push 需要读取的待推送条目字段（pending 条目没有摘要字段）。
#[derive(Debug, sqlx::FromRow)]
pub struct PendingItem {
    pub id: String,
    pub source: String,
    pub title: String,
    pub link: String,
    pub description_text: String,
    pub pub_date: String,
    pub is_direct: bool,
    pub source_tags: String,
    pub enqueued_at: String,
}

pub const STATUS_PENDING: &str = "pending";
pub const STATUS_SENT: &str = "sent";
pub const STATUS_FAILED: &str = "failed";
/// LLM 结构化提取失败。
pub const STATUS_FAILED_LLM: &str = "failed_llm";
/// 摘要成功但渠道投递失败。
pub const STATUS_FAILED_SEND: &str = "failed_send";

pub async fn connect(path: &Path) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(30));
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .with_context(|| format!("open sqlite {}", path.display()))?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("run migrations")?;
    Ok(pool)
}

/// 排序键：发布时间（不可解析时用入队时间）加一天内均匀抖动，
/// 沿用旧版 push.pl 的算法（id 前 8 位 hex mod 86400）。
pub fn compute_sort_key(id: &str, pub_date: &str, enqueued_at: &str) -> i64 {
    let epoch = parse_datetime(pub_date)
        .or_else(|| parse_datetime(enqueued_at))
        .map(|dt| dt.timestamp())
        .unwrap_or_else(|| Utc::now().timestamp());
    let jitter = u64::from_str_radix(id.get(..8).unwrap_or(""), 16).unwrap_or(0) % 86_400;
    epoch + jitter as i64
}

/// 宽松解析历史/外部时间串：RFC3339 或常见 `YYYY-MM-DD[ HH:MM:SS]`。
fn parse_datetime(input: &str) -> Option<DateTime<Utc>> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(input) {
        return Some(dt.with_timezone(&Utc));
    }
    for format in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(input, format) {
            return Some(Utc.from_utc_datetime(&naive));
        }
    }
    None
}

pub async fn insert_items(pool: &SqlitePool, items: &[Item]) -> Result<usize> {
    let mut tx = pool.begin().await.context("begin transaction")?;
    let mut inserted = 0usize;
    for item in items {
        let result = sqlx::query(
            r#"INSERT OR IGNORE INTO items
               (id, source, title, link, description_text, pub_date, is_direct,
                source_tags, enqueued_at, sort_key, status,
                llm_title, llm_summary, llm_tags, sent_at, error)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&item.id)
        .bind(&item.source)
        .bind(&item.title)
        .bind(&item.link)
        .bind(&item.description_text)
        .bind(&item.pub_date)
        .bind(item.is_direct)
        .bind(&item.source_tags)
        .bind(&item.enqueued_at)
        .bind(item.sort_key)
        .bind(&item.status)
        .bind(&item.llm_title)
        .bind(&item.llm_summary)
        .bind(&item.llm_tags)
        .bind(&item.sent_at)
        .bind(&item.error)
        .execute(&mut *tx)
        .await
        .with_context(|| format!("insert item {}", item.id))?;
        inserted += result.rows_affected() as usize;
    }
    tx.commit().await.context("commit transaction")?;
    Ok(inserted)
}

/// 全部已收录条目的 id 集合，sync 用它去重。
pub async fn all_ids(pool: &SqlitePool) -> Result<std::collections::HashSet<String>> {
    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM items")
        .fetch_all(pool)
        .await
        .context("load existing ids")?;
    Ok(ids.into_iter().collect())
}

/// 各状态的条目计数，供 status 子命令使用。
pub async fn count_by_status(pool: &SqlitePool) -> Result<std::collections::BTreeMap<String, i64>> {
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT status, COUNT(*) FROM items GROUP BY status")
            .fetch_all(pool)
            .await
            .context("count items by status")?;
    Ok(rows.into_iter().collect())
}
