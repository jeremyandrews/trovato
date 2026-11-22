-- B108 fixture: clubs, by slug and id, and who manages each.
CREATE TABLE IF NOT EXISTS tov_club (
    slug TEXT PRIMARY KEY,
    id   TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS tov_manager (
    club_id TEXT NOT NULL,
    user_id UUID NOT NULL,
    PRIMARY KEY (club_id, user_id)
);
