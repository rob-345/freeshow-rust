//! Test/fixture binary backend only; never used by `MidiService::start`.
use super::*;
use std::sync::Condvar;
#[derive(Default)]
struct State {
    trace: Vec<Value>,
    inputs: Vec<Device>,
    outputs: Vec<Device>,
    callbacks: Vec<(String, InputCallback)>,
    block: Option<String>,
    entered: bool,
    close_fail: Option<String>,
}
#[derive(Clone, Default)]
pub struct Controlled {
    state: Arc<(Mutex<State>, Condvar)>,
}
impl Controlled {
    pub fn new(inputs: Vec<Device>, outputs: Vec<Device>) -> Self {
        let result = Self::default();
        {
            let mut state = lock(&result.state.0);
            state.inputs = inputs;
            state.outputs = outputs;
        }
        result
    }
    pub fn trace(&self) -> Vec<Value> {
        lock(&self.state.0).trace.clone()
    }
    fn record(state: &mut State, value: Value) {
        assert!(state.trace.len() < 4096);
        state.trace.push(value);
    }
    pub fn callback(&self, id: &str) -> Option<InputCallback> {
        lock(&self.state.0)
            .callbacks
            .iter()
            .find(|r| r.0 == id)
            .map(|r| r.1.clone())
    }
    pub fn emit(&self, id: &str, bytes: &[u8]) {
        if let Some(callback) = self.callback(id) {
            callback(1234, bytes);
        }
    }
    pub fn block(&self, kind: &str) {
        let mut state = lock(&self.state.0);
        state.block = Some(kind.into());
        state.entered = false;
    }
    pub fn entered(&self) -> bool {
        lock(&self.state.0).entered
    }
    pub fn release(&self) {
        lock(&self.state.0).block = None;
        self.state.1.notify_all();
    }
    pub fn fail_close(&self, name: &str) {
        lock(&self.state.0).close_fail = Some(name.into());
    }
    pub fn remove_inputs(&self) {
        lock(&self.state.0).inputs.clear();
    }
    fn wait(&self, kind: &str) {
        let mut state = lock(&self.state.0);
        if state.block.as_deref() == Some(kind) {
            state.entered = true;
            self.state.1.notify_all();
            while state.block.is_some() {
                state = self.state.1.wait(state).unwrap();
            }
        }
    }
}
struct ControlledInput {
    control: Controlled,
    device: Device,
    closed: bool,
}
impl Input for ControlledInput {
    fn close(&mut self) -> Result<()> {
        self.control.wait("close");
        let mut state = lock(&self.control.state.0);
        if self.closed {
            return Ok(());
        }
        Controlled::record(
            &mut state,
            serde_json::json!(["close", "in", self.device.name]),
        );
        if state.close_fail.as_deref() == Some(&self.device.name) {
            state.close_fail = None;
            return Err(Error::new("controlled_close", "Controlled close failure"));
        }
        state.callbacks.retain(|r| r.0 != self.device.id);
        self.closed = true;
        Ok(())
    }
}
struct ControlledOutput {
    control: Controlled,
    device: Device,
    closed: bool,
}
impl Output for ControlledOutput {
    fn send(&mut self, bytes: &[u8]) -> Result<()> {
        self.control.wait("send");
        let mut state = lock(&self.control.state.0);
        Controlled::record(
            &mut state,
            serde_json::json!(["send", self.device.name, bytes]),
        );
        Ok(())
    }
    fn close(&mut self) -> Result<()> {
        self.control.wait("close");
        let mut state = lock(&self.control.state.0);
        if self.closed {
            return Ok(());
        }
        Controlled::record(
            &mut state,
            serde_json::json!(["close", "out", self.device.name]),
        );
        if state.close_fail.as_deref() == Some(&self.device.name) {
            state.close_fail = None;
            return Err(Error::new("controlled_close", "Controlled close failure"));
        }
        self.closed = true;
        Ok(())
    }
}
impl Backend for Controlled {
    fn devices(&mut self, direction: Direction) -> Result<Vec<Device>> {
        let state = lock(&self.state.0);
        Ok(match direction {
            Direction::Input => state.inputs.clone(),
            Direction::Output => state.outputs.clone(),
        })
    }
    fn input(&mut self, device: &Device, callback: InputCallback) -> Result<Box<dyn Input>> {
        // Real native callbacks may fire before connect returns its handle.
        lock(&self.state.0)
            .callbacks
            .push((device.id.clone(), callback));
        self.wait("open");
        let mut state = lock(&self.state.0);
        Controlled::record(&mut state, serde_json::json!(["open", "in", device.name]));
        if device.name.starts_with("fail") {
            state.callbacks.retain(|row| row.0 != device.id);
            return Err(Error::new("controlled_open", "Controlled open failure"));
        }
        Ok(Box::new(ControlledInput {
            control: self.clone(),
            device: device.clone(),
            closed: false,
        }))
    }
    fn output(&mut self, device: &Device) -> Result<Box<dyn Output>> {
        self.wait("open");
        let mut state = lock(&self.state.0);
        Controlled::record(&mut state, serde_json::json!(["open", "out", device.name]));
        if device.name.starts_with("fail") {
            return Err(Error::new("controlled_open", "Controlled open failure"));
        }
        Ok(Box::new(ControlledOutput {
            control: self.clone(),
            device: device.clone(),
            closed: false,
        }))
    }
}
