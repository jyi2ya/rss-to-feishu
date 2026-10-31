-- 旧版程序把“没有发布时间”存成空字符串；空串无法按日期解码，
-- 统一转为 NULL（NULL 才是“没有时间”的规范表达）。
UPDATE items SET pub_date = NULL WHERE pub_date = '';
