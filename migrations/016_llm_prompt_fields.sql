-- Add prompt name and version columns to llm_traces for prompt version tracking.

ALTER TABLE llm_traces ADD COLUMN prompt_name TEXT;
ALTER TABLE llm_traces ADD COLUMN prompt_version TEXT;

CREATE INDEX IF NOT EXISTS idx_llm_traces_prompt
    ON llm_traces(project_id, prompt_name, prompt_version);
