use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::engines;
use crate::error::{AppError, ErrorCode};
use crate::level::Level;
use crate::storage::{Storage, remove_if_exists};

#[derive(Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum JobState {
    Queued,
    /// `progress` is 0–100 for videos and 0 for everything else.
    Running { progress: u8 },
    Done { output_size: u64, no_gain: bool, note: String },
    Failed { error: AppError },
}

#[derive(Clone, Serialize)]
pub struct Job {
    pub id: Uuid,
    pub file_name: String,
    pub level: Level,
    pub original_size: u64,
    #[serde(flatten)]
    pub state: JobState,
    #[serde(skip)]
    pub download_name: Option<String>,
    #[serde(skip)]
    uploaded_at: Instant,
    #[serde(skip)]
    finished_at: Option<Instant>,
    #[serde(skip)]
    progress: Arc<AtomicU8>,
}

/// Retention rules, fixed at startup from the settings.
pub struct Policy {
    pub delete_originals_after_job: bool,
    pub original_retention: Duration,
    pub output_retention: Duration,
}

pub struct Jobs {
    map: Mutex<HashMap<Uuid, Job>>,
    slots: Arc<Semaphore>,
    storage: Storage,
    policy: Policy,
    strip_metadata: bool,
    hw: engines::Hw,
}

impl Jobs {
    pub fn new(workers: usize, storage: Storage, policy: Policy, strip_metadata: bool, hw: engines::Hw) -> Self {
        Self { map: Mutex::new(HashMap::new()), slots: Arc::new(Semaphore::new(workers)), storage, policy, strip_metadata, hw }
    }

    pub fn get(&self, id: Uuid) -> Option<Job> {
        let mut job = self.lock().get(&id).cloned()?;
        if let JobState::Running { progress } = &mut job.state {
            *progress = job.progress.load(Ordering::Relaxed);
        }
        Some(job)
    }

    /// Jobs that are queued or running (they are cancelled by a restart).
    pub fn in_progress(&self) -> usize {
        self.lock()
            .values()
            .filter(|j| matches!(j.state, JobState::Queued | JobState::Running { .. }))
            .count()
    }

    /// Registers an uploaded file and queues it for compression.
    pub fn submit(self: &Arc<Self>, id: Uuid, file_name: String, level: Level, original_size: u64) -> Job {
        let job = Job {
            id,
            file_name,
            level,
            original_size,
            state: JobState::Queued,
            download_name: None,
            uploaded_at: Instant::now(),
            finished_at: None,
            progress: Arc::new(AtomicU8::new(0)),
        };
        self.lock().insert(id, job.clone());
        tokio::spawn(Arc::clone(self).run(id, level));
        job
    }

    /// Deletes a job and its files (used after a one-time download).
    pub fn forget(&self, id: Uuid) {
        self.lock().remove(&id);
        self.delete_files(id);
    }

    /// Applies the retention policy. Blocking: call from `spawn_blocking`. Returns files deleted.
    pub fn sweep(&self) -> usize {
        let now = Instant::now();
        let mut expired = Vec::new();
        let mut originals = Vec::new();
        let known: HashSet<Uuid> = {
            let mut map = self.lock();
            map.retain(|id, job| {
                if !self.policy.delete_originals_after_job
                    && now.duration_since(job.uploaded_at) >= self.policy.original_retention
                {
                    originals.push(*id);
                }
                let keep = job.finished_at.is_none_or(|t| now.duration_since(t) < self.policy.output_retention);
                if !keep {
                    expired.push(*id);
                }
                keep
            });
            map.keys().copied().collect()
        };

        let mut removed = 0;
        for id in originals {
            removed += usize::from(self.delete(&self.storage.upload_path(id)));
        }
        for id in expired {
            tracing::info!(job = %id, "retention expired");
            removed += self.delete_files(id);
        }
        removed + self.storage.remove_orphans(&known, self.policy.output_retention)
    }

