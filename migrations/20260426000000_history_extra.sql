-- Add stops and sensors to history cache
ALTER TABLE history_days ADD COLUMN stops_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE history_days ADD COLUMN sensors_json TEXT NOT NULL DEFAULT '[]';
