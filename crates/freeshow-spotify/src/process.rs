use crate::{service::Shared, Backend, Error, Result};
use std::{
    process::{ExitStatus, Stdio},
    sync::Arc,
    time::Instant,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
};
use tokio_util::sync::CancellationToken;

pub(crate) struct Output {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub status: ExitStatus,
}
async fn read<R: AsyncRead + Unpin>(reader: &mut R, limit: usize) -> Result<Vec<u8>> {
    let mut result = Vec::with_capacity(limit.min(4096));
    let mut buffer = [0; 4096];
    loop {
        let count = reader
            .read(&mut buffer)
            .await
            .map_err(|_| Error::new("spotify_pipe", "Failed to read native process output"))?;
        if count == 0 {
            return Ok(result);
        }
        if count > limit.saturating_sub(result.len()) {
            return Err(Error::new(
                "spotify_output_limit",
                "Native process output limit exceeded",
            ));
        }
        result.extend_from_slice(&buffer[..count]);
    }
}
pub(crate) async fn run(
    backend: &Backend,
    args: &[&str],
    limits: (usize, usize),
    deadline: Instant,
    cancel: &CancellationToken,
    shared: &Arc<Shared>,
    active: &mut Option<Child>,
) -> Result<Output> {
    if cancel.is_cancelled() {
        return Err(Error::new("spotify_cancelled", "Native query cancelled"));
    }
    if Instant::now() >= deadline {
        return Err(Error::new(
            "spotify_timeout",
            "Total query deadline exceeded",
        ));
    }
    let mut command = Command::new(&backend.executable);
    command
        .args(args)
        .env_clear()
        .envs(backend.env.iter().map(|(k, v)| (*k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    *active = Some(command.spawn().map_err(|_| {
        Error::new(
            "spotify_binary_unavailable",
            "Native dbus-send process could not start",
        )
    })?);
    let child = active.as_mut().expect("anchored child");
    let count = shared.spawned.try_update(
        std::sync::atomic::Ordering::AcqRel,
        std::sync::atomic::Ordering::Acquire,
        |n| n.checked_add(1),
    );
    shared.pid.store(
        child.id().unwrap_or(0),
        std::sync::atomic::Ordering::Release,
    );
    shared.notify_spawn();
    #[cfg(test)]
    if shared
        .panic_after_spawn
        .swap(false, std::sync::atomic::Ordering::AcqRel)
    {
        panic!("controlled post-spawn query fault");
    }
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let output = async {
        let (stdout, stderr, status) = tokio::try_join!(
            read(&mut stdout, limits.0),
            read(&mut stderr, limits.1),
            async {
                child
                    .wait()
                    .await
                    .map_err(|_| Error::new("spotify_wait", "Native child wait failed"))
            }
        )?;
        Ok(Output {
            stdout,
            stderr,
            status,
        })
    };
    let result = {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(Error::new("spotify_cancelled", "Native query cancelled")),
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => Err(Error::new("spotify_timeout", "Total query deadline exceeded")),
            result = output => result,
        }
    };
    if count.is_err() {
        shared.fail(Error::new(
            "spotify_counter_limit",
            "Process counter exhausted",
        ));
    }
    settle(active, shared, result.is_err()).await;
    result
}
pub(crate) async fn settle(active: &mut Option<Child>, shared: &Arc<Shared>, kill: bool) {
    if let Some(child) = active.as_mut() {
        if kill {
            let _ = child.start_kill();
        }
        #[cfg(test)]
        shared.hold_reap();
        // Accepted child ownership stays on this fixed owner. Even a caller's
        // timeout never drops the only wait. A real wait fault is sticky and
        // retains the same child for retry rather than certifying drain.
        loop {
            match child.wait().await {
                Ok(_) => break,
                Err(_) => {
                    shared.fail(Error::new(
                        "spotify_wait",
                        "Native child reap remains pending",
                    ));
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
        shared.pid.store(0, std::sync::atomic::Ordering::Release);
        if shared
            .reaped
            .try_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |n| n.checked_add(1),
            )
            .is_err()
        {
            shared.fail(Error::new(
                "spotify_counter_limit",
                "Reap counter exhausted",
            ));
        }
        active.take();
    }
}
