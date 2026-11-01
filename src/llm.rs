//! 用 rig 把原 brain 的 `/text/preview` rpc 直接做进本二进制：
//! 把一段文本交给 LLM，结构化提取标题、摘要与标签，不再依赖 brain 服务。
//!
//! 输出约束沿用旧版 brain `text_preview` 的 schema 与 prompt。

use anyhow::Context;
use rig::providers::openai::Client;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::Llm;

/// LLM 需要产出的结构化结果。字段描述会被 rig 写进 prompt。
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Preview {
    #[schemars(description = "用20字以内概括核心主题")]
    pub title: String,
    #[schemars(
        description = "用300字以内概括主要内容，包含关键信息和结论，保持完整句子结构。一段写完不要分段也不要小标题。对专业术语保持一致性。注意保持语言流畅自然，尽量避免使用欧化汉语"
    )]
    pub preview: String,
    #[schemars(
        description = "标签数组，数量在 7 到 10 个之间，按重要性降序排列。优先提取文本中的原有关键词。每个标签：名词或者形容词，禁止以井号开头，禁止输出空格，尽量使用中文，如果是英文多单词短语则使用大驼峰格式，禁止出现空格和特殊符号"
    )]
    pub tags: Vec<String>,
}

/// 持有构建好的 extractor，跨条目复用。
pub struct Summarizer {
    extractor: rig::extractor::Extractor<rig::providers::openai::CompletionModel, Preview>,
}

impl Summarizer {
    pub fn new(llm: &Llm) -> anyhow::Result<Self> {
        // 本地网关可能不校验 key，缺省传空串。
        let client = Client::from_url(llm.api_key.as_deref().unwrap_or(""), llm.api_base.as_str());
        let extractor = client
            .extractor::<Preview>(&llm.model)
            .preamble("你是一个文本分析专家。根据给定文段，提取出文段的标题、摘要和关键词，无论文段的语言是什么，你必须使用简体中文编写。")
            .build();
        Ok(Self { extractor })
    }

    pub async fn preview(&self, content: &str) -> anyhow::Result<Preview> {
        self.extractor
            .extract(content)
            .await
            .with_context(|| "llm structured extraction failed")
    }
}
