//! rss-to-feishu：抓取 RSS/Atom 订阅，LLM 摘要后推送到飞书群。
//!
//! 子命令：
//! - `sync`：抓取 config.toml 里的订阅源，新条目入队；
//! - `push`：摘要并推送 pending 条目（缺省 1 条，`--limit N` 限量）；
//! - `migrate`：把旧版 rss-feishu-bridge 的 db 迁进 sqlite；
//! - `status`：查看条目状态统计（ASCII 表格或 JSON）。

mod channel;
mod clients;
mod config;
mod db;
mod legacy;
mod llm;
mod migrate;
mod push;
mod status;
mod sync;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// 配置文件路径
    #[arg(
        long,
        global = true,
        default_value = "config.toml",
        value_name = "PATH"
    )]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 抓取配置中的订阅源，将新条目入队
    Sync,
    /// 摘要并推送队列中的条目
    Push {
        /// 最多处理多少条
        #[arg(long, default_value_t = 1)]
        limit: u32,
    },
    /// 将旧版 rss-feishu-bridge 的 db 迁移到 sqlite
    Migrate {
        /// 旧项目目录（内含 db/*.csv 与 db.jsonl）
        #[arg(long)]
        from: PathBuf,
    },
    /// 查看当前条目状态统计
    Status {
        /// 输出 JSON（供机器读取），缺省输出 ASCII 表格
        #[arg(long)]
        json: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let config = config::Config::load(&cli.config)?;
    match cli.command {
        Command::Sync => sync::run(&config).await?,
        Command::Push { limit } => push::run(&config, limit).await?,
        Command::Migrate { from } => migrate::run(&config, &from).await?,
        Command::Status { json } => status::run(&config, json).await?,
    }
    Ok(())
}
