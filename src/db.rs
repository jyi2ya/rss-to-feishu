//! sqlite 存取：连接池、迁移、条目表读写与排序键计算。
//!
//! 数据库是本工具的根基：所有 db 错误直接 panic（expect），
//! 坏了没有可恢复路径，不如立刻死掉。

use std::{path::Path, time::Duration};

use chrono::{DateTime, Utc};
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

/// 条目状态：variant 名（snake_case）即库里的 TEXT 值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, strum::AsRefStr)]
#[sqlx(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum Status {
    Pending,
    Sent,
    FailedFetch,
    FailedLlm,
    FailedSend,
    FailedOther,
}

/// 条目在库中的完整记录（插入/迁移用，不派生 Clone）。
#[derive(Debug)]
pub struct Item {
    pub id: String,
    pub source: String,
    pub title: String,
    /// 原文链接；源里没给时为 None，显示时兜底 `[NO LINK]`。
    pub link: Option<String>,
    pub description_text: String,
    /// 发布时间；源里没给或解析失败时为 None，展示/排序时兜底入队时间。
    pub pub_date: Option<DateTime<Utc>>,
    pub fetch: bool,
    pub source_tags: sqlx::types::Json<Vec<String>>,
    pub enqueued_at: DateTime<Utc>,
    pub sort_key: i64,
    pub status: Status,
    pub llm_title: Option<String>,
    pub llm_summary: Option<String>,
    pub llm_tags: Option<sqlx::types::Json<Vec<String>>>,
    pub sent_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

/// push 需要读取的待推送条目字段（pending 条目没有摘要字段）。
#[derive(Debug, sqlx::FromRow)]
pub struct PendingItem {
    pub id: String,
    pub source: String,
    pub title: String,
    pub link: Option<String>,
    pub description_text: String,
    pub pub_date: Option<DateTime<Utc>>,
    pub fetch: bool,
    pub source_tags: sqlx::types::Json<Vec<String>>,
    pub enqueued_at: DateTime<Utc>,
}

pub async fn connect(path: &Path) -> SqlitePool {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(30));
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .expect("数据库无法打开，无法继续");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("数据库迁移失败，无法继续");
    pool
}

/// 排序键：发布时间（不可解析时用入队时间）加一天内均匀抖动，
/// 沿用旧版 push.pl 的算法（id 前 8 位 hex mod 86400）。
/// 排序键：发布时间（缺省时用入队时间）加一天内均匀抖动，
/// 沿用旧版 push.pl 的算法（id 前 8 位 hex mod 86400）。
pub fn compute_sort_key(
    id: &str,
    pub_date: Option<DateTime<Utc>>,
    enqueued_at: DateTime<Utc>,
) -> i64 {
    let epoch = pub_date.unwrap_or(enqueued_at).timestamp();
    // 8 位 hex 最大 0xFFFFFFFF，i64 放得下，避免 u64 -> i64 的 wrap cast。
    let jitter = i64::from_str_radix(id.get(..8).unwrap_or(""), 16).unwrap_or(0) % 86_400;
    epoch + jitter
}

/// 过滤明显异常的发布时间：feed 里偶有 `0001-01-01` 之类的垃圾值，
/// 1970 年以前一律视为没有发布时间（展示/排序时兜底入队时间）。
pub fn sane_pub_date(pub_date: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    pub_date.filter(|date| date.timestamp() > 0)
}

/// 每条多行语句最多绑定的条目数（16 列 × 500 行 = 8000 个参数，
/// 低于 SQLite 的参数上限）。
const BULK_CHUNK: usize = 500;

/// 批量入库（单事务，供 sync 与 migrate 共用）：
/// 多行 INSERT ... ON CONFLICT(id) DO UPDATE，新条目插入、
/// 已有条目刷新内容字段（站点改版换 URL 等情况），
/// 状态/摘要/发送时间等生命周期字段保持不变。
pub async fn insert_items(pool: &SqlitePool, items: &[Item]) {
    if items.is_empty() {
        return;
    }
    let mut tx = pool.begin().await.expect("开启事务失败，无法继续");
    for chunk in items.chunks(BULK_CHUNK) {
        upsert_chunk(&mut *tx, chunk).await;
    }
    tx.commit().await.expect("提交事务失败，无法继续");
}

