mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{Reply, TestApp, send};
use forge_api::github::types::User;
use image::{ImageEncoder, RgbImage};
use serde_json::json;
use sqlx::PgPool;

fn png(w: u32, h: u32, shade: u8) -> Vec<u8> {
    let mut out = Vec::new();
    let img = RgbImage::from_fn(w, h, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, shade]));
    image::codecs::png::PngEncoder::new(&mut out).write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgb8).unwrap();
    out
}

async fn upload(app: &TestApp, token: &str, bytes: &[u8], kind: &str) -> Reply {
    upload_with(app, token, bytes, kind, &[]).await
}

async fn upload_with(app: &TestApp, token: &str, bytes: &[u8], kind: &str, fields: &[(&str, &str)]) -> Reply {
    let boundary = "XBOUNDARYX";
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"kind\"\r\n\r\n{kind}\r\n").as_bytes());
    for (name, value) in fields {
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes());
    }
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.png\"\r\nContent-Type: image/png\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let request = Request::post("/v1/addons/lucky/images")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", format!("multipart/form-data; boundary={boundary}"))
        .body(Body::from(body))
        .unwrap();
    send(&app.router, request).await
}

fn app_with_dir(pool: PgPool) -> (TestApp, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_string_lossy().into_owned();
    (TestApp::with_config(pool, move |c| c.image_dir = path), dir)
}

fn files(dir: &tempfile::TempDir) -> usize {
    std::fs::read_dir(dir.path()).unwrap().count()
}

