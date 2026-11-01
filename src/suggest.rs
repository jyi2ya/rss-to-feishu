//! `suggest` 子命令：拉取一个 feed，取最新若干条目的正文文本，
//! 让 LLM 判断这些条目是否是完整文章，给出是否需要开启 `fetch = true` 的建议。
//! 抓取、条目正文回退与排序都复用 sync 的现成逻辑。

use anyhow::{Context, Result};

use crate::{config::Config, llm::FeedAdvisor, sync};

pub async fn run(config: &Config, url: &str, count: usize) -> Result<()> {
    let url = url::Url::parse(url).with_context(|| format!("parse feed url {url}"))?;
    let client = sync::http_client(config)?;
    let mut items = sync::fetch_feed(&client, &url, false, &[]).await?;
    // 与 push 相同的次序：sort_key 升序，取最新的前 count 条。
    items.sort_by_key(|item| item.sort_key);
    items.truncate(count);
    if items.is_empty() {
        anyhow::bail!("feed 里没有可判断的条目（条目正文都为空？）");
    }

    println!("抽样 {} 条条目：", items.len());
    for item in &items {
        println!("- {}", item.title);
    }
    println!();

    let text = items
        .iter()
        .map(|item| {
            format!(
                "【{}】{}\n{}",
                item.source, item.title, item.description_text
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");
    let advisor = FeedAdvisor::new(&config.llm)?;
    let suggestion = advisor.suggest(&text).await?;

    if suggestion.full_articles {
        println!("判断：条目本身是完整文章，无需开启 fetch。");
    } else {
        println!("判断：条目只是摘要/片段，建议开启 fetch = true。");
    }
    println!("理由：{}", suggestion.reason);
    println!();
    println!(
        "建议配置：\n[[feeds]]\nurl = \"{}\"\nfetch = {}",
        url, !suggestion.full_articles
    );
    Ok(())
}
