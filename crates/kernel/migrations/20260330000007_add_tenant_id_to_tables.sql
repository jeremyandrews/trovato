-- Add tenant_id to all content tables (Story 46.1).
-- Uses DEFAULT_TENANT_ID so existing single-tenant data works unchanged.
-- NOT NULL with DEFAULT means INSERT without tenant_id still works.

-- Items and revisions
ALTER TABLE item ADD COLUMN IF NOT EXISTS tenant_id UUID NOT NULL DEFAULT '0193a5a0-0001-7000-8000-000000000001' REFERENCES tenant(id);
ALTER TABLE item_revision ADD COLUMN IF NOT EXISTS tenant_id UUID NOT NULL DEFAULT '0193a5a0-0001-7000-8000-000000000001' REFERENCES tenant(id);

-- Categories
ALTER TABLE category ADD COLUMN IF NOT EXISTS tenant_id UUID NOT NULL DEFAULT '0193a5a0-0001-7000-8000-000000000001' REFERENCES tenant(id);
ALTER TABLE category_tag ADD COLUMN IF NOT EXISTS tenant_id UUID NOT NULL DEFAULT '0193a5a0-0001-7000-8000-000000000001' REFERENCES tenant(id);

-- Supporting tables
ALTER TABLE file_managed ADD COLUMN IF NOT EXISTS tenant_id UUID NOT NULL DEFAULT '0193a5a0-0001-7000-8000-000000000001' REFERENCES tenant(id);
ALTER TABLE site_config ADD COLUMN IF NOT EXISTS tenant_id UUID NOT NULL DEFAULT '0193a5a0-0001-7000-8000-000000000001' REFERENCES tenant(id);
ALTER TABLE url_alias ADD COLUMN IF NOT EXISTS tenant_id UUID NOT NULL DEFAULT '0193a5a0-0001-7000-8000-000000000001' REFERENCES tenant(id);
ALTER TABLE menu_link ADD COLUMN IF NOT EXISTS tenant_id UUID NOT NULL DEFAULT '0193a5a0-0001-7000-8000-000000000001' REFERENCES tenant(id);
ALTER TABLE tile ADD COLUMN IF NOT EXISTS tenant_id UUID NOT NULL DEFAULT '0193a5a0-0001-7000-8000-000000000001' REFERENCES tenant(id);
ALTER TABLE comment ADD COLUMN IF NOT EXISTS tenant_id UUID NOT NULL DEFAULT '0193a5a0-0001-7000-8000-000000000001' REFERENCES tenant(id);

-- Indexes for tenant-scoped queries
CREATE INDEX IF NOT EXISTS idx_item_tenant_id ON item(tenant_id);
CREATE INDEX IF NOT EXISTS idx_item_revision_tenant_id ON item_revision(tenant_id);
CREATE INDEX IF NOT EXISTS idx_category_tenant_id ON category(tenant_id);
CREATE INDEX IF NOT EXISTS idx_category_tag_tenant_id ON category_tag(tenant_id);
CREATE INDEX IF NOT EXISTS idx_file_managed_tenant_id ON file_managed(tenant_id);
CREATE INDEX IF NOT EXISTS idx_url_alias_tenant_id ON url_alias(tenant_id);
CREATE INDEX IF NOT EXISTS idx_comment_tenant_id ON comment(tenant_id);
