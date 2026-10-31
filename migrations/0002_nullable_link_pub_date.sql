-- 条目表重新设计：link 与 pub_date 允许为空（源里缺失时存 NULL），
-- 展示/排序时由程序兜底。SQLite 不能改 NOT NULL，重建表并原样搬运数据。
CREATE TABLE items_new (
    id TEXT PRIMARY KEY NOT NULL,
    source TEXT NOT NULL,
    title TEXT NOT NULL,
    link TEXT,
    description_text TEXT NOT NULL,
    pub_date TEXT,
    is_direct INTEGER NOT NULL DEFAULT 0,
    source_tags TEXT NOT NULL DEFAULT '[]',
    enqueued_at TEXT NOT NULL,
    sort_key INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    llm_title TEXT,
    llm_summary TEXT,
    llm_tags TEXT,
    sent_at TEXT,
    error TEXT
);

INSERT INTO items_new SELECT * FROM items;
DROP TABLE items;
ALTER TABLE items_new RENAME TO items;

CREATE INDEX IF NOT EXISTS idx_items_pending ON items (status, sort_key);
