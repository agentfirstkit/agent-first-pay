use fs2::FileExt;
use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
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
    _file: std::fs::File,
}

/// Try to acquire an exclusive lock on `{data_dir}/afpay.lock` with a timeout.
/// Retries with short intervals until the timeout expires.
///
/// On successful acquisition the lock file gets mode `0o600` (unix only) and the
/// holder's pid is written to its first line. The file carries no sensitive data,
/// but tightening permissions matches the data-dir hygiene of `store::db`, and
/// pid-on-disk lets operators diagnose a stuck lock without resorting to `lsof`.
pub fn acquire(data_dir: &str, timeout_ms: Option<u64>) -> Result<DataLock, String> {
    let timeout = Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));

    let dir = Path::new(data_dir);
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("cannot create data directory {data_dir}: {e}"))?;

    let lock_path = dir.join("afpay.lock");
    // Open WITHOUT truncating so contenders can read the holder's pid without
    // racing the holder's `O_TRUNC`. The successful acquirer truncates itself
    // post-lock (set_len + seek) so its pid is the only thing on disk.
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| format!("cannot create lock file {}: {e}", lock_path.display()))?;

    let start = Instant::now();
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => {
                // Best-effort diagnostics: clear stale content, record pid, tighten
                // permissions. The lock itself is already held; failures here do
                // NOT invalidate it, so swallow rather than abort.
                let _ = file.set_len(0);
                let _ = file.seek(SeekFrom::Start(0));
                let _ = writeln!(&file, "{}", std::process::id());
                let _ = (&file).flush();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(
                        &lock_path,
                        std::fs::Permissions::from_mode(0o600),
                    );
                }
                return Ok(DataLock { _file: file });
            }
            Err(_) => {
                if start.elapsed() >= timeout {
                    // Surface the holding pid so operators don't have to grep
                    // `lsof`. Unreadable file → fall back to bare error.
                    let pid_hint = std::fs::read_to_string(&lock_path)
                        .ok()
                        .and_then(|s| s.lines().next().map(str::to_string))
                        .filter(|s| !s.is_empty())
                        .map(|pid| format!(" (current holder pid: {pid})"))
                        .unwrap_or_default();
                    return Err(format!(
                        "timeout acquiring lock on {data_dir} after {}ms{pid_hint}; another operation may be in progress",
                        timeout.as_millis()
                    ));
                }
                std::thread::sleep(Duration::from_millis(RETRY_INTERVAL_MS));
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    // Reading the lock file while it is held is a Unix affordance: locks there
    // are advisory, so another handle can still read the pid the holder wrote.
    // Windows locks the bytes, and `read_to_string` on them fails — which is
    // what `acquire` already calls out as the case where the pid hint is
    // dropped. `second_acquire_reports_holder_pid_on_timeout` covers what that
    // leaves on Windows.
    #[cfg(unix)]
    #[test]
    fn lock_file_records_pid_and_is_mode_0600_on_unix() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_str().unwrap();

        let _guard = acquire(dir, Some(1000)).expect("acquire lock");

        let lock_path = tmp.path().join("afpay.lock");
        let contents = std::fs::read_to_string(&lock_path).expect("read lock file");
        let pid: u32 = contents
            .lines()
            .next()
            .unwrap()
            .parse()
            .expect("pid is integer");
        assert_eq!(pid, std::process::id());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&lock_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "lock file must be 0o600");
        }
    }

    #[test]
    fn second_acquire_reports_holder_pid_on_timeout() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_str().unwrap();

        let _holder = acquire(dir, Some(1000)).expect("first acquire");

        // fs2 advisory locks on unix are per-process — two acquire() calls from
        // the SAME process both succeed. Run the second attempt in a child
        // process so the kernel actually rejects it.
        let bin = std::env::current_exe().unwrap();
        let helper = std::process::Command::new(&bin)
            .args([
                "--exact",
                "store::lock::tests::__lock_contender_helper",
                "--nocapture",
            ])
            .env("AFPAY_LOCK_TEST_DIR", dir)
            .output()
            .expect("spawn contender");
        let stderr = String::from_utf8_lossy(&helper.stderr);
        let stdout = String::from_utf8_lossy(&helper.stdout);
        let combined = format!("{stdout}{stderr}");
        // The property that matters on both platforms is that the contender was
        // refused: `__lock_contender_helper` asserts the timeout itself, so a
        // second acquirer slipping through fails there rather than here.
        assert!(
            combined.contains("timeout acquiring lock"),
            "contender must report a lock timeout; got:\n{combined}"
        );

        // The pid hint on top of it is a Unix affordance. Windows holds the
        // bytes of the lock file, so the holder's pid cannot be read out of it
        // and `acquire` falls back to the bare error by design.
        let expected_pid = std::process::id().to_string();
        if cfg!(unix) {
            assert!(
                combined.contains(&expected_pid),
                "contender output must reference holder pid {expected_pid}; got:\n{combined}"
            );
        } else {
            assert!(
                !combined.contains(&format!("current holder pid: {expected_pid}")),
                "a pid hint here would mean the lock file was readable while held; got:\n{combined}"
            );
        }
    }

    // Invoked as a child process by `second_acquire_reports_holder_pid_on_timeout`.
    // Prints the timeout error to stdout so the parent can assert on it.
    #[test]
    #[allow(clippy::print_stdout)]
    fn __lock_contender_helper() {
        let Ok(dir) = std::env::var("AFPAY_LOCK_TEST_DIR") else {
            return;
        };
        let err = acquire(&dir, Some(200)).expect_err("contender must time out");
        println!("{err}");
    }
}
