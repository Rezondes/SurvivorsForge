-- Moderators: everything under /admin except giving roles and Grafana. `granted_role` is the role
-- an admin gave on the website; it replaces `admin_granted` and, like it, survives every login.
-- Admins from ADMIN_GITHUB_IDS stay admins whatever it says.
ALTER TABLE users DROP CONSTRAINT users_role_check;
ALTER TABLE users ADD CONSTRAINT users_role_check CHECK (role IN ('user', 'moderator', 'admin'));
ALTER TABLE users ADD COLUMN granted_role TEXT CHECK (granted_role IN ('moderator', 'admin'));
UPDATE users SET granted_role = 'admin' WHERE admin_granted;
ALTER TABLE users DROP COLUMN admin_granted;
