//! The database role Grafana reads with (migration 0029).

use sqlx::PgPool;

/// Lets `grafana_reader` log in with `password`. Postgres takes no parameter in `ALTER ROLE`, so
/// the database quotes the password itself (`format('%L')`).
pub async fn set_reader_password(pool: &PgPool, password: &str) -> sqlx::Result<()> {
    let statement: String = sqlx::query_scalar("SELECT format('ALTER ROLE grafana_reader WITH LOGIN PASSWORD %L', $1::text)").bind(password).fetch_one(pool).await?;
    sqlx::raw_sql(&statement).execute(pool).await?;
    Ok(())
}
