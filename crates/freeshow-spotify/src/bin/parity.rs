//! Bounded test transport, not a runtime sidecar or renderer command registry.
use freeshow_spotify::*;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    future::Future,
    io::{self, BufRead, Read, Write},
    path::Path,
    pin::pin,
    sync::Arc,
    task::{Context as TaskContext, Poll, Wake, Waker},
    time::{Duration, Instant},
};
struct ThreadWake(std::thread::Thread);
impl Wake for ThreadWake {
    fn wake(self: Arc<Self>) {
        self.0.unpark()
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark()
    }
}
fn wait<F: Future>(future: F) -> F::Output {
    let w = Waker::from(Arc::new(ThreadWake(std::thread::current())));
    let mut f = pin!(future);
    loop {
        match f.as_mut().poll(&mut TaskContext::from_waker(&w)) {
            Poll::Ready(v) => return v,
            Poll::Pending => std::thread::park(),
        }
    }
}
fn invalid() -> Error {
    Error {
        code: "spotify_invalid",
        message: "Invalid test command",
    }
}
fn main() {
    let mut service = None;
    let owner = Owner::new("spotify-test", 1, Path::new("/fixture/library")).unwrap();
    let mut tickets = HashMap::<u64, Ticket>::new();
    let mut next = 0u64;
    let mut stdin = io::stdin().lock();
    loop {
        let mut line = Vec::new();
        match stdin
            .by_ref()
            .take(2 * 1024 * 1024 + 1)
            .read_until(b'\n', &mut line)
        {
            Ok(0) => break,
            Ok(_) if line.len() <= 2 * 1024 * 1024 => {}
            _ => break,
        }
        let result = (|| -> Result<Value> {
            let input: Value = serde_json::from_slice(&line).map_err(|_| invalid())?;
            match input["op"].as_str().unwrap_or("") {
                "parse" => {
                    let outcome = parse_linux(input["text"].as_str().ok_or_else(invalid)?)?;
                    Ok(json!({"wire":outcome.wire()}))
                }
                "start" => {
                    if service.is_some() {
                        return Err(invalid());
                    }
                    let mut limits = Limits::default();
                    if let Some(ms) = input["timeoutMs"].as_u64() {
                        limits.query_timeout = Duration::from_millis(ms);
                    }
                    if let Some(n) = input["stdoutBytes"].as_u64() {
                        limits.stdout_bytes = usize::try_from(n).map_err(|_| invalid())?;
                    }
                    let binary = std::env::var_os("FREESHOW_SPOTIFY_DBUS_SEND")
                        .unwrap_or_else(|| "/usr/bin/dbus-send".into());
                    let backend = Backend::capture(Path::new(&binary))?;
                    let started = SpotifyService::start(owner.clone(), backend, limits)?;
                    let capability = wait(started.ready())?;
                    service = Some(started);
                    Ok(json!(capability))
                }
                "begin" => {
                    let s = service.as_ref().ok_or_else(invalid)?;
                    let ms = input["deadlineMs"].as_u64().unwrap_or(10000).min(10000);
                    let ticket =
                        s.begin_state(&owner, Instant::now() + Duration::from_millis(ms))?;
                    next = next.checked_add(1).ok_or_else(invalid)?;
                    tickets.insert(next, ticket);
                    Ok(json!({"id":next}))
                }
                "finish" => {
                    let ticket = tickets
                        .remove(&input["id"].as_u64().ok_or_else(invalid)?)
                        .ok_or_else(invalid)?;
                    let outcome = wait(ticket.finish())?.into_reply(&owner)?;
                    let null_reason = match &outcome {
                        Outcome::SourceNull(reason) => Some(reason),
                        _ => None,
                    };
                    Ok(json!({"wire":outcome.wire(),"nullReason":null_reason}))
                }
                "drop" => {
                    tickets
                        .remove(&input["id"].as_u64().ok_or_else(invalid)?)
                        .ok_or_else(invalid)?;
                    Ok(Value::Null)
                }
                "cancel" => {
                    tickets
                        .get(&input["id"].as_u64().ok_or_else(invalid)?)
                        .ok_or_else(invalid)?
                        .cancel();
                    Ok(Value::Null)
                }
                "status" => Ok(json!(service.as_ref().ok_or_else(invalid)?.status())),
                "close" => {
                    wait(service.as_ref().ok_or_else(invalid)?.shutdown())?;
                    Ok(Value::Null)
                }
                _ => Err(invalid()),
            }
        })();
        let result = match result {
            Ok(value) => json!({"ok":true,"value":value}),
            Err(error) => json!({"ok":false,"error":error}),
        };
        println!("{result}");
        io::stdout().flush().unwrap();
    }
    tickets.clear();
    if let Some(service) = service {
        let _ = wait(service.shutdown());
    }
}
