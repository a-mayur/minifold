use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use uuid::Uuid;

use crate::error::human_bytes;

/// Keep this much free space per allowed upload: original + output + working room.
pub const FREE_SPACE_FACTOR: f64 = 2.5;

/// Files on disk are named by job id only, never by the user's file name.
#[derive(Clone)]
pub struct Storage {
    root: PathBuf,
    uploads: PathBuf,
    outputs: PathBuf,
}

impl Storage {
    /// Creates the data folders, wipes leftovers from a previous run and checks free space.
    pub fn prepare(data_dir: &Path, max_upload_bytes: u64) -> Result<Self, String> {
        let storage = Self {
            root: data_dir.to_path_buf(),
            uploads: data_dir.join("uploads"),
            outputs: data_dir.join("outputs"),
        };
        for dir in [&storage.uploads, &storage.outputs] {
            std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
            let removed = wipe_dir(dir)?;
            if removed > 0 {
                tracing::info!("removed {removed} leftover file(s) from {}", dir.display());
            }
        }
        check_writable(&storage.uploads)?;
        let available = storage.free_space().map_err(|e| format!("cannot read free space of {}: {e}", data_dir.display()))?;
        if let Some(problem) = space_problem(available, max_upload_bytes) {
            return Err(format!(
                "{problem} in {}. Free up space or lower max_upload_gb in settings.toml.",
                data_dir.display()
            ));
        }
        Ok(storage)
    }

    pub fn upload_path(&self, id: Uuid) -> PathBuf {
        self.uploads.join(id.to_string())
    }

    pub fn output_path(&self, id: Uuid) -> PathBuf {
        self.outputs.join(id.to_string())
    }

    pub fn free_space(&self) -> std::io::Result<u64> {
        fs4::available_space(&self.root)
    }

    /// Deletes files that belong to no known job and haven't been written to for `max_age`
    /// (e.g. an upload that stalled). Returns how many were deleted.
    pub fn remove_orphans(&self, known: &HashSet<Uuid>, max_age: Duration) -> usize {
        let now = SystemTime::now();
        let mut removed = 0;
        for dir in [&self.uploads, &self.outputs] {
            let Ok(entries) = std::fs::read_dir(dir) else {
                tracing::error!("cannot read {}", dir.display());
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let owned = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| Uuid::parse_str(n).ok())
                    .is_some_and(|id| known.contains(&id));
                let old = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .is_ok_and(|t| now.duration_since(t).unwrap_or_default() >= max_age);
                if !owned && old {
                    match remove_if_exists(&path) {
                        Ok(()) => removed += 1,
                        Err(e) => tracing::error!("cannot delete orphan file: {e}"),
                    }
                }
            }
        }
        removed
    }
}

/// `Some(reason)` when there isn't enough free space for the given upload limit.
pub fn space_problem(available: u64, max_upload: u64) -> Option<String> {
    let required = (max_upload as f64 * FREE_SPACE_FACTOR) as u64;
    (available < required).then(|| {
        format!(
            "Not enough free disk space: {} free, {} needed (2.5x the {} upload limit)",
            human_bytes(available),
            human_bytes(required),
            human_bytes(max_upload)
        )
    })
}

/// Removes a file, treating "already gone" as success.
pub fn remove_if_exists(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn wipe_dir(dir: &Path) -> Result<usize, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    let mut removed = 0;
    for entry in entries {
        let path = entry.map_err(|e| format!("cannot read {}: {e}", dir.display()))?.path();
        std::fs::remove_file(&path).map_err(|e| format!("cannot remove leftover {}: {e}", path.display()))?;
        removed += 1;
    }
    Ok(removed)
}

fn check_writable(dir: &Path) -> Result<(), String> {
    let probe = dir.join(".write-test");
    std::fs::write(&probe, b"ok").map_err(|e| format!("{} is not writable: {e}", dir.display()))?;
    std::fs::remove_file(&probe).map_err(|e| format!("cannot remove {}: {e}", probe.display()))
}
