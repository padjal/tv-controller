CREATE TABLE IF NOT EXISTS devices (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    ip            TEXT NOT NULL,
    state         TEXT NOT NULL DEFAULT 'Idle',
    current_video TEXT,
    last_seen     INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS videos (
    id            TEXT PRIMARY KEY,
    filename      TEXT NOT NULL UNIQUE,
    path          TEXT NOT NULL,
    size_bytes    INTEGER NOT NULL,
    duration_secs INTEGER,
    added_at      INTEGER NOT NULL
);
