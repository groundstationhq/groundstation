-- When the newest event a finding describes arrived. The finding goes when
-- its tenant stops keeping that event, not a retention period after the
-- worker last saw it, which could be days later.
ALTER TABLE insights ADD COLUMN evidence_received_at timestamptz;
