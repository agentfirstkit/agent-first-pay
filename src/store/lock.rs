use fs2::FileExt;
use std::fs::File;
use std::path::Path;
use std::time::{Duration, Instant};

/// Default lock timeout: 5 seconds.
const DEFAULT_TIMEOUT_MS: u64 = 5000;
/// Retry interval when waiting for the lock.
const RETRY_INTERVAL_MS: u64 = 50;

/// RAII guard for a data-directory exclusive lock.
/// The lock is released when this value is dropped.
#[derive(Debug)]
pub struct DataLock {
    _file: File,
}

/// Try to acquire an exclusive lock on `{data_dir}/afpay.lock` with a timeout.
/// Retries with short intervals until the timeout expires.
pub fn acquire(data_dir: &str, timeout_ms: Option<u64>) -> Result<DataLock, String> {
    let timeout = Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));

    let dir = Path::new(data_dir);
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("cannot create data directory {data_dir}: {e}"))?;

    let lock_path = dir.join("afpay.lock");
    let file = File::create(&lock_path)
        .map_err(|e| format!("cannot create lock file {}: {e}", lock_path.display()))?;

    let start = Instant::now();
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(DataLock { _file: file }),
            Err(_) => {
                if start.elapsed() >= timeout {
                    return Err(format!(
                        "timeout acquiring lock on {data_dir} after {}ms; another operation may be in progress",
                        timeout.as_millis()
                    ));
                }
                std::thread::sleep(Duration::from_millis(RETRY_INTERVAL_MS));
            }
        }
    }
}
