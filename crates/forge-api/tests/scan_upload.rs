mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{Reply, TestApp, send};
use serde_json::Value;
use sqlx::PgPool;
use std::io::{Cursor, Write};

fn zip(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, content) in entries {
        writer.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
        writer.write_all(content.as_bytes()).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

async fn scan(app: &TestApp, token: Option<&str>, bytes: &[u8]) -> Reply {
    let boundary = "XBOUNDARYX";
    let mut body = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.zip\"\r\nContent-Type: application/zip\r\n\r\n").into_bytes();
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let mut request = Request::post("/v1/scan").header("content-type", format!("multipart/form-data; boundary={boundary}"));
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    send(&app.router, request.body(Body::from(body)).unwrap()).await
}

fn rules(reply: &Reply) -> Vec<(String, String)> {
    reply.json()["findings"].as_array().unwrap().iter().map(|f| (f["rule"].as_str().unwrap().into(), f["severity"].as_str().unwrap().into())).collect()
}

const TOC: &str = "## Interface: 11200\nLucky.lua\n";

#[sqlx::test]
async fn creators_scan_a_zip_before_publishing(pool: PgPool) {
    let app = TestApp::new(pool);
    let alice = app.publish(1, "alice", "lucky").await;
    let bob = app.login(2, "bob").await;
    let harmful = zip(&[("Lucky/Lucky.toc", TOC), ("Lucky/Lucky.lua", "os.execute('calc')")]);

    assert_eq!(scan(&app, None, &harmful).await.status, StatusCode::UNAUTHORIZED);

    let own = scan(&app, Some(&alice), &harmful).await;
    assert_eq!(own.status, StatusCode::OK, "{}", own.body);
    assert_eq!((own.json()["result"].clone(), own.json()["folders"].clone()), (Value::from("flagged"), serde_json::json!(["Lucky"])));
    assert!(rules(&own).contains(&("lua.forbidden_global".into(), "critical".into())), "{:?}", rules(&own));
    assert!(!rules(&own).iter().any(|(r, _)| r == "zip.foreign_folder" || r == "zip.undeclared_folder"), "the owner updates their own addon");

    let other = scan(&app, Some(&bob), &zip(&[("Lucky/Lucky.toc", TOC), ("Lucky/Lucky.lua", "print(1)")])).await;
    assert!(rules(&other).contains(&("zip.foreign_folder".into(), "critical".into())), "{:?}", rules(&other));

    let clean = scan(&app, Some(&bob), &zip(&[("Odds/Odds.toc", "## Interface: 11200\nOdds.lua\n"), ("Odds/Odds.lua", "print(1)")])).await;
    assert_eq!(clean.json()["result"], "clean", "{}", clean.body);
    assert!(clean.json()["scanner_version"].is_string());
}

#[sqlx::test]
async fn scan_needs_a_file(pool: PgPool) {
    let app = TestApp::new(pool);
    let bob = app.login(2, "bob").await;
    let reply = scan(&app, Some(&bob), b"").await;
    assert_eq!((reply.status, reply.error().as_str()), (StatusCode::UNPROCESSABLE_ENTITY, "no_file"));
    let broken = scan(&app, Some(&bob), b"not a zip").await;
    assert!(rules(&broken).contains(&("zip.unreadable".into(), "critical".into())), "{}", broken.body);
}
