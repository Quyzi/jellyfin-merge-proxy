-- Canonical catalog index: what was last crawled from each server.
CREATE TABLE index_servers (
    server    TEXT PRIMARY KEY,
    synced_at INTEGER NOT NULL,
    -- JSON object: parental rating name -> score on that server.
    ratings   TEXT NOT NULL
);

-- One row per item per server; `data` is the JSON index entry.
CREATE TABLE index_entries (
    server      TEXT NOT NULL,
    upstream_id TEXT NOT NULL,
    data        TEXT NOT NULL,
    PRIMARY KEY (server, upstream_id)
);

-- Stable canonical id of every grouped proxy id (copies of one item on
-- several servers; genres/studios/people merged by name).
CREATE TABLE canonical_ids (
    member    TEXT PRIMARY KEY,
    canonical TEXT NOT NULL
);

-- Merged library views, so their ids resolve before the first refresh.
CREATE TABLE merged_views (
    proxy_id TEXT PRIMARY KEY,
    data     TEXT NOT NULL
);
