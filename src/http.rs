use std::path::Path as FsPath;

use axum::Json;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::AppState;
use crate::admin;
use crate::auth;
use crate::error::{AppError, ErrorCode, human_bytes};
use crate::jobs::JobState;
use crate::level::Level;
use crate::storage::remove_if_exists;

const INDEX_HTML: &str = include_str!("../web/index.html");
const ADMIN_HTML: &str = include_str!("../web/admin.html");
const APP_JS: &str = include_str!("../web/app.js");
const ADMIN_JS: &str = include_str!("../web/admin.js");
const APP_CSS: &str = include_str!("../web/app.css");

pub fn router(app: AppState) -> Router {
    let user_api = Router::new()
        .route("/config", get(config))
        .route("/jobs", post(create_job).layer(DefaultBodyLimit::disable()))
        .route("/jobs/{id}", get(job_status))
        .route("/jobs/{id}/download", get(download))
        .route_layer(middleware::from_fn_with_state(app.clone(), auth::require_user));

    let admin_api = Router::new()
        .route("/settings", get(admin::get_settings).put(admin::put_settings))
        .route("/restart", post(admin::restart))
        .route_layer(middleware::from_fn_with_state(app.clone(), auth::require_admin));

    const HTML: &str = "text/html; charset=utf-8";
    const JS: &str = "text/javascript; charset=utf-8";
    Router::new()
        .route("/", get(|| async { static_file(INDEX_HTML, HTML) }))
        .route("/admin", get(|| async { static_file(ADMIN_HTML, HTML) }))
        .route("/app.js", get(|| async { static_file(APP_JS, JS) }))
        .route("/admin.js", get(|| async { static_file(ADMIN_JS, JS) }))
        .route("/app.css", get(|| async { static_file(APP_CSS, "text/css; charset=utf-8") }))
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/login", post(user_login))
        .route("/api/logout", post(|| async { logout(auth::USER_COOKIE) }))
        .route("/api/admin/login", post(admin_login))
        .route("/api/admin/logout", post(|| async { logout(auth::ADMIN_COOKIE) }))
        .nest("/api/admin", admin_api)
        .nest("/api", user_api)
        .fallback(|| async { AppError::new(ErrorCode::NotFound, "Not found.") })
        .layer(middleware::from_fn(security_headers))
        .with_state(app)
}

fn static_file(body: &'static str, content_type: &'static str) -> Response {
    ([(header::CONTENT_TYPE, content_type), (header::CACHE_CONTROL, "no-cache")], body).into_response()
}

/// The browser may only load resources from this server: nothing reaches third parties.
async fn security_headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; connect-src 'self'; frame-ancestors 'none'; form-action 'self'; base-uri 'none'",
        ),
    );
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    res
}

#[derive(Deserialize)]
struct LoginBody {
    token: String,
}

async fn user_login(State(app): State<AppState>, body: Bytes) -> Result<Response, AppError> {
    login(&body, &app.config.auth_token, auth::USER_COOKIE, "That access token is wrong.")
}

async fn admin_login(State(app): State<AppState>, body: Bytes) -> Result<Response, AppError> {
    login(&body, &app.config.admin_token, auth::ADMIN_COOKIE, "That admin token is wrong.")
}

fn login(body: &[u8], expected: &str, cookie: &str, wrong: &str) -> Result<Response, AppError> {
    let body: LoginBody = serde_json::from_slice(body)
        .map_err(|_| AppError::new(ErrorCode::InvalidRequest, "Expected JSON like {\"token\": \"...\"}."))?;
    if !auth::tokens_match(body.token.trim(), expected) {
        return Err(AppError::new(ErrorCode::Unauthorized, wrong));
    }
    let mut res = StatusCode::NO_CONTENT.into_response();
    res.headers_mut().insert(header::SET_COOKIE, auth::session_cookie(cookie, expected));
    Ok(res)
}

fn logout(cookie: &str) -> Response {
    let mut res = StatusCode::NO_CONTENT.into_response();
    res.headers_mut().insert(header::SET_COOKIE, auth::clear_cookie(cookie));
    res
}

async fn config(State(app): State<AppState>) -> Json<serde_json::Value> {
    let s = &app.settings;
    Json(json!({
        "max_upload_bytes": s.max_upload_bytes(),
        "delete_originals_after_job": s.delete_originals_after_job,
        "original_retention_minutes": s.original_retention_minutes,
        "output_retention_minutes": s.output_retention_minutes,
        "delete_after_download": s.delete_after_download,
        "strip_metadata": s.strip_metadata,
    }))
}

#[derive(Deserialize)]
struct UploadParams {
    name: Option<String>,
    level: Option<String>,
}

/// Upload one file as the raw request body: `POST /api/jobs?name=<file name>&level=<level>`.
async fn create_job(
    State(app): State<AppState>,
    Query(params): Query<UploadParams>,
    headers: HeaderMap,
    body: Body,
) -> Result<(StatusCode, Json<crate::jobs::Job>), AppError> {
    let file_name = clean_file_name(params.name.as_deref().unwrap_or(""))?;
    let level: Level = params
        .level
        .as_deref()
        .ok_or_else(|| AppError::new(ErrorCode::InvalidLevel, "Choose a compression level."))?
        .parse()?;

    let max = app.settings.max_upload_bytes();
    if let Some(len) = headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()?.parse::<u64>().ok())
        && len > max
    {
        return Err(too_large(Some(len), max));
    }

    let id = Uuid::new_v4();
    let path = app.storage.upload_path(id);
    let size = match save_body(body, &path, max).await {
        Ok(size) => size,
        Err(e) => {
            discard(&path).await;
            return Err(e);
        }
    };
    if size == 0 {
        discard(&path).await;
        return Err(AppError::new(ErrorCode::EmptyFile, "This file is empty, so there's nothing to compress."));
    }

    tracing::info!(job = %id, size, "upload received");
    let job = app.jobs.submit(id, file_name, level, size);
    Ok((StatusCode::ACCEPTED, Json(job)))
}

