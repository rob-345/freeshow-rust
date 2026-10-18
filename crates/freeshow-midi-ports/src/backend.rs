use crate::*;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Input,
    Output,
}
#[derive(Debug, Clone, Serialize)]
pub struct Device {
    pub id: String,
    pub name: String,
}
pub type InputCallback = Arc<dyn Fn(u64, &[u8]) + Send + Sync>;
pub trait Input: Send {
    fn close(&mut self) -> Result<()>;
}
pub trait Output: Send {
    fn send(&mut self, bytes: &[u8]) -> Result<()>;
    fn close(&mut self) -> Result<()>;
}
/// Native-only backend seam for controlled tests. Methods run only on the fixed
/// handle owner; callbacks must never block and may race open/close return.
pub trait Backend: Send + 'static {
    fn devices(&mut self, direction: Direction) -> Result<Vec<Device>>;
    fn input(&mut self, device: &Device, callback: InputCallback) -> Result<Box<dyn Input>>;
    fn output(&mut self, device: &Device) -> Result<Box<dyn Output>>;
}
#[cfg(feature = "native")]
pub struct NativeBackend;
#[cfg(feature = "native")]
mod native {
    use super::*;
    use midir::{Ignore, MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};
    fn unavailable() -> Error {
        Error::new(
            "midi_backend_unavailable",
            "Native MIDI backend initialization failed (missing/denied ALSA, CoreMIDI or WinMM)",
        )
    }
    fn disconnected() -> Error {
        Error::new(
            "midi_disconnected",
            "Native MIDI device is missing, denied or disconnected",
        )
    }
    fn input() -> Result<MidiInput> {
        let mut input = MidiInput::new("FreeShow Rust MIDI").map_err(|_| unavailable())?;
        // Upstream ALSA checks this before extending its SysEx accumulator, not
        // only before callback delivery. Full-frame MTC is a remaining parity item.
        input.ignore(Ignore::Sysex);
        Ok(input)
    }
    fn output() -> Result<MidiOutput> {
        MidiOutput::new("FreeShow Rust MIDI").map_err(|_| unavailable())
    }
    fn count(count: usize) -> Result<()> {
        if count > DEVICE_LIMIT {
            Err(Error::limit())
        } else {
            Ok(())
        }
    }
    struct In(Option<MidiInputConnection<()>>);
    impl Input for In {
        fn close(&mut self) -> Result<()> {
            if let Some(c) = self.0.take() {
                c.close();
            }
            Ok(())
        }
    }
    struct Out(Option<MidiOutputConnection>);
    impl Output for Out {
        fn send(&mut self, bytes: &[u8]) -> Result<()> {
            self.0
                .as_mut()
                .ok_or_else(disconnected)?
                .send(bytes)
                .map_err(|_| disconnected())
        }
        fn close(&mut self) -> Result<()> {
            if let Some(c) = self.0.take() {
                c.close();
            }
            Ok(())
        }
    }
    impl Backend for NativeBackend {
        fn devices(&mut self, direction: Direction) -> Result<Vec<Device>> {
            let mut result = Vec::new();
            match direction {
                Direction::Input => {
                    let input = input()?;
                    count(input.port_count())?;
                    let ports = input.ports();
                    count(ports.len())?;
                    for p in ports {
                        result.push(Device {
                            id: p.id(),
                            name: input.port_name(&p).map_err(|_| disconnected())?,
                        });
                    }
                }
                Direction::Output => {
                    let output = output()?;
                    count(output.port_count())?;
                    let ports = output.ports();
                    count(ports.len())?;
                    for p in ports {
                        result.push(Device {
                            id: p.id(),
                            name: output.port_name(&p).map_err(|_| disconnected())?,
                        });
                    }
                }
            }
            Ok(result)
        }
        fn input(&mut self, device: &Device, callback: InputCallback) -> Result<Box<dyn Input>> {
            let input = input()?;
            count(input.port_count())?;
            let port = input.find_port_by_id(&device.id).ok_or_else(disconnected)?;
            let connection = input
                .connect(
                    &port,
                    "FreeShow Rust MIDI input",
                    move |time, bytes, _| callback(time, bytes),
                    (),
                )
                .map_err(|_| disconnected())?;
            Ok(Box::new(In(Some(connection))))
        }
        fn output(&mut self, device: &Device) -> Result<Box<dyn Output>> {
            let output = output()?;
            count(output.port_count())?;
            let port = output
                .find_port_by_id(&device.id)
                .ok_or_else(disconnected)?;
            let connection = output
                .connect(&port, "FreeShow Rust MIDI output")
                .map_err(|_| disconnected())?;
            Ok(Box::new(Out(Some(connection))))
        }
    }
}
