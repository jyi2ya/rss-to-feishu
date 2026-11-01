//! `migrate` 子命令：把旧版 rss-feishu-bridge 的 db（CSV 表 + JSONL 事件日志）
//! 迁移进本项目的 sqlite，并打印旧订阅源对应的 `[feeds]` TOML 片段。

use std::{collections::BTreeMap, path::Path};

use anyhow::Result;
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use tracing::{info, warn};

use crate::{
    config::Config,
    db::{self, Item},
    legacy::{self, BoolOrInt, Event},
};

/// 迁移中间态：旧库各字段都允许缺失，最后统一兜底。
#[derive(Debug, Default)]
struct LegacyEntry {
    source: Option<String>,
    title: Option<String>,
    link: Option<String>,
    description_text: Option<String>,
    pub_date: Option<String>,
    fetch: Option<bool>,
    source_tags: Option<String>,
    enqueued_at: Option<String>,
    sent: bool,
    sent_at: Option<String>,
    llm_title: Option<String>,
    llm_summary: Option<String>,
    llm_tags: Vec<String>,
}

pub async fn run(config: &Config, old_dir: &Path) -> Result<()> {
    let pool = db::connect(&config.database_path).await;
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

    let events = legacy::read_jsonl(old_dir)?;
    // 旧文件里 update 可能写在 add 之前：先过一遍 add 建立条目，
    // 再过一遍 send/update 回填状态与摘要。
    for event in &events {
        if let Event::Add { data, time } = event {
            apply_jsonl_add(&mut entries, data, *time);
        }
    }
    for event in &events {
        match event {
            Event::Add { .. } => {}
            Event::Send { data, .. } => {
                if let Some(entry) = entries.get_mut(data) {
                    entry.sent = true;
                }
            }
            Event::Update { data, .. } => apply_jsonl_update(&mut entries, data),
            Event::Unknown => warn!("db.jsonl: unknown event type, skip"),
        }
    }

    let now = Utc::now();
    let mut items = Vec::new();
    for (id, entry) in &entries {
        let Some(item) = entry.finalize(id, now) else {
            warn!("entry {id}: unusable, skip");
            continue;
        };
        items.push(item);
    }
    db::insert_items(&pool, &items).await;
    print_feeds_snippet(old_dir)?;

    info!("migrate done: {} entries loaded", entries.len());
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
        entry.fetch = Some(raw.trim() == "1" || raw.trim().eq_ignore_ascii_case("true"));
    }
    fill(&mut entry.enqueued_at, row.get("enqueued_at"));
    if is_sent {
        entry.sent = true;
        fill(&mut entry.sent_at, row.get("sent_at"));
        fill(&mut entry.llm_title, row.get("llm_title"));
        fill(&mut entry.llm_summary, row.get("llm_summary"));
        if let Some(raw) = row.get("llm_tags") {
            entry.llm_tags = parse_tags_json(raw).unwrap_or_default();
        }
    }
}

fn apply_jsonl_add(
    entries: &mut BTreeMap<String, LegacyEntry>,
    data: &legacy::AddData,
    time: Option<f64>,
) {
    let Some(id) = data.id.as_deref().filter(|id| !id.is_empty()) else {
        warn!("db.jsonl: add event without id, skip");
        return;
    };
    let entry = entries.entry(id.to_string()).or_default();
    fill(&mut entry.source, data.source.as_ref());
    fill(&mut entry.title, data.title.as_ref());
    fill(&mut entry.link, data.link.as_ref());
    // 旧 add 里 description 与 description_text 并存，任取其一解压。
    if let Some(raw) = data
        .description
        .as_deref()
        .or(data.description_text.as_deref())
    {
        entry.description_text = Some(legacy::decode_legacy_gz(raw));
    }
    fill(&mut entry.pub_date, data.pub_date.as_ref());
    entry.fetch = match &data.is_direct {
        Some(BoolOrInt::Bool(value)) => Some(*value),
        Some(BoolOrInt::Int(value)) => Some(*value == 1),
        Some(BoolOrInt::Text(value)) => {
            Some(value.trim() == "1" || value.trim().eq_ignore_ascii_case("true"))
        }
        None => None,
    };
    fill(&mut entry.source_tags, data.source_tags.as_ref());
    // 早期版本的 add 事件把摘要与发送状态直接嵌在 data 里（没有单独的 update/send）。
    fill(&mut entry.enqueued_at, data.enqueued_at.as_ref());
    if entry.enqueued_at.is_none() {
        entry.enqueued_at = time.map(epoch_to_rfc3339);
    }
    fill(&mut entry.llm_title, data.llm_title.as_ref());
    fill(&mut entry.llm_summary, data.llm_summary.as_ref());
    if let Some(tags) = &data.llm_tags {
        entry.llm_tags = match tags {
            legacy::TagsText::List(tags) => {
                tags.iter().filter(|tag| !tag.is_empty()).cloned().collect()
            }
            legacy::TagsText::Text(raw) => raw
                .split('|')
                .filter(|tag| !tag.is_empty())
                .map(str::to_string)
                .collect(),
        };
    }
    if let Some(raw) = data.sent_at.as_deref().filter(|s| !s.trim().is_empty()) {
        entry.sent = true;
        entry.sent_at = Some(raw.to_string());
    }
}

