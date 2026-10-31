-- 条目表：sync 写入待推送条目，push 更新摘要与状态，migrate 导入旧库数据。
CREATE TABLE IF NOT EXISTS items (
    id TEXT PRIMARY KEY NOT NULL,
    source TEXT NOT NULL,
    title TEXT NOT NULL,
    link TEXT NOT NULL,
    description_text TEXT NOT NULL,
    pub_date TEXT NOT NULL,
    is_direct INTEGER NOT NULL DEFAULT 0,
    source_tags TEXT NOT NULL DEFAULT '',
    enqueued_at TEXT NOT NULL,
    sort_key INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    llm_title TEXT,
    llm_summary TEXT,
    llm_tags TEXT,
    sent_at TEXT,
    error TEXT
);

CREATE INDEX IF NOT EXISTS idx_items_pending ON items (status, sort_key);
