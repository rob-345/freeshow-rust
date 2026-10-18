use crate::*;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering},
        mpsc,
    },
    thread::JoinHandle,
};
use tokio::sync::{mpsc as async_mpsc, oneshot};
pub(crate) struct Shared {
    pub owner: Owner,
    pub closing: AtomicBool,
    pub cancel: CancellationToken,
    pub inputs: AtomicUsize,
    pub outputs: AtomicUsize,
    pub sequence: AtomicU64,
    admitted: AtomicUsize,
    failure: Mutex<Option<Error>>,
    callback_fault: AtomicU8,
    listeners: Mutex<Vec<u64>>,
}
impl Shared {
    pub fn live(&self) -> Result<()> {
        if self.closing.load(Ordering::Acquire) || self.cancel.is_cancelled() {
            Err(Error::closing())
        } else {
            Ok(())
        }
    }
    pub fn trip(&self, code: u8) {
        let _ = self
            .callback_fault
            .compare_exchange(0, code, Ordering::AcqRel, Ordering::Acquire);
        self.closing.store(true, Ordering::Release);
        self.cancel.cancel();
    }
    fn callback_error(&self) -> Option<Error> {
        match self.callback_fault.load(Ordering::Acquire) {
            1 => Some(Error::new(
                "midi_input_packet",
                "Unsupported native MIDI callback packet",
            )),
            2 => Some(Error::new(
                "midi_input_overflow",
                "Native MIDI input queue overflow; drain owner before reopen",
            )),
            _ => None,
        }
    }
    pub fn fail(&self, error: Error) {
        let mut failure = lock(&self.failure);
        if failure.is_none() {
            *failure = Some(error);
        }
        self.closing.store(true, Ordering::Release);
        self.cancel.cancel();
    }
    pub fn listeners(&self, listeners: Vec<u64>) {
        *lock(&self.listeners) = listeners;
    }
}
struct Work {
    command: Command,
    control: Control,
    reply: oneshot::Sender<Result<Reply>>,
    _lease: Lease,
}
struct Lease(Arc<Shared>);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.admitted.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Inner {
    shared: Arc<Shared>,
    sender: mpsc::SyncSender<Work>,
    events: Mutex<Option<async_mpsc::Receiver<Event>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    wake: std::thread::Thread,
    config: Config,
}
#[derive(Clone)]
pub struct MidiService {
    inner: Arc<Inner>,
}
pub struct Ticket {
    reply: oneshot::Receiver<Result<Reply>>,
    cancel: CancellationToken,
    armed: bool,
}
impl Ticket {
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
    pub async fn finish(mut self) -> Result<Reply> {
        let result = (&mut self.reply)
            .await
            .map_err(|_| Error::new("midi_worker", "Native MIDI owner stopped before replying"))?;
        self.armed = false;
        result
    }
}
impl Drop for Ticket {
    fn drop(&mut self) {
        if self.armed {
            self.cancel.cancel();
        }
    }
}
pub struct EventStream {
    receiver: async_mpsc::Receiver<Event>,
    shared: Arc<Shared>,
}
impl EventStream {
    pub fn pending(&self) -> usize {
        self.receiver.len()
    }
    pub async fn recv(&mut self) -> Result<Event> {
        loop {
            self.shared.live()?;
            let event = tokio::select! {biased;_=self.shared.cancel.cancelled()=>return Err(Error::closing()),event=self.receiver.recv()=>event.ok_or_else(Error::closing)?};
            self.shared.live()?;
            let listener = match &event {
                Event::Midi { listener, .. } | Event::Mtc { listener, .. } => *listener,
            };
            if lock(&self.shared.listeners).contains(&listener) {
                return Ok(event);
            }
        }
    }
}
impl MidiService {
    /// One service per trusted active context/run. Cancel/drain this owner before
    /// creating its successor; no OS handle is moved onto GTK or Tokio IO.
    pub fn with_backend(
        owner: Owner,
        owner_cancel: CancellationToken,
        config: Config,
        backend: Box<dyn Backend>,
    ) -> Result<Self> {
        if config.idle_poll < Duration::from_millis(1)
            || config.idle_poll > Duration::from_secs(1)
            || config.shutdown_timeout.is_zero()
            || config.shutdown_timeout > Duration::from_secs(30)
        {
            return Err(Error::limit());
        }
        let shared = Arc::new(Shared {
            owner,
            closing: AtomicBool::new(false),
            cancel: owner_cancel,
            inputs: AtomicUsize::new(0),
            outputs: AtomicUsize::new(0),
            sequence: AtomicU64::new(0),
            admitted: AtomicUsize::new(0),
            failure: Mutex::new(None),
            callback_fault: AtomicU8::new(0),
            listeners: Mutex::new(vec![]),
        });
        let (send, receive) = mpsc::sync_channel::<Work>(COMMAND_LIMIT);
        let (raw_send, raw_receive) = mpsc::sync_channel(EVENT_LIMIT);
        let (event_send, event_receive) = async_mpsc::channel(EVENT_LIMIT);
        let worker_shared = shared.clone();
        let poll = config.idle_poll;
        let thread = std::thread::Builder::new()
            .name("freeshow-midi-owner".into())
            .spawn(move || {
                run_owner(
                    worker_shared,
                    backend,
                    receive,
                    raw_send,
                    raw_receive,
                    event_send,
                    poll,
                )
            })
            .map_err(|_| Error::new("midi_worker", "Could not start native MIDI owner"))?;
        let wake = thread.thread().clone();
        Ok(Self {
            inner: Arc::new(Inner {
                shared,
                sender: send,
                events: Mutex::new(Some(event_receive)),
                thread: Mutex::new(Some(thread)),
                wake,
                config,
            }),
        })
    }
    #[cfg(feature = "native")]
    pub fn start(owner: Owner, owner_cancel: CancellationToken, config: Config) -> Result<Self> {
        Self::with_backend(owner, owner_cancel, config, Box::new(NativeBackend))
    }
    pub fn begin(
        &self,
        owner: &Owner,
        command: Command,
        caller: CancellationToken,
        deadline: Instant,
    ) -> Result<Ticket> {
        self.inner.shared.live()?;
        if owner != &self.inner.shared.owner {
            return Err(Error::new(
                "stale_generation",
                "Native MIDI owner/context/run is obsolete",
            ));
        }
        match &command {
            Command::Send(v) | Command::Listen(v) => bounded_value(v)?,
            Command::CloseInputs(v) if v.len() > 4096 => return Err(Error::limit()),
            Command::MtcListen(s) | Command::MtcSend { selector: s, .. } => {
                select_devices(s, &[])?;
            }
            _ => (),
        }
        let control = Control {
            caller: caller.clone(),
            deadline,
        };
        control.check()?;
        self.inner
            .shared
            .admitted
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < COMMAND_LIMIT).then_some(n + 1)
            })
            .map_err(|_| Error::new("midi_busy", "Native MIDI command queue is full"))?;
        let lease = Lease(self.inner.shared.clone());
        let (reply, receive) = oneshot::channel();
        self.inner
            .sender
            .try_send(Work {
                command,
                control,
                reply,
                _lease: lease,
            })
            .map_err(|_| Error::new("midi_busy", "Native MIDI command owner unavailable"))?;
        self.inner.wake.unpark();
        Ok(Ticket {
            reply: receive,
            cancel: caller,
            armed: true,
        })
    }
    pub fn events(&self) -> Result<EventStream> {
        let receiver = lock(&self.inner.events).take().ok_or_else(|| {
            Error::new(
                "midi_event_owner",
                "Native MIDI events already have an owner",
            )
        })?;
        Ok(EventStream {
            receiver,
            shared: self.inner.shared.clone(),
        })
    }
    pub fn status(&self) -> Status {
        let shared = &self.inner.shared;
        Status {
            closing: shared.live().is_err(),
            admitted: shared.admitted.load(Ordering::Acquire),
            inputs: shared.inputs.load(Ordering::Acquire),
            outputs: shared.outputs.load(Ordering::Acquire),
            worker_finished: lock(&self.inner.thread)
                .as_ref()
                .is_none_or(JoinHandle::is_finished),
            failure: lock(&shared.failure)
                .clone()
                .or_else(|| shared.callback_error()),
        }
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.inner.shared.closing.store(true, Ordering::Release);
        self.inner.shared.cancel.cancel();
        self.inner.wake.unpark();
        let deadline = Instant::now() + self.inner.config.shutdown_timeout;
        loop {
            if lock(&self.inner.thread)
                .as_ref()
                .is_none_or(JoinHandle::is_finished)
            {
                break;
            }
            if Instant::now() >= deadline {
                return Err(Error::new(
                    "midi_shutdown_timeout",
                    "Native MIDI owner has not drained actual backend operations",
                ));
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        if let Some(thread) = lock(&self.inner.thread).take() {
            thread.join().map_err(|_| {
                Error::new("midi_worker", "Native MIDI owner panicked during shutdown")
            })?;
        }
        if let Some(error) = lock(&self.inner.shared.failure).clone() {
            return Err(error);
        }
        Ok(())
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.shared.closing.store(true, Ordering::Release);
        self.shared.cancel.cancel();
        self.wake.unpark();
    }
}

fn run_owner(
    shared: Arc<Shared>,
    backend: Box<dyn Backend>,
    receive: mpsc::Receiver<Work>,
    raw_send: mpsc::SyncSender<ports::Raw>,
    raw_receive: mpsc::Receiver<ports::Raw>,
    event_send: async_mpsc::Sender<Event>,
    poll: Duration,
) {
    let mut ports = ports::Ports::new(backend, shared.clone(), raw_send);
    let mut health = Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        while shared.live().is_ok() {
            let mut did_work = false;
            // Commands stay ordered and cannot be starved by callbacks.
            for _ in 0..COMMAND_LIMIT {
                let Ok(work) = receive.try_recv() else {
                    break;
                };
                did_work = true;
                let result = shared
                    .live()
                    .and_then(|_| work.control.check())
                    .and_then(|_| ports.command(work.command));
                ports.publish_listeners();
                let _ = work.reply.send(result);
            }
            for _ in 0..EVENT_LIMIT {
                if shared.live().is_err() {
                    break;
                }
                let Ok(raw) = raw_receive.try_recv() else {
                    break;
                };
                did_work = true;
                match ports.messages(raw) {
                    Ok(events) => {
                        for event in events {
                            if event_send.try_send(event).is_err() {
                                shared.fail(Error::new("midi_event_overflow","Native MIDI event consumer is slow or closed; drain owner before reopen"));
                                break;
                            }
                        }
                    }
                    Err(error) => shared.fail(error),
                }
            }
            if shared.live().is_ok() && health.elapsed() >= Duration::from_secs(1) {
                health = Instant::now();
                if let Err(error) = ports.health() {
                    shared.fail(error);
                }
            }
            if !did_work {
                std::thread::park_timeout(poll);
            }
        }
    }));
    shared.closing.store(true, Ordering::Release);
    shared.cancel.cancel();
    shared.listeners(vec![]);
    if result.is_err() {
        shared.fail(Error::new("midi_worker", "Native MIDI backend panicked"));
    }
    if let Some(error) = shared.callback_error() {
        shared.fail(error);
    }
    // No detached open/send/close: this owner drains the real backend operations.
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ports.shutdown())) {
        Ok(errors) => {
            if let Some(error) = errors.into_iter().next() {
                shared.fail(error);
            }
        }
        Err(_) => shared.fail(Error::new("midi_worker", "Native MIDI close panicked")),
    }
    while let Ok(work) = receive.try_recv() {
        let _ = work.reply.send(Err(Error::closing()));
    }
}
