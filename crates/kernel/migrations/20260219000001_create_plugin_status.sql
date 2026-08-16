CREATE TABLE plugin_status (
    name VARCHAR(64) PRIMARY KEY,
    status SMALLINT NOT NULL DEFAULT 0,
    version VARCHAR(32) NOT NULL,
    installed_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);
