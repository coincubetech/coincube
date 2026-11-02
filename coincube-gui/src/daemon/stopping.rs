//! Stopping embedded daemons off the UI thread.
//!
//! `Daemon::stop` joins the daemon's poller, which first finishes the network
//! request it has in flight. On a slow or freshly woken network that kept the
//! UI thread blocked long enough for macOS to show the busy cursor when a Cube
//! was locked. The lock paths hand the daemon to [`stop_in_background`]
//! instead. Until that stop returns the daemon still holds its data directory
//! (`daemon.lock` and the database), so whatever next starts or deletes that
//! directory calls [`wait_for`] first, and the process calls [`wait_for_all`]
//! before it exits.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tracing::{error, info, warn};

use super::Daemon;

/// How long a start or delete waits for a stopping daemon before going ahead
/// anyway. Past it the daemon's own `daemon.lock` refuses a second start with
/// a clear error, which beats an endless spinner.
const WAIT_LIMIT: Duration = Duration::from_secs(120);

/// Data directories whose daemon is still stopping, one entry per daemon.
/// `None` is a daemon without a known directory; only [`wait_for_all`] waits
/// for it.
static STOPPING: OnceLock<(Mutex<Vec<Option<PathBuf>>>, Condvar)> = OnceLock::new();

fn stopping() -> &'static (Mutex<Vec<Option<PathBuf>>>, Condvar) {
    STOPPING.get_or_init(|| (Mutex::new(Vec::new()), Condvar::new()))
}

/// Stop `daemon` on its own thread, then run `then`, and return at once.
///
/// `then` keeps the blocking path's order for work that must follow the
/// daemon, such as releasing a managed node the daemon polls.
pub fn stop_in_background(
    daemon: Arc<dyn Daemon + Sync + Send>,
    then: impl FnOnce() + Send + 'static,
) {
    let dir = daemon
        .config()
        .and_then(|config| config.data_directory())
        .map(|dir| dir.path().to_path_buf());
    let runtime = tokio::runtime::Handle::try_current().ok();
    run_in_background(dir, move || {
        let result = match runtime {
            Some(runtime) => runtime.block_on(daemon.stop()).map_err(|e| e.to_string()),
            None => tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())
                .and_then(|runtime| runtime.block_on(daemon.stop()).map_err(|e| e.to_string())),
        };
        match result {
            Ok(()) => info!("Internal daemon stopped"),
            Err(e) => error!("Internal daemon failed to stop: {}", e),
        }
        drop(daemon);
        then();
    });
}

fn run_in_background(dir: Option<PathBuf>, stop: impl FnOnce() + Send + 'static) {
    // Deregisters even if `stop` panics, so no waiter is stranded.
    struct Stopped(Option<PathBuf>);
    impl Drop for Stopped {
        fn drop(&mut self) {
            let (pending, changed) = stopping();
            let mut pending = pending.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(index) = pending.iter().position(|dir| *dir == self.0) {
                pending.swap_remove(index);
            }
            changed.notify_all();
        }
    }

    stopping()
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(dir.clone());
    let stopped = Stopped(dir);
    std::thread::Builder::new()
        .name("daemon-stop".into())
        .spawn(move || {
            let _stopped = stopped;
            stop();
        })
        .expect("failed to spawn the daemon stop thread");
}

fn is_stopping(pending: &[Option<PathBuf>], dir: Option<&Path>) -> bool {
    match dir {
        Some(dir) => pending
            .iter()
            .any(|stopping| stopping.as_deref() == Some(dir)),
        None => !pending.is_empty(),
    }
}

/// Wait until no daemon for `dir` (every daemon, for `None`) is stopping, up
/// to `timeout`. Returns whether they all stopped.
fn wait(dir: Option<&Path>, timeout: Duration) -> bool {
    let (pending, changed) = stopping();
    let mut pending = pending.lock().unwrap_or_else(|e| e.into_inner());
    let deadline = Instant::now() + timeout;
    while is_stopping(&pending, dir) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        pending = changed
            .wait_timeout(pending, remaining)
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
    true
}

/// Block until a daemon still stopping for `dir` has released it.
pub fn wait_for(dir: &Path) {
    if !wait(Some(dir), WAIT_LIMIT) {
        warn!(
            "A daemon for {} is still stopping after {:?}; going ahead",
            dir.display(),
            WAIT_LIMIT
        );
    }
}

/// [`wait_for`] without blocking an async executor thread.
pub async fn wait_for_async(dir: PathBuf) {
    let pending = {
        let pending = stopping().0.lock().unwrap_or_else(|e| e.into_inner());
        is_stopping(&pending, Some(&dir))
    };
    if pending {
        if let Err(e) = tokio::task::spawn_blocking(move || wait_for(&dir)).await {
            error!("Waiting for a stopping daemon failed: {}", e);
        }
    }
}

