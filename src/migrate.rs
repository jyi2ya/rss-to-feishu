//! `migrate` 子命令：把旧版 rss-feishu-bridge 的 db（CSV 表 + JSONL 事件日志）
//! 迁移进本项目的 sqlite，并打印旧订阅源对应的 `[feeds]` TOML 片段。

use std::{collections::BTreeMap, path::Path};

use anyhow::Result;
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use serde_json::Value;
use tracing::{info, warn};

use crate::{
    config::Config,
    db::{self, Item, STATUS_PENDING, STATUS_SENT},
    legacy,
};

/// 迁移中间态：旧库各字段都允许缺失，最后统一兜底。
#[derive(Debug, Default)]
struct LegacyEntry {
    source: Option<String>,
    title: Option<String>,
    link: Option<String>,
    description_text: Option<String>,
    pub_date: Option<String>,
    is_direct: Option<bool>,
    source_tags: Option<String>,
    enqueued_at: Option<String>,
    sent: bool,
    sent_at: Option<String>,
    llm_title: Option<String>,
    llm_summary: Option<String>,
    llm_tags: Option<String>,
}

pub async fn run(config: &Config, old_dir: &Path) -> Result<()> {
    let pool = db::connect(&config.database_path).await?;
    let mut entries: BTreeMap<String, LegacyEntry> = BTreeMap::new();

    for (table, is_sent) in [("old_queue", false), ("queue", false), ("sent", true)] {
        for row in legacy::read_csv_table(old_dir, table)? {
            let Some(id) = row.get("id").filter(|id| !id.is_empty()) else {
                warn!("{table}.csv: row without id, skip");
                continue;
            };
            apply_csv_row(entries.entry(id.clone()).or_default(), &row, is_sent);
        }
    }

    for (event_type, data, time) in legacy::read_jsonl(old_dir)? {
        match event_type.as_str() {
            "add" | "add_old" => apply_jsonl_add(&mut entries, &data, time),
            "send" => {
                if let Some(entry) = data.as_str().and_then(|id| entries.get_mut(id)) {
                    entry.sent = true;
                }
            }
            "update" => apply_jsonl_update(&mut entries, &data),
            other => warn!("db.jsonl: unknown event type {other:?}, skip"),
        }
    }

    let now = Utc::now().to_rfc3339();
    let mut items = Vec::new();
    for (id, entry) in &entries {
        let Some(item) = entry.finalize(id, &now) else {
            warn!("entry {id}: unusable, skip");
            continue;
        };
        items.push(item);
    }
    let inserted = db::insert_items(&pool, &items).await?;
    print_feeds_snippet(old_dir)?;

    info!(
        "migrate done: {} entries loaded, {} inserted",
        entries.len(),
        inserted
    );
    Ok(())
}

fn apply_csv_row(entry: &mut LegacyEntry, row: &legacy::CsvRow, is_sent: bool) {
    fill(&mut entry.source, row.get("source"));
    fill(&mut entry.title, row.get("title"));
    fill(&mut entry.link, row.get("link"));
    if let Some(raw) = row.get("description_text") {
        entry.description_text = Some(legacy::decode_legacy_gz(raw));
    }
    fill(&mut entry.pub_date, row.get("pub_date"));
    if let Some(raw) = row.get("is_direct") {
        entry.is_direct = Some(raw.trim() == "1" || raw.trim().eq_ignore_ascii_case("true"));
    }
    fill(&mut entry.enqueued_at, row.get("enqueued_at"));
    if is_sent {
        entry.sent = true;
        fill(&mut entry.sent_at, row.get("sent_at"));
        fill(&mut entry.llm_title, row.get("llm_title"));
        fill(&mut entry.llm_summary, row.get("llm_summary"));
        if let Some(raw) = row.get("llm_tags") {
            entry.llm_tags = parse_tags_json(raw);
        }
    }
}

fn apply_jsonl_add(entries: &mut BTreeMap<String, LegacyEntry>, data: &Value, time: Option<i64>) {
    let Some(id) = data.get("id").and_then(Value::as_str) else {
        warn!("db.jsonl: add event without id, skip");
        return;
    };
    let entry = entries.entry(id.to_string()).or_default();
    fill_json(&mut entry.source, data.get("source"));
    fill_json(&mut entry.title, data.get("title"));
    fill_json(&mut entry.link, data.get("link"));
    if let Some(raw) = data.get("description_text").and_then(Value::as_str) {
        entry.description_text = Some(legacy::decode_legacy_gz(raw));
    }
    fill_json(&mut entry.pub_date, data.get("pub_date"));
    match data.get("is_direct") {
        Some(Value::Bool(v)) => entry.is_direct = Some(*v),
        Some(Value::Number(v)) => entry.is_direct = Some(v.as_i64() == Some(1)),
        _ => {}
    }
    fill_json(&mut entry.source_tags, data.get("source_tags"));
    if entry.enqueued_at.is_none() {
        entry.enqueued_at = time.map(epoch_to_rfc3339);
    }
}

