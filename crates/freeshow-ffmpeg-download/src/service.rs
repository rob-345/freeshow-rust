use crate::*;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread::JoinHandle,
};
use tokio::sync::{oneshot, watch, OwnedSemaphorePermit, Semaphore};
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Queued,
    Streaming,
    Prepared,
    Accepted,
    Closed,
}
pub(crate) struct Shared {
    pub cancel: CancellationToken,
    pub failure: Mutex<Option<Error>>,
    pub active: AtomicUsize,
    pub slots: Arc<Semaphore>,
    pub jobs: Mutex<Vec<std::sync::Weak<Job>>>,
}
pub(crate) struct Job {
    pub plan: HostDownloadPlan,
    pub config: Config,
    pub control: Control,
    pub private: PathBuf,
    pub staged: PathBuf,
    pub phase: Mutex<Phase>,
    pub artifact: Arc<Mutex<io::Artifact>>,
    pub stats: Mutex<TransferStats>,
    pub progress: watch::Sender<Option<Progress>>,
    pub shared: Arc<Shared>,
    io: Mutex<Option<mpsc::SyncSender<io::Work>>>,
    cleanup_done: watch::Sender<Option<Result<()>>>,
    permit: Mutex<Option<OwnedSemaphorePermit>>,
}
impl Job {
    pub async fn cleanup(&self) -> Result<()> {
        let mut done = self.cleanup_done.subscribe();
        self.queue_cleanup()?;
        loop {
            let result = done.borrow().clone();
            if let Some(result) = result {
                return result;
            }
            done.changed()
                .await
                .map_err(|_| Error::new("download_cleanup", "Cleanup worker stopped"))?;
        }
    }
    fn queue_cleanup(&self) -> Result<()> {
        let Some(permit) = locked(&self.permit).take() else {
            return Ok(());
        };
        *locked(&self.phase) = Phase::Closed;
        let result = locked(&self.io)
            .take()
            .ok_or_else(|| Error::new("download_cleanup", "Private cleanup sender unavailable"))?
            .try_send(io::Work::Cleanup {
                path: self.private.clone(),
                artifact: std::mem::take(&mut *locked(&self.artifact)),
                root: self.plan.root.clone(),
                shared: self.shared.clone(),
                permit,
                completion: self.cleanup_done.clone(),
            })
            .map_err(|_| Error::new("download_cleanup", "Private cleanup owner unavailable"));
        if let Err(error) = &result {
            *locked(&self.shared.failure) = Some(error.clone());
            self.shared.cancel.cancel();
            self.cleanup_done.send_replace(Some(Err(error.clone())));
        }
        result
    }
    pub async fn resolve(&self, host: String, port: u16) -> Result<Vec<std::net::SocketAddr>> {
        let (reply, receive) = oneshot::channel();
        locked(&self.io)
            .as_ref()
            .ok_or_else(Error::shutdown)?
            .try_send(io::Work::Resolve {
                host,
                port,
                control: self.control.clone(),
                reply,
            })
            .map_err(|_| Error::new("download_io", "Fixed DNS worker unavailable"))?;
        // This fixed network pump retains the admitted job until the actual OS
        // lookup returns, including after its ticket/caller is cancelled.
        receive
            .await
            .map_err(|_| Error::new("download_io", "DNS worker stopped"))?
    }
    pub async fn authorize(&self) -> Result<()> {
        let (reply, receive) = oneshot::channel();
        locked(&self.io)
            .as_ref()
            .ok_or_else(Error::shutdown)?
            .try_send(io::Work::Authorize {
                root: self.plan.root.clone(),
                artifact: self.artifact.clone(),
                control: self.control.clone(),
                reply,
            })
            .map_err(|_| Error::new("download_io", "Fixed root worker unavailable"))?;
        receive
            .await
            .map_err(|_| Error::new("download_io", "Root worker stopped"))?
    }
    pub async fn archive(
        &self,
        source: tokio::sync::mpsc::Receiver<Vec<u8>>,
    ) -> Result<oneshot::Receiver<Result<TransferStats>>> {
        let (reply, receive) = oneshot::channel();
        locked(&self.io)
            .as_ref()
            .ok_or_else(Error::shutdown)?
            .try_send(io::Work::Archive {
                job: ArchiveJob {
                    private: self.private.clone(),
                    staged: self.staged.clone(),
                    root: self.plan.root.clone(),
                    control: self.control.clone(),
                    config: self.config.clone(),
                    artifact: self.artifact.clone(),
                },
                source,
                reply,
            })
            .map_err(|_| Error::new("download_io", "Fixed archive worker unavailable"))?;
        Ok(receive)
    }
}
// The artifact's authority cell is shared with its one fixed archive worker.
pub(crate) struct ArchiveJob {
    pub private: PathBuf,
    pub staged: PathBuf,
    pub root: PathBuf,
    pub control: Control,
    pub config: Config,
    pub artifact: Arc<Mutex<io::Artifact>>,
}
impl Drop for Job {
    fn drop(&mut self) {
        let _ = self.queue_cleanup();
    }
}
struct NetWork {
    job: Arc<Job>,
    reply: oneshot::Sender<Result<PreparedDownload>>,
}
struct Inner {
    config: Config,
    shared: Arc<Shared>,
    sender: Mutex<Option<tokio::sync::mpsc::Sender<NetWork>>>,
    io: Mutex<Option<mpsc::SyncSender<io::Work>>>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    network: tokio::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
    shutdown: tokio::sync::Mutex<()>,
}
#[derive(Clone)]
pub struct DownloadService {
    inner: Arc<Inner>,
}
pub struct DownloadTicket {
    pub(crate) job: Arc<Job>,
    reply: Option<oneshot::Receiver<Result<PreparedDownload>>>,
    armed: bool,
}
impl DownloadTicket {
    pub fn progress(&self) -> watch::Receiver<Option<Progress>> {
        self.job.progress.subscribe()
    }
    pub fn cancel(&self) {
        self.job.control.cancel.cancel();
    }
    pub async fn finish(mut self) -> Result<PreparedDownload> {
        let result = self
            .reply
            .take()
            .unwrap()
            .await
            .map_err(|_| Error::new("download_worker", "Download network owner stopped"))?;
        self.armed = false;
        if let Err(error) = self.job.control.check() {
            if let Ok(prepared) = result {
                prepared.discard().await?;
            }
            return Err(error);
        }
        result
    }
}
impl Drop for DownloadTicket {
    fn drop(&mut self) {
        if self.armed {
            self.job.control.cancel.cancel();
        }
    }
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub admitted: usize,
    pub active: usize,
    pub workers: usize,
    pub closing: bool,
    pub failure: Option<Error>,
}
impl DownloadService {
    pub fn start(config: Config) -> Result<Self> {
        config.validate()?;
        tokio::runtime::Handle::try_current().map_err(|_| {
            Error::new(
                "download_runtime",
                "Download service requires a Tokio runtime",
            )
        })?;
        let tls = http::tls()?;
        let shared = Arc::new(Shared {
            cancel: CancellationToken::new(),
            failure: Mutex::new(None),
            active: AtomicUsize::new(0),
            slots: Arc::new(Semaphore::new(MAX_JOBS)),
            jobs: Mutex::new(Vec::with_capacity(MAX_JOBS)),
        });
        let (send_io, receive_io) = mpsc::sync_channel(4);
        let receive_io = Arc::new(Mutex::new(receive_io));
        let mut threads = Vec::new();
        for index in 0..2 {
            let receive = receive_io.clone();
            match std::thread::Builder::new()
                .name(format!("freeshow-ffmpeg-files-{index}"))
                .spawn(move || loop {
                    let work = locked(&receive).recv();
                    let Ok(work) = work else {
                        break;
                    };
                    io::run(work);
                }) {
                Ok(thread) => threads.push(thread),
                Err(_) => {
                    drop(send_io);
                    drop(receive_io);
                    for thread in threads {
                        let _ = thread.join();
                    }
                    return Err(Error::new(
                        "download_worker",
                        "Could not start download workers",
                    ));
                }
            }
        }
        let (sender, receiver) = tokio::sync::mpsc::channel::<NetWork>(MAX_JOBS);
        let receiver = Arc::new(tokio::sync::Mutex::new(receiver));
        let mut network = Vec::new();
        for _ in 0..2 {
            let receiver = receiver.clone();
            let shared = shared.clone();
            let tls = tls.clone();
            network.push(tokio::spawn(async move {
                loop {
                    let work=if shared.cancel.is_cancelled(){
                        if reap(&shared,true).await{continue;}
                        let mut receiver=receiver.lock().await;receiver.close();receiver.try_recv().ok()
                    }else{
                        tokio::select! {biased;
                            _=shared.cancel.cancelled()=>continue,
                            _=tokio::time::sleep(Duration::from_millis(20))=>{reap(&shared,false).await;continue;},
                            work=async{receiver.lock().await.recv().await}=>work,
                        }
                    };
                    let Some(work)=work else{break;};shared.active.fetch_add(1,Ordering::AcqRel);
                    let result=http::transfer(&work.job,tls.clone()).await;
                    let result=match result {
                        Ok(stats)=>if let Err(error)=work.job.control.check(){match work.job.cleanup().await{Ok(())=>Err(error),Err(cleanup)=>Err(cleanup)}}else{*locked(&work.job.phase)=Phase::Prepared;Ok(PreparedDownload{job:work.job.clone(),stats})},
                        Err(error)=>match work.job.cleanup().await{Ok(())=>Err(error),Err(cleanup)=>Err(cleanup)},
                    };
                    shared.active.fetch_sub(1,Ordering::AcqRel);let _=work.reply.send(result);
                }
            }));
        }
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                shared,
                sender: Mutex::new(Some(sender)),
                io: Mutex::new(Some(send_io)),
                threads: Mutex::new(threads),
                network: tokio::sync::Mutex::new(network),
                shutdown: tokio::sync::Mutex::new(()),
            }),
        })
    }
    pub fn begin(
        &self,
        plan: HostDownloadPlan,
        caller: CancellationToken,
        deadline: Instant,
    ) -> Result<DownloadTicket> {
        if self.inner.shared.cancel.is_cancelled() {
            return Err(Error::shutdown());
        }
        let control = Control {
            service: self.inner.shared.cancel.clone(),
            caller,
            cancel: CancellationToken::new(),
            deadline: deadline.min(Instant::now() + self.inner.config.timeout),
        };
        control.check()?;
        let permit = self
            .inner
            .shared
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                Error::new(
                    "download_busy",
                    "FFmpeg download preparation is at capacity",
                )
            })?;
        let mut random = [0u8; 16];
        getrandom::fill(&mut random)
            .map_err(|_| Error::new("download_random", "Private download name unavailable"))?;
        let id = random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let private = plan.root.join(format!(".freeshow-ffmpeg-{id}"));
        let staged = private.join("artifact");
        let (progress, _) = watch::channel(None);
        let (cleanup_done, _) = watch::channel(None);
        let job = Arc::new(Job {
            plan,
            config: self.inner.config.clone(),
            control,
            private,
            staged,
            phase: Mutex::new(Phase::Queued),
            artifact: Arc::new(Mutex::new(io::Artifact::default())),
            stats: Mutex::new(TransferStats::default()),
            progress,
            shared: self.inner.shared.clone(),
            io: Mutex::new(Some(
                locked(&self.inner.io)
                    .as_ref()
                    .ok_or_else(Error::shutdown)?
                    .clone(),
            )),
            cleanup_done,
            permit: Mutex::new(Some(permit)),
        });
        {
            let mut jobs = locked(&self.inner.shared.jobs);
            jobs.retain(|job| {
                job.upgrade()
                    .is_some_and(|job| *locked(&job.phase) != Phase::Closed)
            });
            if jobs.len() >= MAX_JOBS {
                return Err(Error::limit());
            }
            jobs.push(Arc::downgrade(&job));
        }
        let (reply, receive) = oneshot::channel();
        locked(&self.inner.sender)
            .as_ref()
            .ok_or_else(Error::shutdown)?
            .try_send(NetWork {
                job: job.clone(),
                reply,
            })
            .map_err(|_| Error::new("download_busy", "Download network queue unavailable"))?;
        Ok(DownloadTicket {
            job,
            reply: Some(receive),
            armed: true,
        })
    }
    pub fn status(&self) -> Status {
        Status {
            admitted: MAX_JOBS - self.inner.shared.slots.available_permits(),
            active: self.inner.shared.active.load(Ordering::Acquire),
            workers: locked(&self.inner.threads)
                .iter()
                .filter(|w| !w.is_finished())
                .count(),
            closing: self.inner.shared.cancel.is_cancelled(),
            failure: locked(&self.inner.shared.failure).clone(),
        }
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.inner.shared.cancel.cancel();
        locked(&self.inner.sender).take();
        let deadline = Instant::now() + self.inner.config.shutdown_timeout;
        let wait = async {
            let _serial = self.inner.shutdown.lock().await;
            let mut network = self.inner.network.lock().await;
            for pump in network.iter_mut() {
                pump.await
                    .map_err(|_| Error::new("download_worker", "Download network pump panicked"))?;
            }
            network.clear();
            drop(network);
            while self.inner.shared.slots.available_permits() != MAX_JOBS {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            locked(&self.inner.io).take();
            while !locked(&self.inner.threads)
                .iter()
                .all(JoinHandle::is_finished)
            {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            for thread in locked(&self.inner.threads).drain(..) {
                thread
                    .join()
                    .map_err(|_| Error::new("download_worker", "Download file worker panicked"))?;
            }
            if let Some(error) = locked(&self.inner.shared.failure).clone() {
                return Err(error);
            }
            Ok(())
        };
        tokio::time::timeout_at(deadline.into(), wait)
            .await
            .map_err(|_| {
                Error::new(
                    "download_shutdown_timeout",
                    "Download ownership did not drain before shutdown deadline",
                )
            })?
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.shared.cancel.cancel();
        self.sender
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        self.io.get_mut().unwrap_or_else(|e| e.into_inner()).take();
    }
}

async fn reap(shared: &Shared, terminal: bool) -> bool {
    let jobs = locked(&shared.jobs)
        .iter()
        .filter_map(std::sync::Weak::upgrade)
        .collect::<Vec<_>>();
    for job in jobs {
        let revoke = {
            let mut phase = locked(&job.phase);
            if *phase == Phase::Prepared && (terminal || job.control.check().is_err()) {
                *phase = Phase::Closed;
                true
            } else {
                false
            }
        };
        if revoke {
            let _ = job.cleanup().await;
            return true;
        }
    }
    false
}
