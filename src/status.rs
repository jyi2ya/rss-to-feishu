//! `status` 子命令：统计各类条目的数量。
//!
//! 默认输出人类可读的 ASCII 表格（tabled），`--json` 输出机器可读的 JSON。

use anyhow::{Context, Result};
use serde::Serialize;
use tabled::settings::Style;

use crate::{
    config::Config,
    db::{self, STATUS_FAILED, STATUS_FAILED_LLM, STATUS_FAILED_SEND, STATUS_PENDING, STATUS_SENT},
};

#[derive(Debug, Serialize)]
pub struct Status {
    /// 同步总数（所有状态之和）。
    pub total: i64,
    /// 待投递（已同步、尚未处理）。
    pub pending: i64,
    /// 投递成功。
    pub sent: i64,
    /// LLM 总结失败。
    pub failed_llm: i64,
    /// 总结成功但投递失败。
    pub failed_send: i64,
    /// 其他失败（正文抓取失败、无匹配渠道等）。
    pub failed_other: i64,
}

pub async fn run(config: &Config, json: bool) -> Result<()> {
    let pool = db::connect(&config.database_path).await?;
    let counts = db::count_by_status(&pool).await?;
    let count = |status: &str| *counts.get(status).unwrap_or(&0);
    let status = Status {
        total: counts.values().sum(),
        pending: count(STATUS_PENDING),
        sent: count(STATUS_SENT),
        failed_llm: count(STATUS_FAILED_LLM),
        failed_send: count(STATUS_FAILED_SEND),
        failed_other: count(STATUS_FAILED),
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&status).context("encode status json")?
        );
    } else {
        println!("{}", render_table(&status));
    }
    Ok(())
}

fn render_table(status: &Status) -> String {
    let total = status.total.to_string();
    let pending = status.pending.to_string();
    let sent = status.sent.to_string();
    let failed_llm = status.failed_llm.to_string();
    let failed_send = status.failed_send.to_string();
    let failed_other = status.failed_other.to_string();
    let mut builder = tabled::builder::Builder::default();
    builder.push_record(["指标", "数量"]);
    builder.push_record(["同步总数", &total]);
    builder.push_record(["待投递", &pending]);
    builder.push_record(["投递成功", &sent]);
    builder.push_record(["LLM 总结失败", &failed_llm]);
    builder.push_record(["投递失败", &failed_send]);
    builder.push_record(["其他失败", &failed_other]);
    let mut table = builder.build();
    table.with(Style::ascii());
    table.to_string()
}
