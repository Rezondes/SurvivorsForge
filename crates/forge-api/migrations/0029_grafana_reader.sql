-- Grafana reads the database with its own role: SELECT only, and never the tables with login
-- secrets. The role cannot log in until the API sets its password from GRAFANA_DB_PASSWORD.
-- Roles belong to the whole server, so a second database (tests) finds it there already.
DO $$
BEGIN
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'grafana_reader') THEN
        CREATE ROLE grafana_reader NOLOGIN;
    END IF;
END
$$;

GRANT USAGE ON SCHEMA public TO grafana_reader;
GRANT SELECT ON ALL TABLES IN SCHEMA public TO grafana_reader;
REVOKE ALL ON sessions, login_codes, oauth_states FROM grafana_reader;
-- Tables of later migrations: readable too, unless that migration revokes it.
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT ON TABLES TO grafana_reader;