/// 多行 INSERT ... ON CONFLICT(id) DO UPDATE：
/// 内容字段以本次新值为准，生命周期字段保持库中原值。
async fn upsert_chunk<'e, E>(executor: E, items: &[Item])
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let mut builder = sqlx::QueryBuilder::new(
        "INSERT INTO items (id, source, title, link, description_text, pub_date, \
         fetch, source_tags, enqueued_at, sort_key, status, llm_title, llm_summary, \
         llm_tags, sent_at, error) ",
    );
    let _rows = builder
        .push_values(items, |mut b, item| {
            let _b = b
                .push_bind(&item.id)
                .push_bind(&item.source)
                .push_bind(&item.title)
                .push_bind(&item.link)
                .push_bind(&item.description_text)
                .push_bind(item.pub_date)
                .push_bind(item.fetch)
                .push_bind(&item.source_tags)
                .push_bind(item.enqueued_at)
                .push_bind(item.sort_key)
                .push_bind(item.status)
                .push_bind(&item.llm_title)
                .push_bind(&item.llm_summary)
                .push_bind(&item.llm_tags)
                .push_bind(item.sent_at)
                .push_bind(&item.error);
        })
        .push(
            " ON CONFLICT(id) DO UPDATE SET source = excluded.source, title = excluded.title, \
             link = excluded.link, description_text = excluded.description_text, \
             pub_date = excluded.pub_date, fetch = excluded.fetch, \
             source_tags = excluded.source_tags, sort_key = excluded.sort_key",
        )
        .build()
        .execute(executor)
        .await
        .expect("批量写入条目失败，无法继续");
}

/// 各状态的条目计数，供 status 子命令使用。
pub async fn count_by_status(pool: &SqlitePool) -> std::collections::BTreeMap<String, i64> {
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT status, COUNT(*) FROM items GROUP BY status")
            .fetch_all(pool)
            .await
            .expect("统计条目失败，无法继续");
    rows.into_iter().collect()
}

/// 取前 limit 条待推送条目（按排序键、入队时间）。
pub async fn pending_items(pool: &SqlitePool, limit: u32) -> Vec<PendingItem> {
    sqlx::query_as::<_, PendingItem>(
        "SELECT * FROM items WHERE status = ? ORDER BY sort_key, enqueued_at LIMIT ?",
    )
    .bind(Status::Pending.as_ref())
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await
    .expect("加载待推送条目失败，无法继续")
}

/// 标记条目已发送：写入状态、发送时间与 LLM 摘要。
pub async fn mark_sent(
    pool: &SqlitePool,
    item_id: &str,
    sent_at: DateTime<Utc>,
    llm_title: &str,
    llm_summary: &str,
    llm_tags_json: &str,
) {
    let _rows = sqlx::query(
        "UPDATE items SET status = ?, sent_at = ?, llm_title = ?, llm_summary = ?, llm_tags = ?, error = NULL WHERE id = ?",
    )
    .bind(Status::Sent.as_ref())
    .bind(sent_at)
    .bind(llm_title)
    .bind(llm_summary)
    .bind(llm_tags_json)
    .bind(item_id)
    .execute(pool)
    .await
    .expect("标记条目已发送失败，无法继续");
}

/// 标记条目失败：写入状态与原因。
pub async fn mark_failed(pool: &SqlitePool, item_id: &str, status: &str, error: &str) {
    let _rows = sqlx::query("UPDATE items SET status = ?, error = ? WHERE id = ?")
        .bind(status)
        .bind(error)
        .bind(item_id)
        .execute(pool)
        .await
        .expect("标记条目失败状态失败，无法继续");
}
