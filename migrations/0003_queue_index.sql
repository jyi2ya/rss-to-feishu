-- 队列扫描索引：push 按 (status, sort_key, enqueued_at) 取队头，
-- status 子命令按 status 分组统计也走它的前导列；
-- id 已有主键索引，无需重复建。
DROP INDEX IF EXISTS idx_items_pending;
CREATE INDEX IF NOT EXISTS idx_items_status_sort_key_enqueued_at
    ON items (status, sort_key, enqueued_at);
