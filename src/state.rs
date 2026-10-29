//! Shared application state with a swappable library snapshot.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{anyhow, Result};

use crate::db::Db;
use crate::scan::{self, Snapshot};

struct Inner {
    libraries: Vec<PathBuf>,
    db: Mutex<Db>,
    /// Readers clone the inner `Arc` and release the lock immediately, so a rescan
    /// only holds the write lock for a pointer swap.
    snapshot: RwLock<Arc<Snapshot>>,
}

#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

impl AppState {
    /// Create the state with an empty snapshot; call [`AppState::rescan`] to populate it.
    pub fn new(libraries: Vec<PathBuf>, db: Db) -> Self {
        Self {
            inner: Arc::new(Inner {
                libraries,
                db: Mutex::new(db),
                snapshot: RwLock::new(Arc::new(Snapshot::default())),
            }),
        }
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

    /// Scan synchronously (blocking) and swap in the new snapshot. Returns the video count.
    pub fn rescan_blocking(&self) -> Result<usize> {
        let snap = {
            let db = self.inner.db.lock().unwrap_or_else(|p| p.into_inner());
            scan::scan(&db, &self.inner.libraries)?
        };
        let n = snap.len();
        self.swap(snap);
        Ok(n)
    }

    /// Scan on a blocking thread so the async runtime stays responsive.
    pub async fn rescan(&self) -> Result<usize> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.rescan_blocking())
            .await
            .map_err(|e| anyhow!("scan task failed: {e}"))?
    }

    pub fn library_count(&self) -> usize {
        self.inner.libraries.len()
    }
}
