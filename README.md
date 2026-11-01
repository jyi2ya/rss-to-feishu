# rss-to-feishu

把 RSS/Atom 订阅聚合起来，交给 LLM 生成中文标题、摘要与标签，再推送到飞书群的命令行工具。
旧版 `rss-feishu-bridge`（Perl）的 Rust 重写：去掉了压缩、重试、节流和标签聚类，配置、存储与 HTTP 客户端全部重做。

## 特性

- **`sync`**：并发抓取配置里的订阅源（feed-rs 解析 RSS/Atom/JSON Feed），新条目入 sqlite；
- **`push`**：取 pending 条目，`fetch = true` 的源先经网页抓取服务（DeepSeek Search API 的 Fetch 工具）抓网页正文，再用 rig 做结构化提取（标题/摘要/标签），推送到飞书；
- **`migrate`**：把旧版 db（`db/*.csv` + `db.jsonl`，含 `GZ:` 压缩字段）迁进 sqlite，并打印旧订阅源对应的 `[feeds]` TOML 片段；
- 飞书渠道是配置数组：`tenant`（自建应用，用环境变量里的 tenant_access_token 发消息，不调登录接口）或 `webhook`（群机器人），带标签白名单；
- HTTP 请求约束：网页抓取接口（DeepSeek Search API 的 Fetch 工具，base_url 可配官方或本地兼容实现）与飞书群消息接口都写成 OpenAPI 放在 `openapi/` 下，经 progenitor 在编译期生成客户端。

## 构建

```sh
cargo build --release
```

## 配置

复制 `config.example.toml` 为 `config.toml`（含密钥，不入库）：

- `database_path`：sqlite 文件路径（必填）；
- `[llm]`：OpenAI 兼容接口的 `api_base` / `api_key` / `model`（必填）；
- `[fetch]`：网页抓取服务的 `base_url` 与可选 `api_key`（指向官方 API 时必填）；
- `[sync]`：`concurrency`（默认 8）、`timeout_secs`（默认 60）、`proxy`（缺省回退 `https_proxy`/`http_proxy` 环境变量）；
- `[[channels]]`：渠道数组。`type = "tenant"` 配 `token_env`（存 tenant_access_token 的环境变量名）/`chat_id`；`type = "webhook"` 配 `url`。`tags` 是白名单：非空时只收含这些标签（任一命中）的条目，为空兜底收全部；
- `[[feeds]]`：订阅源数组，每个源有 `url`、`tags`、`fetch`（正文直读，缺省 false）。

渠道按数组顺序取**首个**白名单命中的发送，空白名单渠道放最后即可兜底。

## 用法

```sh
# 抓取全部订阅源，新条目入队
rss-to-feishu --config config.toml sync

# 摘要并推送 pending 条目；缺省只推 1 条，--limit 限量（分批消化历史积压时可用）
rss-to-feishu --config config.toml push
rss-to-feishu --config config.toml push --limit 10

# 迁移旧版 rss-feishu-bridge 的 db（旧项目目录内含 db/*.csv 与 db.jsonl）
rss-to-feishu --config config.toml migrate --from /path/to/old/bridge

# 查看条目状态统计（默认 ASCII 表格，--json 输出机器可读 JSON）
rss-to-feishu --config config.toml status
rss-to-feishu --config config.toml status --json
```

`push` 失败按出错点落入不同状态（由 thiserror 定义、strum 生成状态字符串）：正文抓取失败 -> `failed_fetch`，LLM 提取失败 -> `failed_llm`，摘要成功但渠道投递失败 -> `failed_send`，其余（消息编码、无匹配渠道等）-> `failed_other`；都不重试，存在失败时命令以错误退出。`status` 展示同步总数、待投递、投递成功、正文抓取失败、LLM 总结失败、投递失败、其他失败七项。

## 调度

没有内置节流，用 cron 控制节奏即可，例如：

```cron
*/30 * * * *  cd /path/to/rss-to-feishu && ./target/release/rss-to-feishu sync
*/15 * * * *  cd /path/to/rss-to-feishu && ./target/release/rss-to-feishu push --limit 1
```

## 与旧版的行为兼容

- 条目 id 沿用旧版规则 `md5("频道标题/条目id")`，迁移后不会重复入队；
- 排序键沿用旧版：发布时间 epoch + `hex(id 前 8 位) mod 86400`（同日条目打散）；
- 推送文本格式沿用旧版：LLM 标题 / 摘要 / `#标签` / 来源@时间 / 链接。

## 依赖选择

| 用途 | 库 |
| --- | --- |
| RSS/Atom 解析 | [feed-rs](https://crates.io/crates/feed-rs) |
| 存储 | sqlite + [sqlx](https://crates.io/crates/sqlx) |
| LLM 结构化输出 | [rig](https://crates.io/crates/rig-core) |

| Fetch 工具客户端 | [progenitor](https://crates.io/crates/progenitor)（spec 见 `openapi/search.yaml`） |
| 飞书群消息客户端 | [progenitor](https://crates.io/crates/progenitor)（spec 见 `openapi/feishu.yaml`） |
