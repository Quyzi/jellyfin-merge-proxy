-- Proxy id -> upstream item. `seq` keeps insertion order.
CREATE TABLE ids (
    seq         INTEGER PRIMARY KEY,
    proxy_id    TEXT NOT NULL UNIQUE,
    server      TEXT NOT NULL,
    upstream_id TEXT NOT NULL
);

-- Copies of one series on several servers (pairs of proxy ids).
CREATE TABLE sibling_links (
    seq INTEGER PRIMARY KEY,
    a   TEXT NOT NULL,
    b   TEXT NOT NULL,
    UNIQUE (a, b)
);

-- Client sessions, keyed by the token handed out at login.
CREATE TABLE sessions (
    token       TEXT PRIMARY KEY,
    session_id  TEXT NOT NULL,
    user_name   TEXT NOT NULL,
    user_id     TEXT NOT NULL,
    client      TEXT NOT NULL,
    device_name TEXT NOT NULL,
    device_id   TEXT NOT NULL,
    app_version TEXT NOT NULL,
    created     TEXT NOT NULL
);
