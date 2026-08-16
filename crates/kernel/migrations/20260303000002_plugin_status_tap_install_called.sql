ALTER TABLE plugin_status
    ADD COLUMN tap_install_called BOOLEAN NOT NULL DEFAULT FALSE;
