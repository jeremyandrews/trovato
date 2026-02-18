-- Seed text format permissions for the authenticated user role.
-- plain_text is always allowed (no permission needed).
-- filtered_html requires "use filtered_html" permission.
-- full_html requires "use full_html" permission (admin-only by default).

-- Grant "use filtered_html" to the authenticated user role.
-- The authenticated role UUID is 00000000-0000-0000-0000-000000000002.
INSERT INTO role_permissions (role_id, permission)
VALUES ('00000000-0000-0000-0000-000000000002', 'use filtered_html')
ON CONFLICT (role_id, permission) DO NOTHING;
