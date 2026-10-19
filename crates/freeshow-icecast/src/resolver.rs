use crate::{Error, Result};
use std::{
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    sync::{mpsc, OnceLock},
};
use tokio::sync::oneshot;
struct Job {
    host: String,
    port: u16,
    reply: oneshot::Sender<Result<Vec<SocketAddr>>>,
}
struct Resolver {
    sender: mpsc::SyncSender<Job>,
}
impl Resolver {
    fn start() -> Result<Self> {
        Self::start_with(|host, port| {
            (host, port)
                .to_socket_addrs()
                .map(|a| a.take(8).collect::<Vec<_>>())
                .map_err(|e| Error::new("resolve_failed", e))
        })
    }
    fn start_with<R>(mut resolve: R) -> Result<Self>
    where
        R: FnMut(&str, u16) -> Result<Vec<SocketAddr>> + Send + 'static,
    {
        let (sender, receiver) = mpsc::sync_channel::<Job>(1);
        std::thread::Builder::new()
            .name("freeshow-icecast-dns".into())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    if job.reply.is_closed() {
                        continue;
                    }
                    // getaddrinfo cannot be cancelled. One process-wide worker remains bounded
                    // even when timed-out services are destroyed and recreated.
                    let result = resolve(&job.host, job.port);
                    let _ = job.reply.send(result);
                }
            })
            .map_err(|e| Error::new("worker_unavailable", e))?;
        Ok(Self { sender })
    }
}

static RESOLVER: OnceLock<Result<Resolver>> = OnceLock::new();
pub(crate) async fn resolve(host: &str, port: u16) -> Result<Vec<SocketAddr>> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let resolver = RESOLVER
        .get_or_init(Resolver::start)
        .as_ref()
        .map_err(Clone::clone)?;
    let (reply, receiver) = oneshot::channel();
    resolver
        .sender
        .try_send(Job {
            host: host.into(),
            port,
            reply,
        })
        .map_err(|e| match e {
            mpsc::TrySendError::Full(_) => Error::new("resolver_busy", "Bounded DNS queue full"),
            mpsc::TrySendError::Disconnected(_) => {
                Error::new("worker_unavailable", "DNS worker stopped")
            }
        })?;
    receiver
        .await
        .map_err(|_| Error::new("worker_unavailable", "DNS worker did not reply"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_dns_worker_skips_cancelled_queue_and_does_not_multiply() {
        let (entered, entry) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let resolver = Resolver::start_with(move |host, port| {
            entered.send(host.to_owned()).unwrap();
            if host == "blocked" {
                gate.recv().unwrap();
            }
            Ok(vec![SocketAddr::new("127.0.0.1".parse().unwrap(), port)])
        })
        .unwrap();
        let (reply, active) = oneshot::channel();
        resolver
            .sender
            .try_send(Job {
                host: "blocked".into(),
                port: 1,
                reply,
            })
            .unwrap();
        assert_eq!(
            entry
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap(),
            "blocked"
        );
        let (reply, cancelled) = oneshot::channel();
        resolver
            .sender
            .try_send(Job {
                host: "cancelled".into(),
                port: 2,
                reply,
            })
            .unwrap();
        drop(cancelled);
        let (reply, _) = oneshot::channel();
        assert!(matches!(
            resolver.sender.try_send(Job {
                host: "full".into(),
                port: 3,
                reply
            }),
            Err(mpsc::TrySendError::Full(_))
        ));
        release.send(()).unwrap();
        assert_eq!(active.blocking_recv().unwrap().unwrap()[0].port(), 1);
        drop(resolver);
        assert!(entry
            .recv_timeout(std::time::Duration::from_secs(2))
            .is_err());
    }
}