/// Wait for every background stop, up to `timeout`, so process exit does not
/// cut one short. Returns whether they all finished.
pub fn wait_for_all(timeout: Duration) -> bool {
    wait(None, timeout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn unique_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("{name}-{}", uuid::Uuid::new_v4()))
    }

    /// Lock returns while the daemon is still stopping; only a start or delete
    /// of that same directory waits, and only until the stop finishes.
    #[test]
    fn background_stop_returns_at_once_and_holds_only_its_directory() {
        let dir = unique_dir("stopping-own");
        let other = unique_dir("stopping-other");
        let (release, released) = mpsc::channel::<()>();
        let (finished, stop_finished) = mpsc::channel::<()>();

        let started = Instant::now();
        run_in_background(Some(dir.clone()), move || {
            released.recv().unwrap();
            finished.send(()).unwrap();
        });
        assert!(started.elapsed() < Duration::from_secs(1));

        assert!(!wait(Some(&dir), Duration::from_millis(50)));
        assert!(wait(Some(&other), Duration::ZERO));

        release.send(()).unwrap();
        assert!(wait(Some(&dir), Duration::from_secs(5)));
        stop_finished
            .try_recv()
            .expect("the stop ran before the wait ended");
    }

    /// The real lifecycle behind a lock and a reopen: a running embedded
    /// daemon holds its directory's `daemon.lock`, and once `wait_for` returns
    /// the next start can take it.
    #[test]
    fn waiting_for_a_background_stop_frees_the_daemon_lock() {
        use coincube_core::{
            descriptors::{CoincubeDescriptor, CoincubePolicy, PathInfo},
            miniscript::descriptor::DescriptorPublicKey,
        };
        use std::str::FromStr;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _enter = runtime.enter();
        let root = unique_dir("stopping-lock");
        std::fs::create_dir_all(&root).unwrap();
        // Existing public-only test keys. No seed, xprv or signing.
        let owner = DescriptorPublicKey::from_str("[aabbccdd]xpub68JJTXc1MWK8KLW4HGLXZBJknja7kDUJuFHnM424LbziEXsfkh1WQCiEjjHw4zLqSUm4rvhgyGkkuRowE9tCJSgt3TQB5J3SKAbZ2SdcKST/<0;1>/*").unwrap();
        let recovery = DescriptorPublicKey::from_str("[aabbccdd]xpub68JJTXc1MWK8PEQozKsRatrUHXKFNkD1Cb1BuQU9Xr5moCv87anqGyXLyUd4KpnDyZgo3gz4aN1r3NiaoweFW8UutBsBbgKHzaD5HkTkifK/<0;1>/*").unwrap();
        let descriptor = CoincubeDescriptor::new(
            CoincubePolicy::new_legacy(
                PathInfo::Single(owner),
                [(144, PathInfo::Single(recovery))]
                    .iter()
                    .cloned()
                    .collect(),
            )
            .unwrap(),
        );
        let (config, handle) = coincubed::mcp_test_fixture::wallet(&root, descriptor);
        let dir = config.data_directory().unwrap();
        let daemon: Arc<dyn Daemon + Sync + Send> =
            Arc::new(crate::daemon::embedded::EmbeddedDaemon::from_regtest_handle(config, handle));
        assert!(
            dir.try_lock().is_err(),
            "the running daemon holds its directory"
        );

        let (ran, then_ran) = mpsc::channel();
        stop_in_background(daemon, move || ran.send(()).unwrap());
        wait_for(dir.path());
        then_ran
            .try_recv()
            .expect("`then` follows the daemon stop and precedes the release");
        assert!(dir.try_lock().is_ok(), "a reopen can take the lock");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_panicking_stop_still_releases_its_directory() {
        let dir = unique_dir("stopping-panic");
        run_in_background(Some(dir.clone()), || panic!("stop failed"));
        assert!(wait(Some(&dir), Duration::from_secs(5)));
    }

    #[test]
    fn exit_waits_for_every_stop_including_one_without_a_directory() {
        let (release, released) = mpsc::channel::<()>();
        run_in_background(None, move || released.recv().unwrap());
        assert!(wait(Some(&unique_dir("stopping-unknown")), Duration::ZERO));
        assert!(!wait_for_all(Duration::from_millis(50)));
        release.send(()).unwrap();
        assert!(wait_for_all(Duration::from_secs(5)));
    }

    #[test]
    fn async_wait_follows_the_stop() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let dir = unique_dir("stopping-async");
        let (release, released) = mpsc::channel::<()>();
        run_in_background(Some(dir.clone()), move || released.recv().unwrap());
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            release.send(()).unwrap();
        });
        let started = Instant::now();
        runtime.block_on(wait_for_async(dir.clone()));
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert!(wait(Some(&dir), Duration::ZERO));
        releaser.join().unwrap();
    }
}
