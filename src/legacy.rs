//! 旧版 rss-feishu-bridge 存储格式解析：DBI::CSV 表 + JSONL 事件日志 + `GZ:` 压缩字段。
//! 供 `migrate` 子命令读取，不参与日常读写。

use std::{collections::BTreeMap, io::Read, path::Path};

use anyhow::{Context, Result};
use base64::Engine;
use flate2::read::GzDecoder;
use serde_json::Value;

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
            row.insert(header.to_string(), value.to_string());
        }
        rows.push(row);
    }
    Ok(rows)
}

/// 读取 `db.jsonl` 事件日志：`(type, data, time)`，time 为 epoch 秒。
pub fn read_jsonl(old_dir: &Path) -> Result<Vec<(String, Value, Option<i64>)>> {
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
        let value: Value = serde_json::from_str(line)
            .with_context(|| format!("parse {}:{}", path.display(), line_no + 1))?;
        events.push((
            value
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            value.get("data").cloned().unwrap_or(Value::Null),
            value.get("time").and_then(Value::as_i64),
        ));
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
