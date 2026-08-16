CREATE TABLE plugin_migration (
    plugin VARCHAR(64) NOT NULL,
    migration VARCHAR(255) NOT NULL,
    applied_at BIGINT NOT NULL,
    PRIMARY KEY (plugin, migration)
);
