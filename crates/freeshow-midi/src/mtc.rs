use crate::{scalar::int32, Error, Number, MAX_MESSAGE_BYTES};
use serde::Serialize;
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Play,
    Pause,
    Stop,
}
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum TimecodeEffect {
    Frame(Number),
    Status(Status),
}
/// Shared across all timecode modes in source timecode.ts. Do not reset on stop.
#[derive(Debug, Default)]
pub struct StatusGate {
    last: Option<Status>,
}
impl StatusGate {
    pub fn update(&mut self, status: Status) -> Option<TimecodeEffect> {
        if self.last == Some(status) {
            None
        } else {
            self.last = Some(status);
            Some(TimecodeEffect::Status(status))
        }
    }
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MtcSnapshot {
    pub nibbles: [i32; 8],
    pub midi_clock_time: Number,
    pub last_tick_time: Number,
}
#[derive(Debug, Default)]
pub struct MtcReceiver {
    mtc: [i32; 8],
    midi_clock_time: f64,
    last_tick_time: f64,
}
impl MtcReceiver {
    pub fn snapshot(&self) -> MtcSnapshot {
        MtcSnapshot {
            nibbles: self.mtc,
            midi_clock_time: Number::new(self.midi_clock_time),
            last_tick_time: Number::new(self.last_tick_time),
        }
    }
    /// One ordered input stream per receiver. Explicit clock is Date.now() units;
    /// backward/non-finite samples retain source math rather than being clamped.
    pub fn feed(&mut self, msg: &[u8], now_ms: f64) -> Result<Vec<TimecodeEffect>, Error> {
        if msg.len() > MAX_MESSAGE_BYTES {
            return Err(Error::resource());
        }
        let byte = |i: usize| msg.get(i).copied();
        let n = |i: usize| byte(i).map_or(f64::NAN, f64::from);
        let b = |i: usize| byte(i).map_or(0, i32::from);
        let mut effects = Vec::with_capacity(2);
        match byte(0) {
            Some(0xf1) => {
                let data = b(1);
                let index = ((data >> 4) & 7) as usize;
                self.mtc[index] = data & 15;
                if index != 7 {
                    return Ok(effects);
                }
                let frames = (self.mtc[1] << 4) | self.mtc[0];
                let seconds = (self.mtc[3] << 4) | self.mtc[2];
                let minutes = (self.mtc[5] << 4) | self.mtc[4];
                let hours = ((self.mtc[7] & 1) << 4) | self.mtc[6];
                let fps = [24.0, 25.0, 29.97, 30.0][((self.mtc[7] >> 1) & 3) as usize];
                let time = (f64::from(hours * 3600 + minutes * 60 + seconds)
                    + f64::from(frames) / fps)
                    * 1000.0;
                self.midi_clock_time = time;
                effects.push(TimecodeEffect::Frame(Number::new(time)));
                effects.push(TimecodeEffect::Status(Status::Play));
            }
            Some(0xf0) if byte(1) == Some(0x7f) && byte(3) == Some(1) && byte(4) == Some(1) => {
                let hours = b(5) & 31;
                let code = (b(5) >> 5) & 3;
                let fps = [24.0, 25.0, 29.97, 30.0][code as usize];
                let time = (f64::from(hours) * 3600.0 + n(6) * 60.0 + n(7) + n(8) / fps) * 1000.0;
                self.midi_clock_time = time;
                effects.push(TimecodeEffect::Frame(Number::new(time)));
                self.mtc = [
                    b(8) & 15,
                    (b(8) >> 4) & 1,
                    b(7) & 15,
                    (b(7) >> 4) & 3,
                    b(6) & 15,
                    (b(6) >> 4) & 3,
                    hours & 15,
                    ((hours >> 4) & 1) | (code << 1),
                ];
            }
            Some(0xf2) => {
                let sixteenths = (b(2) << 7) | b(1);
                let time = (f64::from(sixteenths) / 120.0) * 15000.0;
                self.midi_clock_time = time;
                self.last_tick_time = 0.0;
                effects.push(TimecodeEffect::Frame(Number::new(time)));
            }
            Some(0xfa) => {
                effects.push(TimecodeEffect::Frame(Number::new(self.midi_clock_time)));
                self.last_tick_time = now_ms;
                effects.push(TimecodeEffect::Status(Status::Play));
            }
            Some(0xfb) => {
                self.last_tick_time = now_ms;
                effects.push(TimecodeEffect::Status(Status::Play));
            }
            Some(0xfc) => {
                self.last_tick_time = 0.0;
                effects.push(TimecodeEffect::Status(Status::Pause));
            }
            Some(0xf8) => {
                if self.last_tick_time > 0.0 {
                    let delta = now_ms - self.last_tick_time;
                    if delta < 500.0 {
                        self.midi_clock_time += delta;
                        effects.push(TimecodeEffect::Frame(Number::new(self.midi_clock_time)));
                    }
                }
                self.last_tick_time = now_ms;
                effects.push(TimecodeEffect::Status(Status::Play));
            }
            _ => (),
        }
        Ok(effects)
    }
}
/// Eight quarter frames in source order. No scheduler, full-frame compensation,
/// drop-frame correction or hardware send is implied by constructing these bytes.
pub fn mtc_packets(time_ms: f64, framerate: f64) -> [[u8; 2]; 8] {
    let hours = int32((time_ms / (1000.0 * 60.0 * 60.0) % 24.0).floor());
    let minutes = int32((time_ms / (1000.0 * 60.0) % 60.0).floor());
    let seconds = int32((time_ms / 1000.0 % 60.0).floor());
    let frames = int32(((time_ms % 1000.0) / 1000.0 * framerate).floor());
    // Source {24:0,...}[framerate] || 1 deliberately maps 24 to code 1.
    let code = if framerate == 29.97 {
        2
    } else if framerate == 30.0 {
        3
    } else {
        1
    };
    let values = [
        frames & 15,
        (frames >> 4) & 1,
        seconds & 15,
        (seconds >> 4) & 3,
        minutes & 15,
        (minutes >> 4) & 3,
        hours & 15,
        ((hours >> 4) & 1) | (code << 1),
    ];
    std::array::from_fn(|index| [0xf1, ((index as i32) << 4 | values[index]) as u8])
}
