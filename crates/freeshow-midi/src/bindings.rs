//! Small native-only bookkeeping for the source's cached ports. OS handles live
//! on the future host's bounded IO owner; a token never represents an open call.
use crate::{bounded_json, bounded_name, receive, Error, MidiValue, WireData, MAX_PORTS};
use serde_json::Value;
use std::sync::Arc;
#[derive(Clone, Debug)]
pub struct PortToken(Arc<()>);
impl PartialEq for PortToken {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for PortToken {}
#[derive(Debug, PartialEq, Eq)]
pub enum InputDecision {
    Open,
    Ignored,
    AlreadyOpen,
}
struct Input {
    key: String,
    id: Option<Value>,
    token: PortToken,
}
#[derive(Default)]
pub struct PortBindings {
    inputs: Vec<Input>,
    outputs: Vec<(String, PortToken)>,
    mtc: Option<PortToken>,
    closed: bool,
}
fn prototype(key: &str) -> bool {
    matches!(
        key,
        "__proto__"
            | "constructor"
            | "toString"
            | "toLocaleString"
            | "valueOf"
            | "hasOwnProperty"
            | "isPrototypeOf"
            | "propertyIsEnumerable"
            | "__defineGetter__"
            | "__defineSetter__"
            | "__lookupGetter__"
            | "__lookupSetter__"
    )
}
fn key(value: Option<&Value>) -> Result<String, Error> {
    if let Some(value) = value {
        bounded_json(value)?;
    }
    let key = MidiValue::from_json(value)?.text();
    bounded_name(&key)?;
    Ok(key)
}
fn index(key: &str) -> Option<u32> {
    let number = key.parse::<u32>().ok()?;
    if number == u32::MAX || number.to_string() != key {
        None
    } else {
        Some(number)
    }
}
fn order(keys: impl Iterator<Item = String>) -> Vec<usize> {
    let keys = keys.collect::<Vec<_>>();
    let mut indices = (0..keys.len()).collect::<Vec<_>>();
    indices.sort_by_key(|i| index(&keys[*i]).map_or((1, 0, *i), |n| (0, n, *i)));
    indices
}
fn diagnostic(mut error: Error) -> Error {
    if error.message.len() > 512 {
        let mut end = 512;
        while !error.message.is_char_boundary(end) {
            end -= 1;
        }
        error.message.truncate(end);
    }
    error
}
impl PortBindings {
    fn live(&self) -> Result<(), Error> {
        if self.closed {
            Err(Error::new(
                "midi_shutdown",
                "StateError",
                "MIDI binding owner is shut down",
            ))
        } else {
            Ok(())
        }
    }
    fn room(&self) -> Result<(), Error> {
        self.live()?;
        if self.inputs.len() + self.outputs.len() + usize::from(self.mtc.is_some()) >= MAX_PORTS {
            Err(Error::resource())
        } else {
            Ok(())
        }
    }
    pub fn output(&self, selector: Option<&Value>) -> Result<Option<PortToken>, Error> {
        self.live()?;
        let key = key(selector)?;
        if prototype(&key) {
            return Err(Error::new(
                "midi_unsupported",
                "UnsupportedError",
                "Prototype-shadowed MIDI output selector",
            ));
        }
        Ok(self
            .outputs
            .iter()
            .find(|(name, _)| name == &key)
            .map(|(_, token)| token.clone()))
    }
    /// Call only after a real backend open succeeds. Packet validation follows
    /// open/reuse in the source; do not preclaim an output when backend open fails.
    pub fn opened_output(&mut self, selector: Option<&Value>) -> Result<PortToken, Error> {
        if let Some(port) = self.output(selector)? {
            return Ok(port);
        }
        self.room()?;
        let token = PortToken(Arc::new(()));
        self.outputs.push((key(selector)?, token.clone()));
        Ok(token)
    }
    pub fn listen(
        &self,
        id: Option<&Value>,
        input: Option<&Value>,
    ) -> Result<InputDecision, Error> {
        self.live()?;
        if let Some(input) = input {
            bounded_json(input)?;
        }
        if !crate::truthy_json(input) {
            return Ok(InputDecision::Ignored);
        }
        let id = key(id)?;
        if prototype(&id) || self.inputs.iter().any(|item| item.key == id) {
            return Ok(InputDecision::AlreadyOpen);
        }
        if input.and_then(Value::as_str) == Some("input") {
            return Ok(InputDecision::Ignored);
        }
        Ok(InputDecision::Open)
    }
    /// An unnamed source input is not cached by Electron. Reject that unsupported
    /// leaking variant explicitly so a host can close its just-opened OS handle.
    pub fn opened_input(
        &mut self,
        id: Option<&Value>,
        input: Option<&Value>,
        has_name: bool,
    ) -> Result<PortToken, Error> {
        if self.listen(id, input)? != InputDecision::Open {
            return Err(Error::new(
                "midi_binding",
                "StateError",
                "Input was ignored or already bound",
            ));
        }
        if !has_name {
            return Err(Error::new(
                "midi_unsupported",
                "UnsupportedError",
                "Unnamed MIDI input cannot be tracked safely",
            ));
        }
        self.room()?;
        let token = PortToken(Arc::new(()));
        self.inputs.push(Input {
            key: key(id)?,
            id: id.cloned(),
            token: token.clone(),
        });
        Ok(token)
    }
    pub fn input_message(
        &self,
        token: &PortToken,
        bytes: &[u8],
        description: &str,
    ) -> Result<Option<WireData>, Error> {
        self.live()?;
        let input = self
            .inputs
            .iter()
            .find(|input| input.token == *token)
            .ok_or_else(|| {
                Error::new(
                    "midi_closed",
                    "StateError",
                    "MIDI input binding is no longer current",
                )
            })?;
        receive(input.id.as_ref(), bytes, description)
    }
    /// Source quirk: a nonempty unknown ID closes every input. A specific close
    /// failure keeps its binding; close-all clears the map even when closes fail.
    /// Run actual close callbacks on the host's IO owner, never its GUI thread.
    pub fn close_inputs(
        &mut self,
        id: &str,
        mut close: impl FnMut(&PortToken) -> Result<(), Error>,
    ) -> Result<Vec<Error>, Error> {
        bounded_name(id)?;
        self.live()?;
        if !id.is_empty() && prototype(id) {
            return Err(Error::new(
                "midi_unsupported",
                "UnsupportedError",
                "Prototype-shadowed MIDI input close",
            ));
        }
        if !id.is_empty() {
            if let Some(index) = self.inputs.iter().position(|input| input.key == id) {
                return match close(&self.inputs[index].token) {
                    Ok(()) => {
                        self.inputs.remove(index);
                        Ok(vec![])
                    }
                    Err(error) => Ok(vec![diagnostic(error)]),
                };
            }
        }
        let mut errors = vec![];
        for index in order(self.inputs.iter().map(|input| input.key.clone())) {
            if let Err(error) = close(&self.inputs[index].token) {
                errors.push(diagnostic(error));
            }
        }
        self.inputs.clear();
        Ok(errors)
    }
    pub fn stop_midi(
        &mut self,
        mut close: impl FnMut(&PortToken) -> Result<(), Error>,
    ) -> Result<Vec<Error>, Error> {
        let mut errors = self.close_inputs("", &mut close)?;
        for index in order(self.outputs.iter().map(|item| item.0.clone())) {
            if let Err(error) = close(&self.outputs[index].1) {
                errors.push(diagnostic(error));
            }
        }
        self.outputs.clear();
        Ok(errors)
    }
    pub fn mtc_output(&self) -> Result<Option<PortToken>, Error> {
        self.live()?;
        Ok(self.mtc.clone())
    }
    /// MTC's first output is retained independently of ordinary MIDI outputs.
    /// Later requested selectors are ignored until stopSendMTC, as in source.
    pub fn opened_mtc_output(&mut self) -> Result<PortToken, Error> {
        self.live()?;
        if let Some(token) = &self.mtc {
            return Ok(token.clone());
        }
        self.room()?;
        let token = PortToken(Arc::new(()));
        self.mtc = Some(token.clone());
        Ok(token)
    }
    /// Source stopSendMTC clears its port only after close succeeds.
    pub fn stop_mtc(
        &mut self,
        close: impl FnOnce(&PortToken) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.live()?;
        if let Some(token) = &self.mtc {
            close(token).map_err(diagnostic)?;
            self.mtc = None;
        }
        Ok(())
    }
    /// Terminal host lifecycle, not Electron's reopenable stopMidi command.
    pub fn shutdown(
        &mut self,
        mut close: impl FnMut(&PortToken) -> Result<(), Error>,
    ) -> Vec<Error> {
        if self.closed {
            return vec![];
        }
        let mut errors = self.stop_midi(&mut close).unwrap_or_else(|e| vec![e]);
        if let Err(error) = self.stop_mtc(&mut close) {
            errors.push(error);
        }
        self.mtc = None;
        self.closed = true;
        errors
    }
    pub fn counts(&self) -> (usize, usize, bool) {
        (self.inputs.len(), self.outputs.len(), self.mtc.is_some())
    }
}
