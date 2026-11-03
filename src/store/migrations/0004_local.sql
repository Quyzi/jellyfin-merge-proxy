-- State the proxy owns itself: per-user settings (UserConfiguration JSON),
-- display preferences per (user, preferences guid, client), and playlists.
CREATE TABLE user_settings (
    user_id    TEXT PRIMARY KEY,
    config     TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE display_prefs (
    user_id    TEXT NOT NULL,
    prefs_id   TEXT NOT NULL,
    client     TEXT NOT NULL,
    data       TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, prefs_id, client)
);

CREATE TABLE playlists (
    id            TEXT PRIMARY KEY,
    owner_user_id TEXT NOT NULL,
    data          TEXT NOT NULL,
    updated_at    INTEGER NOT NULL
);
