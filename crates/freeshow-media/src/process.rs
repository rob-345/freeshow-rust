use crate::{MediaConfig, MediaError};
use std::{ffi::OsString, path::Path, process::Stdio};
use tokio::{io::AsyncReadExt, process::Command};
use tokio_util::sync::CancellationToken;

pub(crate) async fn run(
    executable: &Path,
    args: &[OsString],
    config: &MediaConfig,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, MediaError> {
    if cancel.is_cancelled() {
        return Err(MediaError::Cancelled);
    }
    let mut command = Command::new(executable);
    command.args(args);
    #[cfg(windows)]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW, same as windowsHide in Electron.
    supervise(command, config, cancel).await
}

async fn supervise(
    mut command: Command,
    config: &MediaConfig,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, MediaError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|error| {
        MediaError::Process(format!(
            "{}: {error}",
            command.as_std().get_program().to_string_lossy()
        ))
    })?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let output = async {
        let (out, err, status) = tokio::try_join!(
            read_limited(&mut stdout, config.stdout_limit, "stdout"),
            read_limited(&mut stderr, config.stderr_limit, "stderr"),
            async {
                child
                    .wait()
                    .await
                    .map_err(|error| MediaError::Process(error.to_string()))
            }
        )?;
        if !status.success() {
            return Err(MediaError::Exit {
                code: status.code(),
                stderr: String::from_utf8_lossy(&err).into_owned(),
            });
        }
        Ok(out)
    };
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(MediaError::Cancelled),
        value = tokio::time::timeout(config.process_timeout, output) => value.unwrap_or(Err(MediaError::Timeout)),
    };
    if result.is_err() {
        // Dropping the output future releases its wait borrow before we kill and reap.
        // kill_on_drop also covers runtime teardown; normal cancellation is explicitly reaped here.
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
    result
}

async fn read_limited<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    limit: usize,
    name: &'static str,
) -> Result<Vec<u8>, MediaError> {
    let mut output = Vec::with_capacity(limit.min(8192));
    let mut buffer = [0; 8192];
    loop {
        let count = reader
            .read(&mut buffer)
            .await
            .map_err(|error| MediaError::Process(error.to_string()))?;
        if count == 0 {
            return Ok(output);
        }
        if count > limit.saturating_sub(output.len()) {
            return Err(MediaError::OutputLimit(name));
        }
        output.extend_from_slice(&buffer[..count]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        time::{Duration, Instant},
    };

    fn fixture(name: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--ignored", "--exact", name, "--nocapture"]);
        command
    }
    fn config() -> MediaConfig {
        MediaConfig {
            process_timeout: Duration::from_millis(500),
            stdout_limit: 4096,
            stderr_limit: 4096,
            ..MediaConfig::default()
        }
    }

    // These are subprocess fixtures, not skipped assertions: tests below invoke them.
    #[test]
    #[ignore = "subprocess fixture invoked by supervisor tests"]
    fn child_sleep() {
        std::thread::sleep(Duration::from_secs(30));
    }
    #[test]
    #[ignore = "subprocess fixture invoked by supervisor tests"]
    fn child_stdout() {
        std::io::stdout()
            .write_all(&vec![b'x'; 1024 * 1024])
            .unwrap();
    }
    #[test]
    #[ignore = "subprocess fixture invoked by supervisor tests"]
    fn child_stderr() {
        std::io::stderr()
            .write_all(&vec![b'x'; 1024 * 1024])
            .unwrap();
    }
    #[test]
    #[ignore = "subprocess fixture invoked by supervisor tests"]
    fn child_failure() {
        eprintln!("corrupt media");
        std::process::exit(7);
    }
    #[test]
    #[ignore = "subprocess fixture invoked by supervisor tests"]
    fn child_witness() {
        let file = std::env::var_os("FREESHOW_MEDIA_TEST_WITNESS").unwrap();
        std::fs::write(&file, std::process::id().to_string()).unwrap();
        std::thread::sleep(Duration::from_secs(30));
    }

    #[tokio::test]
    async fn timeout_output_caps_and_failure_exit_are_distinct() {
        let token = CancellationToken::new();
        let started = Instant::now();
        assert!(matches!(
            supervise(fixture("process::tests::child_sleep"), &config(), &token).await,
            Err(MediaError::Timeout)
        ));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(matches!(
            supervise(fixture("process::tests::child_stdout"), &config(), &token).await,
            Err(MediaError::OutputLimit("stdout"))
        ));
        assert!(matches!(
            supervise(fixture("process::tests::child_stderr"), &config(), &token).await,
            Err(MediaError::OutputLimit("stderr"))
        ));
        assert!(
            matches!(supervise(fixture("process::tests::child_failure"), &config(), &token).await, Err(MediaError::Exit {code:Some(7), stderr}) if stderr.contains("corrupt media"))
        );
    }

    #[tokio::test]
    async fn cancellation_kills_and_reaps_before_returning() {
        let file =
            std::env::temp_dir().join(format!("freeshow-media-child-{}", std::process::id()));
        let _ = std::fs::remove_file(&file);
        let mut command = fixture("process::tests::child_witness");
        command.env("FREESHOW_MEDIA_TEST_WITNESS", &file);
        let token = CancellationToken::new();
        let cancel = async {
            for _ in 0..100 {
                if std::fs::read_to_string(&file)
                    .ok()
                    .and_then(|value| value.parse::<u32>().ok())
                    .is_some()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(file.exists(), "test process failed to start");
            token.cancel();
        };
        let long = MediaConfig {
            process_timeout: Duration::from_secs(5),
            ..config()
        };
        let (result, _) = tokio::join!(supervise(command, &long, &token), cancel);
        assert!(matches!(result, Err(MediaError::Cancelled)));
        let pid = std::fs::read_to_string(&file).unwrap();
        #[cfg(target_os = "linux")]
        assert!(
            !std::path::Path::new("/proc").join(pid.trim()).exists(),
            "child must be reaped, not left running or as a zombie"
        );
        #[cfg(not(target_os = "linux"))]
        let _ = pid;
        std::fs::remove_file(file).unwrap();
    }
}
