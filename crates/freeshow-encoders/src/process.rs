//! Supervision adapted from the reviewed media process owner: both pipes are
//! drained concurrently; every normal exit/cancel/limit/timeout awaits reaping.
use crate::{first_error_line, EncoderConfig, EncoderError};
use std::{
    process::Stdio,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::Instant,
};
use tokio::{io::AsyncReadExt, process::Command};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub(crate) struct Metrics {
    pub children: AtomicUsize,
    pub probes: AtomicUsize,
    pub started: AtomicU64,
    pub reaped: AtomicU64,
}
pub(crate) struct Output {
    pub stdout: String,
}
pub(crate) struct Failure {
    pub error: EncoderError,
    pub reason: String,
}
fn failure(code: &'static str, message: impl Into<String>, stderr: &[u8], killed: bool) -> Failure {
    let error = EncoderError::new(code, message);
    let reason = first_error_line(
        &String::from_utf8_lossy(stderr),
        killed,
        Some(&error.message),
    );
    Failure { error, reason }
}
pub(crate) async fn run(
    executable: &std::path::Path,
    args: &[String],
    config: &EncoderConfig,
    limit: usize,
    cancel: &CancellationToken,
    deadline: Instant,
    metrics: &Arc<Metrics>,
) -> std::result::Result<Output, Failure> {
    if cancel.is_cancelled() {
        return Err(failure(
            "cancelled",
            "Encoder detection cancelled",
            &[],
            false,
        ));
    }
    if Instant::now() >= deadline {
        return Err(failure(
            "operation_timeout",
            "Encoder operation deadline reached",
            &[],
            false,
        ));
    }
    if args.len() > 256
        || args
            .iter()
            .any(|arg| arg.len() > 16384 || arg.contains('\0'))
        || args.iter().map(String::len).sum::<usize>() > 65536
    {
        return Err(failure(
            "argument_limit",
            "Encoder arguments exceed a resource limit",
            &[],
            false,
        ));
    }
    let mut command = Command::new(executable);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command.spawn().map_err(|error| {
        failure(
            "spawn_failed",
            format!("spawn {}: {error}", executable.display()),
            &[],
            false,
        )
    })?;
    metrics.children.fetch_add(1, Ordering::Relaxed);
    metrics.started.fetch_add(1, Ordering::Relaxed);
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut out = vec![];
    let mut err = vec![];
    let timeout = config
        .process_timeout
        .min(deadline.saturating_duration_since(Instant::now()));
    let result = {
        let output = async {
            tokio::try_join!(
                read_limited(&mut stdout, &mut out, limit, "stdout"),
                read_limited(&mut stderr, &mut err, limit, "stderr"),
                async {
                    child
                        .wait()
                        .await
                        .map_err(|e| EncoderError::new("wait_failed", e.to_string()))
                }
            )
        };
        tokio::select! {biased;_=cancel.cancelled()=>Err(EncoderError::new("cancelled","Encoder detection cancelled")),value=tokio::time::timeout(timeout,output)=>value.unwrap_or_else(|_|Err(EncoderError::new("process_timeout","Encoder process timed out")))}
    };
    let result = match result {
        Ok(((), (), status)) if status.success() => Ok(Output {
            stdout: String::from_utf8_lossy(&out).into_owned(),
        }),
        Ok(((), (), status)) => Err(failure(
            "process_exit",
            format!("Encoder process exited with {status}"),
            &err,
            false,
        )),
        Err(error) => Err(failure(
            error.code,
            error.message,
            &err,
            error.code == "process_timeout",
        )),
    };
    if result.is_err() {
        let _ = child.start_kill();
    }
    // wait() is safe after an earlier completed wait and also reaps a killed child.
    let reaped = child.wait().await;
    if let Err(error) = reaped {
        return Err(failure("reap_failed", error.to_string(), &err, false));
    }
    metrics.reaped.fetch_add(1, Ordering::Relaxed);
    metrics.children.fetch_sub(1, Ordering::Relaxed);
    result
}
async fn read_limited<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    output: &mut Vec<u8>,
    limit: usize,
    name: &'static str,
) -> crate::Result<()> {
    let mut buffer = [0; 8192];
    loop {
        let count = reader
            .read(&mut buffer)
            .await
            .map_err(|e| EncoderError::new("read_failed", e.to_string()))?;
        if count == 0 {
            return Ok(());
        }
        let remaining = limit.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..count.min(remaining)]);
        if count > remaining {
            return Err(EncoderError::new(
                "output_limit",
                format!("{name} maxBuffer length exceeded"),
            ));
        }
    }
}
