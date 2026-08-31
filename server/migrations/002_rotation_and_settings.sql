-- Per-device display rotation, in degrees clockwise (0, 90, 180, 270).
--
-- TVs are mounted on walls and some are physically turned; the content has to
-- be rotated to match. Applied by the agent as an mpv property rather than by
-- rotating the Pi's framebuffer, so it takes effect without a reboot.
ALTER TABLE devices ADD COLUMN rotation INTEGER NOT NULL DEFAULT 0;

-- Fleet-wide key/value settings. Currently just `idle_background`: the
-- filename of a library image shown when a TV has nothing playing, so an idle
-- screen shows the client's artwork instead of the Raspberry Pi desktop.
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT
);
