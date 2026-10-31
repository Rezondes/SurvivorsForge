mod common;

use sqlx::PgPool;
use sqlx::postgres::PgConnectOptions;

#[sqlx::test]
async fn reader_cannot_write_or_read_sessions(pool: PgPool) {
    forge_api::grafana::set_reader_password(&pool, "it's-a-test").await.unwrap();
    let database: String = sqlx::query_scalar("SELECT current_database()").fetch_one(&pool).await.unwrap();
    let options: PgConnectOptions = std::env::var("DATABASE_URL").unwrap().parse::<PgConnectOptions>().unwrap().username("grafana_reader").password("it's-a-test").database(&database);
    let reader = sqlx::PgPool::connect_with(options).await.expect("the reader logs in with the quoted password");

    let addons: i64 = sqlx::query_scalar("SELECT count(*) FROM addons").fetch_one(&reader).await.unwrap();
    assert_eq!(addons, 0);
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM telemetry_events").fetch_one(&reader).await.expect("statistics tables are readable");
    for secret in ["sessions", "login_codes", "oauth_states"] {
        assert!(sqlx::query(&format!("SELECT 1 FROM {secret}")).execute(&reader).await.is_err(), "{secret} stays closed");
    }
    assert!(sqlx::query("DELETE FROM addons").execute(&reader).await.is_err(), "read only");
    assert!(sqlx::query("UPDATE users SET role = 'admin'").execute(&reader).await.is_err(), "read only");
}
