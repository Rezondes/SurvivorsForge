//! `/v1/addons/{slug}/images`: upload, reorder, caption and delete screenshots and the icon.

use super::addons::{AddonRow, load_addon, not_blocked, require, user_permissions};
use crate::app::AppState;
use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::images::{Crop, ImageError, Kind, MAX_SCREENSHOTS, MAX_UPLOAD, process};
use axum::Json;
use axum::extract::{Multipart, Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

const MAX_CAPTION: usize = 200;

async fn editable(state: &AppState, user: &CurrentUser, slug: &str) -> ApiResult<AddonRow> {
    let addon = load_addon(&state.pool, slug).await?;
    not_blocked(&addon)?;
    require(user_permissions(&state.pool, &addon, user).await?.edit_images, "You may not edit the images of this addon")?;
    Ok(addon)
}

fn bad(status: StatusCode, code: &'static str, message: &str) -> ApiError {
    ApiError::new(status, code, message)
}

fn validate_caption(caption: &str) -> ApiResult<()> {
    if caption.chars().count() <= MAX_CAPTION && !caption.chars().any(char::is_control) {
        Ok(())
    } else {
        Err(bad(StatusCode::UNPROCESSABLE_ENTITY, "invalid_caption", "A caption is at most 200 characters on one line"))
    }
}

/// Writes via a temp file and rename, so a crash never leaves half an image.
async fn store_file(dir: &str, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    let target = PathBuf::from(dir).join(name);
    if tokio::fs::try_exists(&target).await.unwrap_or(false) {
        return Ok(());
    }
    tokio::fs::create_dir_all(dir).await?;
    let tmp = target.with_extension(format!("tmp{}", rand::random::<u32>()));
    tokio::fs::write(&tmp, bytes).await?;
    tokio::fs::rename(&tmp, &target).await
}

pub async fn upload(State(state): State<AppState>, user: CurrentUser, Path(slug): Path<String>, mut form: Multipart) -> ApiResult<(StatusCode, Json<Value>)> {
    let addon = editable(&state, &user, &slug).await?;
    let (mut file, mut kind, mut caption) = (None, Kind::Screenshot, String::new());
    let mut crop: [Option<u32>; 3] = [None; 3];
    let broken = |e: axum::extract::multipart::MultipartError| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            bad(StatusCode::PAYLOAD_TOO_LARGE, "too_large", "Images may be at most 5 MB")
        } else {
            bad(StatusCode::BAD_REQUEST, "bad_upload", "The upload is broken")
        }
    };
    while let Some(field) = form.next_field().await.map_err(broken)? {
        match field.name().unwrap_or_default() {
            "file" => {
                let bytes = field.bytes().await.map_err(broken)?;
                if bytes.len() > MAX_UPLOAD {
                    return Err(bad(StatusCode::PAYLOAD_TOO_LARGE, "too_large", "Images may be at most 5 MB"));
                }
                file = Some(bytes);
            }
            "kind" => {
                let text = field.text().await.unwrap_or_default();
                kind = Kind::parse(&text).ok_or_else(|| bad(StatusCode::UNPROCESSABLE_ENTITY, "invalid_kind", "kind is screenshot or icon"))?;
            }
            "caption" => caption = field.text().await.unwrap_or_default(),
            name @ ("crop_x" | "crop_y" | "crop_size") => {
                let at = ["crop_x", "crop_y", "crop_size"].iter().position(|n| *n == name).unwrap_or(0);
                let text = field.text().await.unwrap_or_default();
                crop[at] = Some(text.trim().parse().map_err(|_| bad(StatusCode::UNPROCESSABLE_ENTITY, "invalid_crop", "The crop is whole pixels"))?);
            }
            _ => {}
        }
    }
    let file = file.ok_or_else(|| bad(StatusCode::UNPROCESSABLE_ENTITY, "no_file", "Send the image as the field file"))?;
    validate_caption(&caption)?;
    let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM images WHERE addon_id = $1 AND kind = $2")
        .bind(addon.id)
        .bind(kind.as_str())
        .fetch_one(&state.pool)
        .await?;
    // A new icon replaces the old one; screenshots have a limit.
    if kind == Kind::Screenshot && count >= MAX_SCREENSHOTS {
        return Err(bad(StatusCode::CONFLICT, "image_limit", "This addon has as many images of this kind as it may"));
    }
    let crop = match crop {
        [Some(x), Some(y), Some(size)] => Some(Crop { x, y, size }),
        [None, None, None] => None,
        _ => return Err(bad(StatusCode::UNPROCESSABLE_ENTITY, "invalid_crop", "Send crop_x, crop_y and crop_size together")),
    };

    let processed = tokio::task::spawn_blocking(move || process(&file, kind, crop)).await.map_err(|_| ApiError::internal())?;
    let processed = processed.map_err(|e| match e {
        ImageError::Unsupported => bad(StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_image", "Only PNG, JPEG and WebP images"),
        ImageError::TooLarge => bad(StatusCode::UNPROCESSABLE_ENTITY, "image_too_large", "The image has too many pixels"),
        ImageError::Invalid => bad(StatusCode::UNPROCESSABLE_ENTITY, "invalid_image", "The image cannot be read"),
        ImageError::BadCrop => bad(StatusCode::UNPROCESSABLE_ENTITY, "invalid_crop", "The icon square must lie inside the image"),
    })?;
    let name = format!("{}.webp", hex::encode(Sha256::digest(&processed.webp)));
    store_file(&state.config.image_dir, &name, &processed.webp).await.map_err(|e| {
        tracing::error!(error = %e, "cannot store image");
        ApiError::internal()
    })?;
    // Old icon out and new entry in together, so a failed insert keeps the old icon.
    let mut tx = state.pool.begin().await?;
    let replaced: Vec<String> = if kind == Kind::Icon {
        sqlx::query_scalar("DELETE FROM images WHERE addon_id = $1 AND kind = 'icon' RETURNING file").bind(addon.id).fetch_all(&mut *tx).await?
    } else {
        Vec::new()
    };
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO images (addon_id, kind, file, width, height, position, caption, uploaded_by, uploaded_at)
         VALUES ($1, $2, $3, $4, $5, (SELECT coalesce(max(position) + 1, 0) FROM images WHERE addon_id = $1), $6, $7, $8) RETURNING id",
    )
    .bind(addon.id)
    .bind(kind.as_str())
    .bind(&name)
    .bind(processed.stored.0 as i32)
    .bind(processed.stored.1 as i32)
    .bind(caption.trim())
    .bind(user.id)
    .bind((state.clock)())
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    for old in replaced {
        remove_unused(&state, &old).await?;
    }
    let mut answer = json!({
        "id": id, "kind": kind.as_str(), "url": format!("{}/{name}", state.config.public_image_url),
        "width": processed.stored.0, "height": processed.stored.1, "caption": caption.trim(),
    });
    if processed.resized() && kind == Kind::Screenshot {
        answer["notice"] = json!("resized");
        answer["original"] = json!({ "width": processed.original.0, "height": processed.original.1 });
        answer["stored"] = json!({ "width": processed.stored.0, "height": processed.stored.1 });
    }
    Ok((StatusCode::CREATED, Json(answer)))
}

