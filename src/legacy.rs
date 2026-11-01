//! 旧版 rss-feishu-bridge 存储格式解析：DBI::CSV 表 + JSONL 事件日志 + `GZ:` 压缩字段。
//! 供 `migrate` 子命令读取，不参与日常读写。

use std::{collections::BTreeMap, io::Read, path::Path};

use anyhow::{Context, Result};
use base64::Engine;
use flate2::read::GzDecoder;

/// CSV 表里的一行，列名 -> 原始字符串。
pub type CsvRow = BTreeMap<String, String>;

/// 读取 `db/<table>.csv`，不存在的表返回空列表。
pub fn read_csv_table(old_dir: &Path, table: &str) -> Result<Vec<CsvRow>> {
    let path = old_dir.join("db").join(format!("{table}.csv"));
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut reader =
        csv::Reader::from_path(&path).with_context(|| format!("open {}", path.display()))?;
    let headers = reader
        .headers()
        .with_context(|| format!("read header of {}", path.display()))?
        .clone();
    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record.with_context(|| format!("read record of {}", path.display()))?;
        let mut row = CsvRow::new();
        for (header, value) in headers.iter().zip(record.iter()) {
            let _previous = row.insert(header.to_string(), value.to_string());
        }
        rows.push(row);
    }
    Ok(rows)
}

/// 一条 `db.jsonl` 事件：`type` 字段作 tag，`data`/`time` 随类型变化。
#[derive(Debug, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// 新条目入队（add_old 同形）。
    #[serde(alias = "add_old")]
    Add {
        data: AddData,
        #[serde(default)]
        time: Option<f64>,
    },
    /// 已发送，data 是条目 id。
    Send { data: String },
    /// LLM 结果回填。
    Update { data: UpdateData },
    /// 不认识的事件类型：跳过，不让整次迁移失败。
    #[serde(other)]
    Unknown,
}

/// add/add_old 事件的 data：旧库与正式 Item 字段名不同
/// （description 与 description_text 并存，is_direct 是整数）。
#[derive(Debug, serde::Deserialize)]
pub struct AddData {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub link: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub description_text: Option<String>,
    #[serde(default)]
    pub pub_date: Option<String>,
    #[serde(default)]
    pub is_direct: Option<BoolOrInt>,
    #[serde(default)]
    pub source_tags: Option<String>,
}

/// update 事件的 data。
#[derive(Debug, serde::Deserialize)]
pub struct UpdateData {
    pub id: String,
    #[serde(default)]
    pub llm_title: Option<String>,
    #[serde(default)]
    pub llm_summary: Option<String>,
    #[serde(default)]
    pub llm_tags: Option<Vec<String>>,
}

/// 旧库的 is_direct 有布尔和整数两种写法。
#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
pub enum BoolOrInt {
    Bool(bool),
    Int(i64),
}

/// 读取 `db.jsonl` 事件日志。
pub fn read_jsonl(old_dir: &Path) -> Result<Vec<Event>> {
    let path = old_dir.join("db.jsonl");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let mut events = Vec::new();
    for (line_no, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event: Event = serde_json::from_str(line)
            .with_context(|| format!("parse {}:{}", path.display(), line_no + 1))?;
        events.push(event);
    }
    Ok(events)
}

/// 解包旧版 `GZ:` 前缀的 gzip+base64 字段；无前缀或解不开时原样返回。
pub fn decode_legacy_gz(input: &str) -> String {
    let mut current = input.to_string();
    for _ in 0..8 {
        let Some(rest) = current.strip_prefix("GZ:") else {
            break;
        };
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(rest) else {
            break;
        };
        let mut decoder = GzDecoder::new(&bytes[..]);
        let mut text = String::new();
        if decoder.read_to_string(&mut text).is_err() {
            break;
        }
        current = text;
    }
    current
}
