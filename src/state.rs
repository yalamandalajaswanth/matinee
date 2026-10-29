//! Shared application state with a swappable library snapshot.
//!
//! Two SQLite connections to the same WAL database: `scan_db` is held for the duration of a
//! rescan, `req_db` serves request-path progress reads/writes, so a scan never blocks them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use serde::Serialize;

use crate::db::{Db, Progress};
use crate::scan::{self, Snapshot};

/// Result of the most recent scan.
#[derive(Debug, Clone, Default)]
struct ScanStatus {
    last_scan: Option<i64>,
    last_error: Option<String>,
}

/// Body of `GET /api/status`.
#[derive(Debug, Serialize)]
pub struct Status {
    pub scanning: bool,
    pub video_count: usize,
    pub last_scan: Option<i64>,
    pub last_error: Option<String>,
}

struct Inner {
    libraries: Vec<PathBuf>,
    /// Held while scanning. Never touched by request handlers.
    scan_db: Mutex<Db>,
    /// Progress reads/writes from request handlers.
    req_db: Mutex<Db>,
    /// Readers clone the inner `Arc` and release the lock immediately, so a rescan
    /// only holds the write lock for a pointer swap.
    snapshot: RwLock<Arc<Snapshot>>,
    scanning: AtomicBool,
    status: Mutex<ScanStatus>,
}

#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

/// Clears the scanning flag on drop, so it resets even on error or panic.
struct ScanGuard(AppState);

impl Drop for ScanGuard {
    fn drop(&mut self) {
        self.0.inner.scanning.store(false, Ordering::SeqCst);
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

impl AppState {
    /// Create the state with an empty snapshot; call [`AppState::rescan`] to populate it.
    pub fn new(libraries: Vec<PathBuf>, scan_db: Db, req_db: Db) -> Self {
        Self {
            inner: Arc::new(Inner {
                libraries,
                scan_db: Mutex::new(scan_db),
                req_db: Mutex::new(req_db),
                snapshot: RwLock::new(Arc::new(Snapshot::default())),
                scanning: AtomicBool::new(false),
                status: Mutex::new(ScanStatus::default()),
            }),
        }
    }

    /// Open two connections to the database file at `path`.
    pub fn open(libraries: Vec<PathBuf>, path: &Path) -> Result<Self> {
        Ok(Self::new(libraries, Db::open(path)?, Db::open(path)?))
    }

    /// The current snapshot. Cheap, never blocks on a running scan.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        match self.inner.snapshot.read() {
            Ok(g) => Arc::clone(&g),
            Err(p) => Arc::clone(&p.into_inner()),
        }
    }

    fn swap(&self, snap: Snapshot) {
        let new = Arc::new(snap);
        match self.inner.snapshot.write() {
            Ok(mut g) => *g = new,
            Err(p) => *p.into_inner() = new,
        }
    }

    fn begin_scan(&self) -> Option<ScanGuard> {
        self.inner
            .scanning
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| ScanGuard(self.clone()))
    }

    fn run_scan(&self, _guard: ScanGuard) -> Result<usize> {
        let result = {
            let db = lock(&self.inner.scan_db);
            scan::scan(&db, &self.inner.libraries)
        };
        let mut status = lock(&self.inner.status);
        match result {
            Ok(snap) => {
                let n = snap.len();
                self.swap(snap);
                status.last_scan = Some(now_secs());
                status.last_error = None;
                Ok(n)
            }
            Err(e) => {
                tracing::error!("scan failed: {e:#}");
                status.last_error = Some(format!("{e:#}"));
                Err(e)
            }
        }
    }

    /// Scan synchronously (blocking) and swap in the new snapshot. Returns the video count.
    /// Fails if a scan is already running.
    pub fn rescan_blocking(&self) -> Result<usize> {
        let guard = self
            .begin_scan()
            .ok_or_else(|| anyhow!("a scan is already running"))?;
        self.run_scan(guard)
    }

    /// Scan on a blocking thread so the async runtime stays responsive.
    pub async fn rescan(&self) -> Result<usize> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.rescan_blocking())
            .await
            .map_err(|e| anyhow!("scan task failed: {e}"))?
    }

    /// Start a background scan unless one is already running. Returns whether one was started.
    pub fn trigger_rescan(&self) -> bool {
        let Some(guard) = self.begin_scan() else {
            return false;
        };
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            if let Ok(n) = this.run_scan(guard) {
                tracing::info!("rescan complete: {n} videos");
            }
        });
        true
    }

    pub fn status(&self) -> Status {
        let s = lock(&self.inner.status).clone();
        Status {
            scanning: self.inner.scanning.load(Ordering::SeqCst),
            video_count: self.snapshot().len(),
            last_scan: s.last_scan,
            last_error: s.last_error,
        }
    }

    pub fn library_count(&self) -> usize {
        self.inner.libraries.len()
    }

    /// Hold the scan connection's lock, as a running scan does. Lets tests prove that
    /// request-path database access does not depend on it.
    #[doc(hidden)]
    pub fn lock_scan_db_for_test(&self) -> MutexGuard<'_, Db> {
        lock(&self.inner.scan_db)
    }

    /// Run `f` with the request-path database connection.
    pub fn with_req_db<T>(&self, f: impl FnOnce(&Db) -> Result<T>) -> Result<T> {
        f(&lock(&self.inner.req_db))
    }

    pub fn all_progress(&self) -> Result<HashMap<i64, Progress>> {
        self.with_req_db(Db::all_progress)
    }
}