#[sqlx::test]
async fn too_big_upload_413(pool: PgPool) {
    let (app, _dir) = app_with_dir(pool);
    let token = app.publish(1, "alice", "lucky").await;
    let reply = upload(&app, &token, &vec![0u8; 6 * 1024 * 1024], "screenshot").await;
    assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[sqlx::test]
async fn upload_stores_webp_and_reports_resize(pool: PgPool) {
    let (app, dir) = app_with_dir(pool);
    let token = app.publish(1, "alice", "lucky").await;
    let reply = upload(&app, &token, &png(3000, 1000, 1), "screenshot").await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let answer = reply.json();
    assert_eq!(answer["notice"], "resized");
    assert_eq!(answer["original"], json!({ "width": 3000, "height": 1000 }));
    assert_eq!(answer["stored"], json!({ "width": 1920, "height": 640 }));
    let url = answer["url"].as_str().unwrap();
    assert!(url.starts_with("https://web.test/img/") && url.ends_with(".webp"), "{url}");
    assert_eq!(files(&dir), 1);

    let view = app.get("/v1/addons/lucky", None).await.json();
    assert_eq!(view["images"][0]["url"], url);
    assert_eq!(upload(&app, &token, b"GIF89a", "screenshot").await.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[sqlx::test]
async fn limits_and_icon_square(pool: PgPool) {
    let (app, _dir) = app_with_dir(pool);
    let token = app.publish(1, "alice", "lucky").await;
    let icon = upload(&app, &token, &png(300, 200, 9), "icon").await;
    assert_eq!((icon.json()["width"].as_u64(), icon.json()["height"].as_u64()), (Some(256), Some(256)));
    let replaced = upload(&app, &token, &png(300, 200, 8), "icon").await;
    assert_eq!(replaced.status, StatusCode::CREATED, "a new icon replaces the old one");
    for shade in 0..10 {
        assert_eq!(upload(&app, &token, &png(20, 20, shade), "screenshot").await.status, StatusCode::CREATED);
    }
    let eleventh = upload(&app, &token, &png(20, 20, 99), "screenshot").await;
    assert_eq!((eleventh.status, eleventh.error().as_str()), (StatusCode::CONFLICT, "image_limit"));
    assert!(app.get("/v1/addons/lucky", None).await.json()["icon_url"].is_string());
}

#[sqlx::test]
async fn the_icon_is_replaced_cropped_and_deleted(pool: PgPool) {
    let (app, dir) = app_with_dir(pool);
    let token = app.publish(1, "alice", "lucky").await;
    let first = upload(&app, &token, &png(300, 200, 1), "icon").await.json();
    let second = upload_with(&app, &token, &png(300, 200, 2), "icon", &[("crop_x", "200"), ("crop_y", "0"), ("crop_size", "100")]).await;
    assert_eq!((second.status, second.json()["width"].as_u64()), (StatusCode::CREATED, Some(256)));
    assert_ne!(first["url"], second.json()["url"]);
    assert_eq!(app.get("/v1/addons/lucky", None).await.json()["icon_url"], second.json()["url"], "the new icon is the one shown");
    assert_eq!(files(&dir), 1, "the old icon file is gone");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM images WHERE kind = 'icon'").fetch_one(&app.pool).await.unwrap();
    assert_eq!(count, 1);

    let bad = upload_with(&app, &token, &png(300, 200, 3), "icon", &[("crop_x", "250"), ("crop_y", "0"), ("crop_size", "100")]).await;
    assert_eq!((bad.status, bad.error().as_str()), (StatusCode::UNPROCESSABLE_ENTITY, "invalid_crop"), "outside the image");

    assert_eq!(app.delete("/v1/addons/lucky/icon", Some(&token)).await.status, StatusCode::NO_CONTENT);
    assert!(app.get("/v1/addons/lucky", None).await.json()["icon_url"].is_null());
    assert_eq!(files(&dir), 0);
    assert_eq!(app.delete("/v1/addons/lucky/icon", Some(&token)).await.status, StatusCode::NOT_FOUND, "no icon left");
}

#[sqlx::test]
async fn permission_checks(pool: PgPool) {
    let (app, _dir) = app_with_dir(pool);
    let owner = app.publish(1, "alice", "lucky").await;
    for (id, login) in [(2, "bob"), (3, "carol")] {
        app.github.data().users.insert(login.into(), User { id, login: login.into(), avatar_url: String::new() });
    }
    app.post("/v1/addons/lucky/members", Some(&owner), json!({ "login": "bob", "can_edit_images": true })).await;
    app.post("/v1/addons/lucky/members", Some(&owner), json!({ "login": "carol", "can_edit_description": true })).await;
    let bob = app.login(2, "bob").await;
    let carol = app.login(3, "carol").await;
    let stranger = app.login(4, "dave").await;

    let reply = upload(&app, &bob, &png(20, 20, 1), "screenshot").await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let id = reply.json()["id"].as_i64().unwrap();
    assert_eq!(upload(&app, &carol, &png(20, 20, 2), "screenshot").await.status, StatusCode::FORBIDDEN);
    assert_eq!(upload(&app, &stranger, &png(20, 20, 3), "screenshot").await.status, StatusCode::FORBIDDEN);
    assert_eq!(app.patch(&format!("/v1/addons/lucky/images/{id}"), Some(&carol), json!({ "caption": "x" })).await.status, StatusCode::FORBIDDEN);
    assert_eq!(app.patch(&format!("/v1/addons/lucky/images/{id}"), Some(&bob), json!({ "caption": "Main view", "position": 3 })).await.status, StatusCode::NO_CONTENT);
    assert_eq!(app.delete(&format!("/v1/addons/lucky/images/{id}"), Some(&stranger)).await.status, StatusCode::FORBIDDEN);
    assert_eq!(app.delete(&format!("/v1/addons/lucky/images/{id}"), Some(&bob)).await.status, StatusCode::NO_CONTENT);
}

#[sqlx::test]
async fn dedupe_and_refcounted_delete(pool: PgPool) {
    let (app, dir) = app_with_dir(pool);
    let token = app.publish(1, "alice", "lucky").await;
    let same = png(40, 40, 7);
    let first = upload(&app, &token, &same, "screenshot").await.json();
    let second = upload(&app, &token, &same, "screenshot").await.json();
    assert_eq!(first["url"], second["url"], "same content, same file");
    assert_eq!(files(&dir), 1);
    app.delete(&format!("/v1/addons/lucky/images/{}", first["id"]), Some(&token)).await;
    assert_eq!(files(&dir), 1, "still used by the second entry");
    app.delete(&format!("/v1/addons/lucky/images/{}", second["id"]), Some(&token)).await;
    assert_eq!(files(&dir), 0);
}
