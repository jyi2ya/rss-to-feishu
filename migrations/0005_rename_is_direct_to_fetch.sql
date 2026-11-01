-- 配置里的 direct 改叫 fetch：正文直读（push 时经 Fetch 工具抓全文）。
ALTER TABLE items RENAME COLUMN is_direct TO fetch;