fn apply_jsonl_update(entries: &mut BTreeMap<String, LegacyEntry>, data: &legacy::UpdateData) {
    let Some(entry) = entries.get_mut(&data.id) else {
        warn!("db.jsonl: update event for unknown id {}, skip", data.id);
        return;
    };
    fill(&mut entry.llm_title, data.llm_title.as_ref());
    fill(&mut entry.llm_summary, data.llm_summary.as_ref());
    if let Some(tags) = &data.llm_tags {
        entry.llm_tags = match tags {
            legacy::TagsText::List(tags) => {
                tags.iter().filter(|tag| !tag.is_empty()).cloned().collect()
            }
            legacy::TagsText::Text(raw) => raw
                .split('|')
                .filter(|tag| !tag.is_empty())
                .map(str::to_string)
                .collect(),
        };
    }
}

impl LegacyEntry {
    /// 兜底缺失字段并组装成正式 Item；缺 title/link 的条目无法推送，返回 None。
    fn finalize(&self, id: &str, now: DateTime<Utc>) -> Option<Item> {
        let source = self.source.clone().unwrap_or_default();
        let title = self.title.clone().unwrap_or_default();
        let link = self.link.clone().filter(|link| !link.is_empty());
        let enqueued_at = self
            .enqueued_at
            .as_deref()
            .and_then(parse_legacy_datetime)
            .unwrap_or(now);
        let pub_date = db::sane_pub_date(self.pub_date.as_deref().and_then(parse_legacy_datetime));
        let sent_at = if self.sent {
            Some(
                self.sent_at
                    .as_deref()
                    .and_then(parse_legacy_datetime)
                    .unwrap_or(enqueued_at),
            )
        } else {
            None
        };
        let sort_key = db::compute_sort_key(id, pub_date, enqueued_at);
        Some(Item {
            id: id.to_string(),
            source,
            title,
            link,
            description_text: self.description_text.clone().unwrap_or_default(),
            pub_date,
            fetch: self.fetch.unwrap_or(false),
            source_tags: sqlx::types::Json(
                self.source_tags
                    .as_deref()
                    .map(|tags| {
                        tags.split('|')
                            .filter(|tag| !tag.is_empty())
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
            ),
            enqueued_at,
            sort_key,
            status: if self.sent {
                db::Status::Sent
            } else {
                db::Status::Pending
            },
            llm_title: self.llm_title.clone(),
            llm_summary: self.llm_summary.clone(),
            llm_tags: (!self.llm_tags.is_empty()).then(|| sqlx::types::Json(self.llm_tags.clone())),
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
    for row in rows {
        let Some(raw_url) = row.get("url").filter(|url| !url.is_empty()) else {
            continue;
        };
        // 旧 link 表的 url 可能带 direct+ 前缀，拆成结构化字段。
        let (url, direct) = match raw_url.strip_prefix("direct+") {
            Some(rest) => (rest, true),
            None => (raw_url.as_str(), false),
        };
        println!("[[feeds]]");
        println!("url = \"{}\"", url.replace('"', "\\\""));
        let tags: Vec<&str> = row
            .get("tags")
            .map(|tags| tags.split('|').filter(|tag| !tag.is_empty()).collect())
            .unwrap_or_default();
        let tags = tags
            .iter()
            .map(|tag| format!("\"{}\"", tag.replace('"', "\\\"")))
            .collect::<Vec<_>>()
            .join(", ");
        println!("tags = [{}]", tags);
        if direct {
            println!("fetch = true");
        }
    }
    Ok(())
}

fn fill(target: &mut Option<String>, value: Option<&String>) {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        *target = Some(value.clone());
    }
}

/// 旧 sent 表的 llm_tags 可能混入 perl 序列化垃圾，只接受 JSON 字符串数组。
fn parse_tags_json(raw: &str) -> Option<Vec<String>> {
    serde_json::from_str(raw).ok()
}

/// jsonl 的 time 是带小数的 epoch 秒，只取整数秒。
fn epoch_to_rfc3339(epoch: f64) -> String {
    // 先截断再经字符串转 i64：丢弃小数秒正是想要的取整行为，
    // 绕开 float→int 直接转换的精度告警。
    let seconds: i64 = epoch.trunc().to_string().parse().unwrap_or(0);
    DateTime::from_timestamp(seconds, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| Utc::now().to_rfc3339())
}

/// 旧库时间串宽松解析：RFC3339 或常见 `YYYY-MM-DD[ HH:MM:SS]`。
fn parse_legacy_datetime(raw: &str) -> Option<DateTime<Utc>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Some(dt.with_timezone(&Utc));
    }
    for format in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(raw, format) {
            return Some(Utc.from_utc_datetime(&naive));
        }
    }
    None
}