async fn job_status(State(app): State<AppState>, Path(id): Path<Uuid>) -> Result<Json<crate::jobs::Job>, AppError> {
    app.jobs.get(id).map(Json).ok_or_else(gone)
}

async fn download(State(app): State<AppState>, Path(id): Path<Uuid>) -> Result<Response, AppError> {
    let job = app.jobs.get(id).ok_or_else(gone)?;
    match job.state {
        JobState::Done { .. } => {}
        JobState::Queued | JobState::Running { .. } => {
            return Err(AppError::new(ErrorCode::NotReady, "Still compressing. Try again when it's done."));
        }
        JobState::Failed { .. } => {
            return Err(AppError::new(ErrorCode::NotReady, "This file failed to compress, so there's nothing to download."));
        }
    }
    let file = tokio::fs::File::open(app.storage.output_path(id)).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound { gone() } else { AppError::io(&e) }
    })?;
    let len = file.metadata().await.map_err(|e| AppError::io(&e))?.len();
    let name = job.download_name.expect("finished job always has a download name");

    let stream = ReaderStream::new(file);
    let body = if app.settings.delete_after_download {
        // Delete once the last byte has been read for sending. hyper stops polling the body
        // after Content-Length bytes, so an end-of-stream hook would never run.
        // An aborted download never reaches `len`, so the file is kept.
        let jobs = std::sync::Arc::clone(&app.jobs);
        let mut sent = 0u64;
        Body::from_stream(stream.map(move |chunk| {
            if let Ok(bytes) = &chunk {
                sent += bytes.len() as u64;
                if sent == len {
                    tracing::info!(job = %id, "downloaded; deleting (delete after download is on)");
                    jobs.forget(id);
                }
            }
            chunk
        }))
    } else {
        Body::from_stream(stream)
    };

    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream")),
            (header::CONTENT_LENGTH, HeaderValue::from(len)),
            (header::CONTENT_DISPOSITION, content_disposition(&name)),
        ],
        body,
    )
        .into_response())
}

/// Streams the request body to disk, enforcing the size limit while reading.
async fn save_body(body: Body, path: &FsPath, max: u64) -> Result<u64, AppError> {
    let mut file = tokio::fs::File::create(path).await.map_err(|e| AppError::io(&e))?;
    let mut stream = body.into_data_stream();
    let mut size: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| {
            AppError::new(ErrorCode::UploadInterrupted, "The upload was interrupted. Check your connection and try again.")
        })?;
        size += chunk.len() as u64;
        if size > max {
            return Err(too_large(None, max));
        }
        file.write_all(&chunk).await.map_err(|e| AppError::io(&e))?;
    }
    file.flush().await.map_err(|e| AppError::io(&e))?;
    Ok(size)
}

async fn discard(path: &FsPath) {
    let path = path.to_owned();
    if let Ok(Err(e)) = tokio::task::spawn_blocking(move || remove_if_exists(&path)).await {
        tracing::error!("cannot delete rejected upload: {e}");
    }
}

fn too_large(size: Option<u64>, max: u64) -> AppError {
    let message = match size {
        Some(size) => format!("This file is {} and the limit on this server is {}.", human_bytes(size), human_bytes(max)),
        None => format!("This file is larger than the {} limit on this server.", human_bytes(max)),
    };
    AppError::new(ErrorCode::FileTooLarge, message)
}

fn gone() -> AppError {
    AppError::new(ErrorCode::NotFound, "This file was deleted by the server's retention policy, or never existed.")
}

/// Keeps only the base name and rejects names that can't be shown or saved safely.
fn clean_file_name(raw: &str) -> Result<String, AppError> {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("").trim();
    if base.is_empty() || base == "." || base == ".." {
        return Err(AppError::new(ErrorCode::MissingFilename, "The upload is missing a file name."));
    }
    if base.chars().any(char::is_control) || base.len() > 255 {
        return Err(AppError::new(
            ErrorCode::MissingFilename,
            "The file name is too long or contains invalid characters. Rename the file and try again.",
        ));
    }
    Ok(base.to_string())
}

/// `attachment` header with an ASCII fallback and the exact UTF-8 name (RFC 6266 / 5987).
fn content_disposition(name: &str) -> HeaderValue {
    let ascii: String = name
        .chars()
        .map(|c| if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' { c } else { '_' })
        .collect();
    let encoded: String = name
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }
        })
        .collect();
    HeaderValue::from_str(&format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}"))
        .expect("header value contains only visible ASCII")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_paths_from_file_names() {
        assert_eq!(clean_file_name("../../etc/passwd").unwrap(), "passwd");
        assert_eq!(clean_file_name("C:\\Users\\me\\report.txt").unwrap(), "report.txt");
        assert!(clean_file_name("..").is_err());
        assert!(clean_file_name("").is_err());
        assert!(clean_file_name("bad\nname.txt").is_err());
    }

    #[test]
    fn encodes_download_names() {
        let value = content_disposition("résumé \"v2\".txt.zst");
        assert_eq!(
            value.to_str().unwrap(),
            "attachment; filename=\"r_sum_ _v2_.txt.zst\"; filename*=UTF-8''r%C3%A9sum%C3%A9%20%22v2%22.txt.zst"
        );
    }
}
