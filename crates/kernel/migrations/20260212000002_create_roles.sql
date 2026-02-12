-- Create roles and permissions tables

-- Roles table
CREATE TABLE roles (
    id UUID PRIMARY KEY,
    name VARCHAR(255) NOT NULL UNIQUE,
    created TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Role permissions (what each role can do)
CREATE TABLE role_permissions (
    role_id UUID NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
    permission VARCHAR(255) NOT NULL,
    PRIMARY KEY (role_id, permission)
);

-- Index for permission lookups
CREATE INDEX idx_role_permissions_permission ON role_permissions(permission);

-- User roles (which roles each user has)
CREATE TABLE user_roles (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role_id UUID NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
    PRIMARY KEY (user_id, role_id)
);

-- Index for user role lookups
CREATE INDEX idx_user_roles_user_id ON user_roles(user_id);

-- Seed default roles
INSERT INTO roles (id, name) VALUES
    ('00000000-0000-0000-0000-000000000001', 'anonymous user'),
    ('00000000-0000-0000-0000-000000000002', 'authenticated user');

-- Anonymous user gets the anonymous role
INSERT INTO user_roles (user_id, role_id) VALUES
    ('00000000-0000-0000-0000-000000000000', '00000000-0000-0000-0000-000000000001');

-- Default permissions for anonymous users
INSERT INTO role_permissions (role_id, permission) VALUES
    ('00000000-0000-0000-0000-000000000001', 'access content');

-- Default permissions for authenticated users
INSERT INTO role_permissions (role_id, permission) VALUES
    ('00000000-0000-0000-0000-000000000002', 'access content'),
    ('00000000-0000-0000-0000-000000000002', 'view own profile');
