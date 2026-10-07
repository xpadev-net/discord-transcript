ALTER TABLE domain_knowledge_items
ADD COLUMN IF NOT EXISTS allow_summary_context BOOLEAN NOT NULL DEFAULT FALSE;