fn apply_jsonl_update(entries: &mut BTreeMap<String, LegacyEntry>, data: &Value) {
    let Some(id) = data.get("id").and_then(Value::as_str) else {
        warn!("db.jsonl: update event without id, skip");
        return;
    };
    let Some(entry) = entries.get_mut(id) else {
        warn!("db.jsonl: update event for unknown id {id}, skip");
        return;
    };
    fill_json(&mut entry.llm_title, data.get("llm_title"));
    fill_json(&mut entry.llm_summary, data.get("llm_summary"));
    if let Some(tags) = data.get("llm_tags").and_then(Value::as_array) {
        let tags: Vec<String> = tags
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        if !tags.is_empty()
            && let Ok(encoded) = serde_json::to_string(&tags)
        {
            entry.llm_tags = Some(encoded);
        }
    }
}

impl LegacyEntry {
    /// 兜底缺失字段并组装成正式 Item；缺 title/link 的条目无法推送，返回 None。
    fn finalize(&self, id: &str, now: &str) -> Option<Item> {
        let source = self.source.clone().unwrap_or_default();
        let title = self.title.clone().unwrap_or_default();
        let link = self
            .link
            .clone()
            .filter(|link| !link.is_empty())
            .unwrap_or_else(|| "[NO LINK]".to_string());
        let enqueued_at = self
            .enqueued_at
            .clone()
            .map(|raw| normalize_datetime(&raw))
            .unwrap_or_else(|| now.to_string());
        let pub_date = match self.pub_date.clone().filter(|date| !date.trim().is_empty()) {
            Some(date) => date,
            None => enqueued_at.clone(),
        };
        let sent_at = if self.sent {
            Some(
                self.sent_at
                    .clone()
                    .map(|raw| normalize_datetime(&raw))
                    .unwrap_or_else(|| enqueued_at.clone()),
            )
        } else {
            None
        };
        let sort_key = db::compute_sort_key(id, &pub_date, &enqueued_at);
        Some(Item {
            id: id.to_string(),
            source,
            title,
            link,
            description_text: self.description_text.clone().unwrap_or_default(),
            pub_date,
            is_direct: self.is_direct.unwrap_or(false),
            source_tags: self.source_tags.clone().unwrap_or_default(),
            enqueued_at,
            sort_key,
            status: if self.sent {
                STATUS_SENT.to_string()
            } else {
                STATUS_PENDING.to_string()
            },
            llm_title: self.llm_title.clone(),
            llm_summary: self.llm_summary.clone(),
            llm_tags: self.llm_tags.clone(),
            sent_at,
            error: None,
        })
    }
}

/// 读取旧 link 表并打印可直接粘进 config.toml 的 [feeds] 片段。
fn print_feeds_snippet(old_dir: &Path) -> Result<()> {
    let rows = legacy::read_csv_table(old_dir, "link")?;
    if rows.is_empty() {
        return Ok(());
    }
    println!("# feeds copied from the old link table, paste into config.toml:");
    println!("[feeds]");
    for row in rows {
        let Some(url) = row.get("url").filter(|url| !url.is_empty()) else {
            continue;
        };
        let tags: Vec<&str> = row
            .get("tags")
            .map(|tags| tags.split('|').filter(|tag| !tag.is_empty()).collect())
            .unwrap_or_default();
        let tags = tags
            .iter()
            .map(|tag| format!("\"{}\"", tag.replace('"', "\\\"")))
            .collect::<Vec<_>>()
            .join(", ");
        println!("\"{}\" = [{}]", url.replace('"', "\\\""), tags);
    }
    Ok(())
}

fn fill(target: &mut Option<String>, value: Option<&String>) {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        *target = Some(value.clone());
    }
}

fn fill_json(target: &mut Option<String>, value: Option<&Value>) {
    if let Some(value) = value
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
    {
        *target = Some(value.to_string());
    }
}

/// 旧 sent 表的 llm_tags 可能混入 perl 序列化垃圾，只接受 JSON 字符串数组。
fn parse_tags_json(raw: &str) -> Option<String> {
    let tags: Vec<String> = serde_json::from_str(raw).ok()?;
    serde_json::to_string(&tags).ok()
}

fn epoch_to_rfc3339(epoch: i64) -> String {
    DateTime::from_timestamp(epoch, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| Utc::now().to_rfc3339())
}

/// 旧库时间串统一成 RFC3339（UTC）。
fn normalize_datetime(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return Utc::now().to_rfc3339();
    }
    if DateTime::parse_from_rfc3339(raw).is_ok() {
        return raw.to_string();
    }
    if let Ok(naive) = NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S") {
        return Utc.from_utc_datetime(&naive).to_rfc3339();
    }
    raw.to_string()
}
