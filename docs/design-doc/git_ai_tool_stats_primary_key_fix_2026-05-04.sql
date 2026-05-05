-- Read-only checks for the current schema and collision symptoms.
SHOW CREATE TABLE git_ai_tool_stats;
SHOW CREATE TABLE git_ai_commit_stats;

SELECT MIN(id) AS min_id,
       MAX(id) AS max_id,
       SUM(CASE WHEN id < 0 THEN 1 ELSE 0 END) AS negative_ids,
       COUNT(*) AS total_rows
FROM git_ai_tool_stats;

SELECT id, source_id, source_type, tool, model, add_line, create_time
FROM git_ai_tool_stats
WHERE id = 505335810;

-- Apply this together with, or after, deploying the ai-cr-manage-service code
-- that changes GitAiToolStats.id from Integer to Long.
ALTER TABLE git_ai_tool_stats
    MODIFY COLUMN id BIGINT NOT NULL COMMENT 'id';

-- Post-change verification.
SHOW CREATE TABLE git_ai_tool_stats;

SELECT COLUMN_NAME, DATA_TYPE, COLUMN_TYPE
FROM INFORMATION_SCHEMA.COLUMNS
WHERE TABLE_SCHEMA = DATABASE()
  AND TABLE_NAME = 'git_ai_tool_stats'
  AND COLUMN_NAME = 'id';