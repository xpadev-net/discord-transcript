ALTER TABLE guild_settings
    ADD COLUMN IF NOT EXISTS bot_token_ciphertext TEXT,
    ADD COLUMN IF NOT EXISTS bot_token_nonce TEXT,
    ADD COLUMN IF NOT EXISTS bot_token_key_version TEXT,
    ADD COLUMN IF NOT EXISTS bot_token_updated_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS bot_token_last_validated_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS bot_user_id TEXT,
    ADD COLUMN IF NOT EXISTS bot_username TEXT;