    async fn run(self: Arc<Self>, id: Uuid, level: Level) {
        let _slot = Arc::clone(&self.slots).acquire_owned().await.expect("job semaphore closed");
        if !self.update(id, |job| job.state = JobState::Running { progress: 0 }) {
            return; // deleted while queued
        }
        tracing::info!(job = %id, ?level, "compression started");

        let input = self.storage.upload_path(id);
        let output = self.storage.output_path(id);
        let (input_c, output_c) = (input.clone(), output.clone());
        let opts = engines::Options { level, strip_metadata: self.strip_metadata };
        let (hw, progress) = (self.hw, self.get(id).map(|j| j.progress).unwrap_or_default());
        let result = tokio::task::spawn_blocking(move || engines::process(&input_c, &output_c, opts, hw, &progress))
            .await
            .unwrap_or_else(|panic| {
                tracing::error!(job = %id, "engine panicked: {panic}");
                Err(AppError::new(ErrorCode::EngineFailed, "Compression crashed unexpectedly."))
            });

        if self.policy.delete_originals_after_job {
            self.delete(&input);
        }

        let finished_at = Some(Instant::now());
        match result {
            Ok(outcome) => {
                tracing::info!(job = %id, size = outcome.output_size, no_gain = outcome.no_gain, "compression done");
                self.update(id, |job| {
                    job.download_name = Some(outcome.rename.apply(&job.file_name));
                    job.state = JobState::Done { output_size: outcome.output_size, no_gain: outcome.no_gain, note: outcome.note };
                    job.finished_at = finished_at;
                });
            }
            Err(error) => {
                tracing::warn!(job = %id, code = ?error.code, "compression failed");
                self.delete(&output);
                self.update(id, |job| {
                    job.state = JobState::Failed { error };
                    job.finished_at = finished_at;
                });
            }
        }
    }

    /// Returns false when the job no longer exists.
    fn update(&self, id: Uuid, change: impl FnOnce(&mut Job)) -> bool {
        self.lock().get_mut(&id).map(change).is_some()
    }

    fn delete_files(&self, id: Uuid) -> usize {
        usize::from(self.delete(&self.storage.upload_path(id))) + usize::from(self.delete(&self.storage.output_path(id)))
    }

    /// Returns true when a file was actually removed.
    fn delete(&self, path: &std::path::Path) -> bool {
        if !path.exists() {
            return false;
        }
        match remove_if_exists(path) {
            Ok(()) => true,
            Err(e) => {
                tracing::error!("cannot delete {}: {e}", path.display());
                false
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, Job>> {
        self.map.lock().expect("jobs lock poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(name: &str, policy: Policy) -> (Arc<Jobs>, Storage) {
        let dir = std::env::temp_dir().join(format!("minifold-jobs-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let storage = Storage::prepare(&dir, 1).unwrap();
        (Arc::new(Jobs::new(1, storage.clone(), policy, true, engines::Hw::Off)), storage)
    }

    async fn run_job(jobs: &Arc<Jobs>, storage: &Storage) -> Uuid {
        let id = Uuid::new_v4();
        std::fs::write(storage.upload_path(id), "line\n".repeat(5000)).unwrap();
        jobs.submit(id, "a.log".into(), Level::Low, 25_000);
        while !matches!(jobs.get(id).unwrap().state, JobState::Done { .. }) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        id
    }

    #[tokio::test]
    async fn expired_outputs_and_jobs_are_deleted() {
        let policy = Policy {
            delete_originals_after_job: true,
            original_retention: Duration::from_secs(3600),
            output_retention: Duration::from_millis(100),
        };
        let (jobs, storage) = setup("expire", policy);
        let id = run_job(&jobs, &storage).await;
        assert!(!storage.upload_path(id).exists(), "original deleted right after the job");
        assert!(storage.output_path(id).exists());

        assert_eq!(jobs.sweep(), 0, "not expired yet");
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(jobs.sweep(), 1);
        assert!(!storage.output_path(id).exists());
        assert!(jobs.get(id).is_none());
    }

    #[tokio::test]
    async fn kept_originals_expire_on_their_own_timer() {
        let policy = Policy {
            delete_originals_after_job: false,
            original_retention: Duration::from_millis(100),
            output_retention: Duration::from_secs(3600),
        };
        let (jobs, storage) = setup("originals", policy);
        let id = run_job(&jobs, &storage).await;
        assert!(storage.upload_path(id).exists(), "original kept after the job");

        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(jobs.sweep(), 1);
        assert!(!storage.upload_path(id).exists());
        assert!(storage.output_path(id).exists(), "output has its own, longer retention");
    }

    #[tokio::test]
    async fn orphan_files_are_deleted() {
        let policy = Policy {
            delete_originals_after_job: true,
            original_retention: Duration::from_secs(3600),
            output_retention: Duration::from_millis(50),
        };
        let (jobs, storage) = setup("orphans", policy);
        let stray = storage.upload_path(Uuid::new_v4());
        std::fs::write(&stray, b"stalled upload").unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(jobs.sweep(), 1);
        assert!(!stray.exists());
    }

    #[tokio::test]
    async fn forget_deletes_files_immediately() {
        let policy = Policy {
            delete_originals_after_job: true,
            original_retention: Duration::from_secs(3600),
            output_retention: Duration::from_secs(3600),
        };
        let (jobs, storage) = setup("forget", policy);
        let id = run_job(&jobs, &storage).await;
        jobs.forget(id);
        assert!(!storage.output_path(id).exists());
        assert!(jobs.get(id).is_none());
    }
}
