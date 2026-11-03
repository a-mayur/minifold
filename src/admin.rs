use std::time::Duration;

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;

use crate::AppState;
use crate::error::{AppError, ErrorCode};
use crate::settings::{self, HwAccel, Settings};
use crate::storage::space_problem;

#[derive(Serialize)]
pub struct AdminView {
    saved: Settings,
    active: Settings,
    restart_required: bool,
    jobs_in_progress: usize,
    /// Hardware video encoders found at startup (the admin page only offers these).
    hw_detected: Vec<HwAccel>,
    /// The active setting names an accelerator that wasn't found at startup.
    hw_missing: Option<HwAccel>,
    system: System,
}

#[derive(Serialize)]
struct System {
    version: &'static str,
    arch: &'static str,
    cpus: usize,
    data_free_bytes: u64,
}

pub async fn get_settings(State(app): State<AppState>) -> Result<Json<AdminView>, AppError> {
    view(&app).map(Json)
}

/// Validates and saves settings. They take effect after a restart.
pub async fn put_settings(State(app): State<AppState>, body: Bytes) -> Result<Json<AdminView>, AppError> {
    let new: Settings = serde_json::from_slice(&body)
        .map_err(|e| AppError::new(ErrorCode::InvalidSetting, format!("Settings are malformed: {e}")))?;
    new.validate().map_err(|e| AppError::new(ErrorCode::InvalidSetting, e.message))?;
    if new.hw_accel != HwAccel::Off && !app.hw_detected.contains(&new.hw_accel) {
        return Err(AppError::new(
            ErrorCode::InvalidSetting,
            format!("{} wasn't detected on this server, so it can't be selected.", new.hw_accel.label()),
        ));
    }

    // Refuse a limit the next startup would reject, so the admin can't lock themselves out.
    let free = app.storage.free_space().map_err(|e| AppError::io(&e))?;
    if let Some(problem) = space_problem(free, new.max_upload_bytes()) {
        return Err(AppError::new(ErrorCode::InvalidSetting, format!("{problem}. Choose a smaller maximum file size.")));
    }

    let path = app.config.settings_path();
    tokio::task::spawn_blocking({
        let new = new.clone();
        move || settings::save(&path, &new)
    })
    .await
    .expect("settings writer panicked")
    .map_err(|e| AppError::io(&e))?;

    tracing::info!(settings = ?new, "settings saved by admin; restart required");
    *app.saved_settings.lock().expect("settings lock poisoned") = new;
    view(&app).map(Json)
}

pub async fn restart(State(app): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    // Let this response reach the browser before the server stops.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        app.shutdown.notify_one();
    });
    (StatusCode::ACCEPTED, Json(serde_json::json!({ "message": "Restarting." })))
}

fn view(app: &AppState) -> Result<AdminView, AppError> {
    let saved = app.saved_settings.lock().expect("settings lock poisoned").clone();
    Ok(AdminView {
        restart_required: saved != app.settings,
        saved,
        active: app.settings.clone(),
        jobs_in_progress: app.jobs.in_progress(),
        hw_detected: app.hw_detected.clone(),
        hw_missing: Some(app.settings.hw_accel).filter(|a| *a != HwAccel::Off && !app.hw_detected.contains(a)),
        system: System {
            version: env!("CARGO_PKG_VERSION"),
            arch: std::env::consts::ARCH,
            cpus: std::thread::available_parallelism().map_or(1, usize::from),
            data_free_bytes: app.storage.free_space().map_err(|e| AppError::io(&e))?,
        },
    })
}
