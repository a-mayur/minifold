mod admin;
mod auth;
mod config;
mod engines;
mod error;
mod http;
mod jobs;
mod level;
mod settings;
mod storage;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use config::Config;
use jobs::{Jobs, Policy};
use settings::Settings;
use storage::Storage;
use tokio::sync::Notify;

const CLEANUP_EVERY: Duration = Duration::from_secs(60);
/// After a stop/restart request, open connections get this long to finish.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

pub struct App {
    pub config: Config,
    /// Settings in effect since startup.
    pub settings: Settings,
    /// Settings saved by the admin; they take effect after a restart.
    pub saved_settings: Mutex<Settings>,
    pub storage: Storage,
    pub jobs: Arc<Jobs>,
    /// Hardware video encoders that passed a test encode at startup.
    pub hw_detected: Vec<settings::HwAccel>,
    pub shutdown: Arc<Notify>,
}

pub type AppState = Arc<App>;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    if let Err(message) = run().await {
        tracing::error!("startup failed: {message}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let config = Config::from_env()?;
    engines::check_tools()?;
    let settings = settings::load_or_create(&config.settings_path())?;
    let storage = Storage::prepare(&config.data_dir, settings.max_upload_bytes())?;
    let policy = Policy {
        delete_originals_after_job: settings.delete_originals_after_job,
        original_retention: settings.original_retention(),
        output_retention: settings.output_retention(),
    };
    let hw_detected = engines::detect_hw();
    let hw = match settings.hw_accel {
        settings::HwAccel::Off => engines::Hw::Off,
        a if hw_detected.contains(&a) => engines::Hw::Use(a),
        a => {
            tracing::warn!("{} is selected but wasn't detected; video jobs will fail until the setting is changed", a.label());
            engines::Hw::Missing(a)
        }
    };
    let jobs = Arc::new(Jobs::new(settings.workers, storage.clone(), policy, settings.strip_metadata, hw));
    tracing::info!(?settings, "settings loaded");

    let bind = config.bind;
    let shutdown = Arc::new(Notify::new());
    let app: AppState = Arc::new(App {
        config,
        saved_settings: Mutex::new(settings.clone()),
        settings,
        storage,
        jobs: Arc::clone(&jobs),
        hw_detected,
        shutdown: Arc::clone(&shutdown),
    });

    tokio::spawn(cleanup_loop(jobs));

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| format!("cannot listen on {bind}: {e}"))?;
    tracing::info!("Minifold {} listening on {bind}", env!("CARGO_PKG_VERSION"));

    axum::serve(listener, http::router(app))
        .with_graceful_shutdown(shutdown_signal(shutdown))
        .await
        .map_err(|e| format!("server error: {e}"))
}

async fn cleanup_loop(jobs: Arc<Jobs>) {
    let mut tick = tokio::time::interval(CLEANUP_EVERY);
    loop {
        tick.tick().await;
        let jobs = Arc::clone(&jobs);
        match tokio::task::spawn_blocking(move || jobs.sweep()).await {
            Ok(0) => {}
            Ok(removed) => tracing::info!(removed, "retention cleanup deleted files"),
            Err(e) => tracing::error!("retention cleanup crashed: {e}"),
        }
    }
}

/// Resolves on Ctrl-C, SIGTERM (docker stop) or an admin restart request.
async fn shutdown_signal(restart: Arc<Notify>) {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("cannot install SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => tracing::info!("shutting down (Ctrl-C)"),
        _ = term.recv() => tracing::info!("shutting down (SIGTERM)"),
        _ = restart.notified() => tracing::info!("restart requested by admin; exiting so Docker restarts the container"),
    }
    tokio::spawn(async {
        tokio::time::sleep(SHUTDOWN_GRACE).await;
        tracing::warn!("connections still open after {}s; exiting anyway", SHUTDOWN_GRACE.as_secs());
        std::process::exit(0);
    });
}
