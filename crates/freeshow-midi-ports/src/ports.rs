use crate::*;
use freeshow_midi::{InputDecision, PortBindings, PortToken};
use std::sync::atomic::{AtomicBool, Ordering};
pub(crate) struct Raw {
    pub physical: u64,
    pub sequence: u64,
    pub bytes: [u8; 12],
    pub len: usize,
    pub timestamp: u64,
    pub wall: f64,
}
struct InPort {
    number: u64,
    device: Device,
    handle: Box<dyn Input>,
    live: Arc<AtomicBool>,
}
struct OutPort {
    device: Device,
    handle: Box<dyn Output>,
}
pub(crate) struct Ports {
    backend: Box<dyn Backend>,
    bindings: PortBindings,
    inputs: Vec<InPort>,
    outputs: Vec<OutPort>,
    input_refs: Vec<(PortToken, u64, u64, u64)>,
    output_refs: Vec<(PortToken, String)>,
    mtc: Option<(u64, u64, u64)>,
    next: u64,
    shared: Arc<service::Shared>,
    raw: std::sync::mpsc::SyncSender<Raw>,
    wake: std::thread::Thread,
}
impl Ports {
    pub fn new(
        backend: Box<dyn Backend>,
        shared: Arc<service::Shared>,
        raw: std::sync::mpsc::SyncSender<Raw>,
    ) -> Self {
        Self {
            backend,
            bindings: PortBindings::default(),
            inputs: vec![],
            outputs: vec![],
            input_refs: vec![],
            output_refs: vec![],
            mtc: None,
            next: 0,
            shared,
            raw,
            wake: std::thread::current(),
        }
    }
    fn next(&mut self) -> u64 {
        self.next += 1;
        self.next
    }
    fn devices(&mut self, direction: Direction) -> Result<Vec<Device>> {
        let devices = self.backend.devices(direction)?;
        if devices.len() > DEVICE_LIMIT {
            return Err(Error::limit());
        }
        let mut total = 0usize;
        for d in &devices {
            total = total.saturating_add(d.name.len() + d.id.len());
            if d.name.len() > 4096 || d.id.len() > 4096 || total > 65536 {
                return Err(Error::limit());
            }
        }
        Ok(devices)
    }
    fn candidates(&mut self, direction: Direction, selector: &Selector) -> Result<Vec<Device>> {
        let devices = self.devices(direction)?;
        let selected = select_devices(selector, &devices)?;
        Ok(selected.into_iter().map(|i| devices[i].clone()).collect())
    }
    fn input(&mut self, selector: &Selector) -> Result<u64> {
        let candidates = self.candidates(Direction::Input, selector)?;
        let mut error = Error::new(
            "midi_open_input",
            "Error opening MIDI listener: Device not found or not supported!",
        );
        for device in candidates {
            if let Some(port) = self.inputs.iter().find(|p| p.device.id == device.id) {
                return Ok(port.number);
            }
            if device.name.is_empty() {
                return Err(Error::new(
                    "midi_unsupported",
                    "Unnamed native input cannot be tracked safely",
                ));
            }
            if self.inputs.len() == INPUT_LIMIT {
                return Err(Error::limit());
            }
            self.shared.live()?;
            let number = self.next();
            let live = Arc::new(AtomicBool::new(false));
            let gate = live.clone();
            let shared = self.shared.clone();
            let send = self.raw.clone();
            let wake = self.wake.clone();
            let callback = Arc::new(move |timestamp: u64, bytes: &[u8]| {
                if !gate.load(Ordering::Acquire)
                    || shared.closing.load(Ordering::Acquire)
                    || shared.cancel.is_cancelled()
                {
                    return;
                }
                // Native SysEx is filtered before upstream assembly. Reject an
                // unexpected oversized/non-status callback before any copying.
                if bytes.is_empty() {
                    return;
                }
                if bytes.len() > 12 || bytes[0] < 0x80 || bytes[0] == 0xf0 {
                    shared.trip(1);
                    wake.unpark();
                    return;
                }
                let sequence = shared.sequence.fetch_add(1, Ordering::AcqRel) + 1;
                let mut packet = [0; 12];
                packet[..bytes.len()].copy_from_slice(bytes);
                let wall = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0.0, |v| v.as_millis() as f64);
                if send
                    .try_send(Raw {
                        physical: number,
                        sequence,
                        bytes: packet,
                        len: bytes.len(),
                        timestamp,
                        wall,
                    })
                    .is_err()
                {
                    shared.trip(2);
                }
                wake.unpark();
            });
            match self.backend.input(&device, callback) {
                Ok(handle) => {
                    self.inputs.push(InPort {
                        number,
                        device,
                        handle,
                        live,
                    });
                    self.counts();
                    return Ok(number);
                }
                Err(e) => {
                    live.store(false, Ordering::Release);
                    error = e;
                }
            }
        }
        Err(error)
    }
    fn arm_input(&self, physical: u64) {
        if let Some(port) = self.inputs.iter().find(|p| p.number == physical) {
            port.live.store(true, Ordering::Release);
        }
    }
    fn output(&mut self, selector: &Selector) -> Result<String> {
        let candidates = self.candidates(Direction::Output, selector)?;
        let mut error = Error::new(
            "midi_open_output",
            "Error sending MIDI signal: Could not connect to receiver!",
        );
        for device in candidates {
            if self.outputs.iter().any(|p| p.device.id == device.id) {
                return Ok(device.id);
            }
            if self.outputs.len() == OUTPUT_LIMIT {
                return Err(Error::limit());
            }
            self.shared.live()?;
            match self.backend.output(&device) {
                Ok(handle) => {
                    let id = device.id.clone();
                    self.outputs.push(OutPort { device, handle });
                    self.counts();
                    return Ok(id);
                }
                Err(e) => error = e,
            }
        }
        Err(error)
    }
    fn packet(&mut self, device: &str, bytes: &[u8]) -> Result<()> {
        self.shared.live()?;
        self.outputs
            .iter_mut()
            .find(|p| p.device.id == device)
            .ok_or_else(|| Error::new("midi_state", "Output handle missing"))?
            .handle
            .send(bytes)
    }
    fn close_input_refs(
        bindings: &mut PortBindings,
        refs: &mut Vec<(PortToken, u64, u64, u64)>,
        ports: &mut Vec<InPort>,
        mtc: Option<(u64, u64, u64)>,
        id: &str,
    ) -> Result<Vec<Error>> {
        bindings
            .close_inputs(id, |token| {
                let Some(i) = refs.iter().position(|r| r.0 == *token) else {
                    return Err(pure_error(Error::new(
                        "midi_state",
                        "Input binding missing",
                    )));
                };
                let physical = refs[i].1;
                if refs.iter().filter(|r| r.1 == physical).count() == 1
                    && mtc.is_none_or(|r| r.1 != physical)
                {
                    if let Some(p) = ports.iter_mut().find(|p| p.number == physical) {
                        p.handle.close().map_err(pure_error)?;
                        p.live.store(false, Ordering::Release);
                    }
                    ports.retain(|p| p.number != physical);
                }
                refs.remove(i);
                Ok(())
            })
            .map(|v| v.into_iter().map(Error::from).collect())
            .map_err(Error::from)
    }
    fn counts(&self) {
        self.shared
            .inputs
            .store(self.inputs.len(), Ordering::Release);
        self.shared
            .outputs
            .store(self.outputs.len(), Ordering::Release);
    }
    fn logical_room(&self) -> Result<()> {
        let (inputs, outputs, mtc) = self.bindings.counts();
        if inputs + outputs + usize::from(mtc) >= freeshow_midi::MAX_PORTS {
            Err(Error::limit())
        } else {
            Ok(())
        }
    }
    pub fn publish_listeners(&self) {
        let mut listeners = self.input_refs.iter().map(|r| r.2).collect::<Vec<_>>();
        if let Some((listener, _, _)) = self.mtc {
            listeners.push(listener);
        }
        self.shared.listeners(listeners);
    }
    pub fn command(&mut self, command: Command) -> Result<Reply> {
        let reply = match command {
            Command::Devices(direction) => {
                let devices = self.devices(direction)?;
                Reply::Devices(freeshow_midi::device_names(
                    &devices
                        .into_iter()
                        .map(|d| serde_json::json!({"name":d.name}))
                        .collect::<Vec<_>>(),
                )?)
            }
            Command::Send(mut data) => {
                let plan = freeshow_midi::prepare_send(&mut data)?;
                let token = if let Some(token) = self.bindings.output(plan.output.as_ref())? {
                    token
                } else {
                    self.logical_room()?;
                    let id = self.output(&Selector::from_field(plan.output.as_ref())?)?;
                    let token = self.bindings.opened_output(plan.output.as_ref())?;
                    self.output_refs.push((token.clone(), id));
                    token
                };
                let device = self
                    .output_refs
                    .iter()
                    .find(|r| r.0 == token)
                    .unwrap()
                    .1
                    .clone();
                let packet = plan.packet()?;
                self.packet(&device, &packet)?;
                Reply::Done
            }
            Command::Listen(data) => {
                if data.is_null() {
                    return Err(Error::new(
                        "midi_invalid",
                        "Cannot read properties of null (reading 'input')",
                    ));
                }
                if self.bindings.listen(data.get("id"), data.get("input"))? == InputDecision::Open {
                    self.logical_room()?;
                    if self.input_refs.len() == INPUT_LIMIT {
                        return Err(Error::limit());
                    }
                    let physical = self.input(&Selector::from_field(data.get("input"))?)?;
                    let has_name = self
                        .inputs
                        .iter()
                        .find(|p| p.number == physical)
                        .is_some_and(|p| !p.device.name.is_empty());
                    let token =
                        self.bindings
                            .opened_input(data.get("id"), data.get("input"), has_name)?;
                    let listener = self.next();
                    let fence = self.shared.sequence.load(Ordering::Acquire);
                    self.input_refs.push((token, physical, listener, fence));
                    self.arm_input(physical);
                }
                Reply::Done
            }
            Command::CloseInputs(id) => {
                let errors = Self::close_input_refs(
                    &mut self.bindings,
                    &mut self.input_refs,
                    &mut self.inputs,
                    self.mtc,
                    &id,
                )?;
                // Source close-all forgets failed bindings. Keep those actual
                // native handles on this terminal owner until real cleanup.
                if self.bindings.counts().0 == 0 && !errors.is_empty() {
                    self.shared.fail(errors[0].clone());
                }
                Reply::Diagnostics(errors)
            }
            Command::Stop => {
                let mut errors = Self::close_input_refs(
                    &mut self.bindings,
                    &mut self.input_refs,
                    &mut self.inputs,
                    self.mtc,
                    "",
                )?;
                let refs = &mut self.output_refs;
                let ports = &mut self.outputs;
                errors.extend(
                    self.bindings
                        .stop_midi(|token| {
                            let Some(i) = refs.iter().position(|r| r.0 == *token) else {
                                return Err(pure_error(Error::new(
                                    "midi_state",
                                    "Output binding missing",
                                )));
                            };
                            let device = refs[i].1.clone();
                            if refs.iter().filter(|r| r.1 == device).count() == 1 {
                                if let Some(p) = ports.iter_mut().find(|p| p.device.id == device) {
                                    p.handle.close().map_err(pure_error)?;
                                }
                                ports.retain(|p| p.device.id != device);
                            }
                            refs.remove(i);
                            Ok(())
                        })?
                        .into_iter()
                        .map(Error::from),
                );
                if let Some(error) = errors.first() {
                    self.shared.fail(error.clone());
                }
                Reply::Diagnostics(errors)
            }
            Command::MtcListen(selector) => {
                if self.mtc.is_some() {
                    return Err(Error::new(
                        "midi_mtc_active",
                        "MTC listener must close before replacement",
                    ));
                }
                let physical = self.input(&selector)?;
                let listener = self.next();
                let fence = self.shared.sequence.load(Ordering::Acquire);
                self.mtc = Some((listener, physical, fence));
                self.arm_input(physical);
                Reply::MtcListener(listener)
            }
            Command::MtcClose(listener) => {
                let Some((current, physical, _)) = self.mtc else {
                    return Err(Error::new("midi_closed", "MTC listener is closed"));
                };
                if current != listener {
                    return Err(Error::new("midi_closed", "Obsolete MTC listener handle"));
                }
                if !self.input_refs.iter().any(|r| r.1 == physical) {
                    if let Some(p) = self.inputs.iter_mut().find(|p| p.number == physical) {
                        p.handle.close()?;
                        p.live.store(false, Ordering::Release);
                    }
                    self.inputs.retain(|p| p.number != physical);
                }
                self.mtc = None;
                Reply::Done
            }
            Command::MtcSend {
                selector,
                time_ms,
                framerate,
            } => {
                let token = if let Some(token) = self.bindings.mtc_output()? {
                    token
                } else {
                    self.logical_room()?;
                    let device = self.output(&selector)?;
                    let token = self.bindings.opened_mtc_output()?;
                    self.output_refs.push((token.clone(), device));
                    token
                };
                let device = self
                    .output_refs
                    .iter()
                    .find(|r| r.0 == token)
                    .unwrap()
                    .1
                    .clone();
                for packet in freeshow_midi::mtc_packets(time_ms, framerate) {
                    self.packet(&device, &packet)?;
                }
                Reply::Done
            }
            Command::MtcStop => {
                let refs = &mut self.output_refs;
                let ports = &mut self.outputs;
                self.bindings.stop_mtc(|token| {
                    let Some(i) = refs.iter().position(|r| r.0 == *token) else {
                        return Err(pure_error(Error::new(
                            "midi_state",
                            "MTC output binding missing",
                        )));
                    };
                    let device = refs[i].1.clone();
                    if refs.iter().filter(|r| r.1 == device).count() == 1 {
                        if let Some(p) = ports.iter_mut().find(|p| p.device.id == device) {
                            p.handle.close().map_err(pure_error)?;
                        }
                        ports.retain(|p| p.device.id != device);
                    }
                    refs.remove(i);
                    Ok(())
                })?;
                Reply::Done
            }
        };
        self.counts();
        Ok(reply)
    }
    pub fn messages(&self, raw: Raw) -> Result<Vec<Event>> {
        if !self.inputs.iter().any(|p| p.number == raw.physical) {
            return Ok(vec![]);
        }
        let bytes = &raw.bytes[..raw.len];
        let description = match bytes[0] & 0xf0 {
            0x80 => "Note Off",
            0x90 => {
                if bytes.get(2).copied().unwrap_or(0) == 0 {
                    "Note Off"
                } else {
                    "Note On"
                }
            }
            // JZZ's description takes precedence over the raw control status in
            // receiveMidi: controller123 is classified as Note Off by source.
            0xb0 if bytes.get(1) == Some(&123) => "All Notes Off",
            _ => "",
        };
        let mut events = Vec::new();
        for (token, physical, listener, fence) in &self.input_refs {
            if *physical == raw.physical && raw.sequence > *fence {
                if let Some(data) = self.bindings.input_message(token, bytes, description)? {
                    events.push(Event::Midi {
                        owner: self.shared.owner.clone(),
                        listener: *listener,
                        data,
                    });
                }
            }
        }
        if let Some((listener, physical, fence)) = self.mtc {
            if physical == raw.physical && raw.sequence > fence {
                events.push(Event::Mtc {
                    owner: self.shared.owner.clone(),
                    listener,
                    bytes: bytes.to_vec(),
                    backend_timestamp_us: raw.timestamp,
                    received_unix_ms: raw.wall,
                });
            }
        }
        Ok(events)
    }
    pub fn health(&mut self) -> Result<()> {
        for direction in [Direction::Input, Direction::Output] {
            let present = match direction {
                Direction::Input => !self.inputs.is_empty(),
                Direction::Output => !self.outputs.is_empty(),
            };
            if !present {
                continue;
            }
            let devices = self.devices(direction)?;
            let missing = match direction {
                Direction::Input => self.inputs.iter().any(|p| {
                    !devices
                        .iter()
                        .any(|d| d.id == p.device.id && d.name == p.device.name)
                }),
                Direction::Output => self.outputs.iter().any(|p| {
                    !devices
                        .iter()
                        .any(|d| d.id == p.device.id && d.name == p.device.name)
                }),
            };
            if missing {
                return Err(Error::new(
                    "midi_disconnected",
                    "Native MIDI port disappeared or changed; owner must drain before reopen",
                ));
            }
        }
        Ok(())
    }
    pub fn shutdown(&mut self) -> Vec<Error> {
        let mut errors = Vec::new();
        for p in &mut self.inputs {
            p.live.store(false, Ordering::Release);
            if let Err(e) = p.handle.close() {
                errors.push(e);
            }
        }
        for p in &mut self.outputs {
            if let Err(e) = p.handle.close() {
                errors.push(e);
            }
        }
        self.inputs.clear();
        self.outputs.clear();
        self.input_refs.clear();
        self.output_refs.clear();
        self.mtc = None;
        self.counts();
        errors.truncate(32);
        errors
    }
}
