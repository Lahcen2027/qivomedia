-- Uploads that need converting (AVI, MPEG-2, VOB, HEVC… → H.264 MP4; PDF → page images)
-- are processed in the background. Screens only receive media whose status is 'ready'.
ALTER TABLE media ADD COLUMN status TEXT NOT NULL DEFAULT 'ready';   -- ready | processing | failed
ALTER TABLE media ADD COLUMN progress INTEGER NOT NULL DEFAULT 100;  -- 0..100 while processing
ALTER TABLE media ADD COLUMN error TEXT;                             -- why it failed, or a note
ALTER TABLE media ADD COLUMN pages TEXT;                             -- PDF: JSON array of page image file names
ALTER TABLE media ADD COLUMN width INTEGER;
ALTER TABLE media ADD COLUMN height INTEGER;
ALTER TABLE media ADD COLUMN source_file TEXT;                       -- original under data/media-src while processing

-- Live video: IP cameras and encoders (RTSP, RTMP, SRT, UDP/multicast) relayed by the server
-- as HLS, or HLS streams that screens play directly.
CREATE TABLE streams (
    id          TEXT PRIMARY KEY,
    org_id      TEXT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    source_enc  TEXT NOT NULL,                   -- encrypted: may contain camera credentials
    source_hint TEXT NOT NULL,                   -- the address without credentials, for display
    relay       INTEGER NOT NULL DEFAULT 1,      -- 1 = through the server, 0 = screens play it directly
    transcode   TEXT NOT NULL DEFAULT 'auto',    -- auto | always | never (video to H.264)
    video_codec TEXT,                            -- detected by the last test
    resolution  TEXT,
    last_error  TEXT,
    created_at  TEXT NOT NULL
);
CREATE INDEX idx_streams_org ON streams(org_id);