#[derive(Deserialize)]
pub struct PatchImage {
    position: Option<i32>,
    caption: Option<String>,
}

pub async fn update(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((slug, id)): Path<(String, i64)>,
    Json(body): Json<PatchImage>,
) -> ApiResult<StatusCode> {
    let addon = editable(&state, &user, &slug).await?;
    if let Some(caption) = &body.caption {
        validate_caption(caption)?;
    }
    let changed = sqlx::query("UPDATE images SET position = coalesce($3, position), caption = coalesce($4, caption) WHERE id = $1 AND addon_id = $2")
        .bind(id)
        .bind(addon.id)
        .bind(body.position)
        .bind(body.caption.as_deref().map(str::trim))
        .execute(&state.pool)
        .await?;
    if changed.rows_affected() == 0 {
        return Err(ApiError::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Deletes the entry; the file goes once no entry uses it any more.
pub async fn delete(State(state): State<AppState>, user: CurrentUser, Path((slug, id)): Path<(String, i64)>) -> ApiResult<StatusCode> {
    let addon = editable(&state, &user, &slug).await?;
    let file: Option<String> = sqlx::query_scalar("DELETE FROM images WHERE id = $1 AND addon_id = $2 RETURNING file")
        .bind(id)
        .bind(addon.id)
        .fetch_optional(&state.pool)
        .await?;
    let file = file.ok_or_else(ApiError::not_found)?;
    remove_unused(&state, &file).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /v1/addons/{slug}/icon`: the addon goes back to showing its initial.
pub async fn delete_icon(State(state): State<AppState>, user: CurrentUser, Path(slug): Path<String>) -> ApiResult<StatusCode> {
    let addon = editable(&state, &user, &slug).await?;
    let files: Vec<String> =
        sqlx::query_scalar("DELETE FROM images WHERE addon_id = $1 AND kind = 'icon' RETURNING file").bind(addon.id).fetch_all(&state.pool).await?;
    if files.is_empty() {
        return Err(ApiError::not_found());
    }
    for file in files {
        remove_unused(&state, &file).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Removes an image file once no entry uses it any more (the same file can serve several).
async fn remove_unused(state: &AppState, file: &str) -> ApiResult<()> {
    let used: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM images WHERE file = $1)").bind(file).fetch_one(&state.pool).await?;
    if !used {
        let _ = tokio::fs::remove_file(PathBuf::from(&state.config.image_dir).join(file)).await;
    }
    Ok(())
}
