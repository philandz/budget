-- Add avatar column to users table if not exists
-- This is needed because budget service JOINs with users table but the avatar column
-- might not be tracked in budget's migration history (identity owns the users table schema)

ALTER TABLE users ADD COLUMN IF NOT EXISTS avatar TEXT NULL AFTER display_name;