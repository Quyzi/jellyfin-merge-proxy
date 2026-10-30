-- Per-proxy-user watch state, keyed by the (canonical) proxy item id.
-- `state` is the JSON user data (played, play count, position, favourite,
-- likes, rating, last played date).
CREATE TABLE user_data (
    user_id    TEXT NOT NULL,
    item_id    TEXT NOT NULL,
    state      TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, item_id)
);

-- (proxy user, server name) pairs whose upstream state was imported once.
CREATE TABLE user_data_seeded (
    user_id   TEXT NOT NULL,
    server    TEXT NOT NULL,
    seeded_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, server)
);
