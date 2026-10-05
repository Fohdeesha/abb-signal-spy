use crate::wire::{cause, encode_frame, rad_format, rad_kind, service, RadOut};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Axis(u8);

impl Axis {
    pub const MAX: u8 = 6;

    pub fn new(one_based: u8) -> Option<Axis> {
        (1..=Self::MAX).contains(&one_based).then_some(Axis(one_based))
    }
    pub fn one_based(self) -> u8 {
        self.0
    }
    pub fn wire(self) -> u8 {
        self.0 - 1
    }
}

impl TryFrom<u8> for Axis {
    type Error = String;
    fn try_from(v: u8) -> Result<Self, Self::Error> {
        Axis::new(v).ok_or_else(|| format!("axis {v} is outside 1..={}", Axis::MAX))
    }
}

impl From<Axis> for u8 {
    fn from(a: Axis) -> u8 {
        a.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MechUnit(String);

impl MechUnit {
    pub const MAX_LEN: usize = 32;

    pub fn new(name: &str) -> Result<MechUnit, String> {
        let n = name.trim();
        if n.is_empty() {
            return Err("the mechanical unit name is empty".into());
        }
        if n.len() > Self::MAX_LEN {
            return Err(format!("the mechanical unit name is longer than {} characters", Self::MAX_LEN));
        }
        if !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!("\"{n}\" is not a mechanical unit name (letters, digits and _ only)"));
        }
        Ok(MechUnit(n.to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for MechUnit {
    type Error = String;
    fn try_from(v: String) -> Result<Self, Self::Error> {
        MechUnit::new(&v)
    }
}

impl From<MechUnit> for String {
    fn from(m: MechUnit) -> String {
        m.0
    }
}

impl std::fmt::Display for MechUnit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Define {
    pub channel: u8,
    pub signal: u32,
    pub unit: MechUnit,
    pub axis: Axis,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    SetProtocolProtobuf,
    StreamConnect,
    Define(Define),
    StartStream,
    StopStream,
    Undefine(u32),
    UndefineAll,
    StreamDisconnect,
}

impl Command {
    fn parts(&self) -> (&'static str, &'static str, String) {
        match self {
            Command::SetProtocolProtobuf => ("SET", "SetProtocol", "-Value 1".into()),
            Command::StreamConnect => ("SET", "StreamConnect", "-Timeout 5000".into()),
            Command::Define(d) => (
                "GET",
                "StreamDefine",
                format!(
                    "-Channel {} -Signal {} -MechUnit {} -Axis {} -SampleRate 1 -Latency 1",
                    d.channel,
                    d.signal,
                    d.unit.as_str(),
                    d.axis.wire()
                ),
            ),
            Command::StartStream => ("SET", "StartStream", String::new()),
            Command::StopStream => ("SET", "StopStream", String::new()),
            Command::Undefine(id) => ("SET", "StreamUndefine", format!("-StreamId {id}")),
            Command::UndefineAll => ("SET", "StreamUndefineAll", String::new()),
            Command::StreamDisconnect => ("SET", "StreamDisconnect", String::new()),
        }
    }

    pub fn name(&self) -> &'static str {
        self.parts().1
    }

    pub fn frame(&self, txn: u16, host: &str) -> Vec<u8> {
        let (verb, prop, args) = self.parts();
        let mut d = Vec::with_capacity(64);
        let mut push = |s: &str| {
            d.extend(s.chars().map(latin1_byte));
            d.push(0);
        };
        push(verb);
        push(&format!("/{host}/INFOSTREAM"));
        push(prop);
        push(&args);
        d.push(0);
        encode_frame(txn, service::REQUEST, cause::CMD, 12, 0, &[RadOut { kind: rad_kind::REQUEST, format: rad_format::TEXT, data: &d }])
    }
}

fn latin1_byte(c: char) -> u8 {
    let v = c as u32;
    if v < 256 { v as u8 } else { b'?' }
}

pub fn hello(txn: u16) -> Vec<u8> {
    encode_frame(txn, service::CONTROL, cause::CMD, 1, 0, &[])
}

pub fn subscribe(txn: u16, host: &str) -> Vec<u8> {
    let mut d = Vec::with_capacity(48);
    d.extend_from_slice(b"SUBSCRIBE\0/");
    d.extend(host.chars().map(latin1_byte));
    d.extend_from_slice(b"/INFOSTREAM\0");
    d.extend_from_slice(b"\0\0");
    d.extend_from_slice(b"1\0");
    d.extend_from_slice(b"0\0");
    d.push(0);
    encode_frame(txn, service::REQUEST, cause::CMD, 12, 0, &[RadOut { kind: rad_kind::REQUEST, format: rad_format::SUBSCRIBE, data: &d }])
}

pub fn aya_reply(txn: u16, ctrl1: u32, ctrl2: u32) -> Vec<u8> {
    encode_frame(txn, service::AYA, cause::RESPONSE, ctrl1, ctrl2, &[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Frame;

    #[test]
    fn axis_is_one_based_and_zero_on_the_wire() {
        assert!(Axis::new(0).is_none());
        assert!(Axis::new(7).is_none());
        assert_eq!(Axis::new(1).unwrap().wire(), 0);
        assert_eq!(Axis::new(6).unwrap().wire(), 5);
        let d = Command::Define(Define { channel: 3, signal: 4002, unit: MechUnit::new("ROB_2").unwrap(), axis: Axis::new(2).unwrap() });
        let f = d.frame(9, "127.0.0.1");
        let fr = Frame::parse(&f).unwrap();
        let rad = fr.rads().next().unwrap();
        assert_eq!(
            rad.data,
            b"GET\0/127.0.0.1/INFOSTREAM\0StreamDefine\0-Channel 3 -Signal 4002 -MechUnit ROB_2 -Axis 1 -SampleRate 1 -Latency 1\0\0"
        );
        assert_eq!(fr.txn(), 9);
        assert_eq!(fr.service(), service::REQUEST);
        assert_eq!(fr.ctrl1(), 12);
    }

    #[test]
    fn mech_unit_rejects_injection() {
        assert!(MechUnit::new("ROB_1").is_ok());
        assert!(MechUnit::new(" ROB_1 ").is_ok());
        assert!(MechUnit::new("ROB_1 -Axis 3").is_err());
        assert!(MechUnit::new("").is_err());
        assert!(MechUnit::new("ROB\0").is_err());
        assert!(MechUnit::new(&"X".repeat(33)).is_err());
    }

    #[test]
    fn frames_match_the_reference_client() {
        let stop = Command::StopStream.frame(14, "127.0.0.1");
        let fr = Frame::parse(&stop).unwrap();
        assert_eq!(fr.rads().next().unwrap().data, b"SET\0/127.0.0.1/INFOSTREAM\0StopStream\0\0\0");
        let s = subscribe(12, "127.0.0.1");
        let fr = Frame::parse(&s).unwrap();
        let rad = fr.rads().next().unwrap();
        assert_eq!(rad.format, rad_format::SUBSCRIBE);
        assert_eq!(rad.data, b"SUBSCRIBE\0/127.0.0.1/INFOSTREAM\0\0\x001\x000\0\0");
        let h = hello(1);
        let fr = Frame::parse(&h).unwrap();
        assert_eq!((fr.service(), fr.cause(), fr.ctrl1(), fr.rad_count()), (6, 4, 1, 0));
        let a = aya_reply(0, 4000, 16000);
        let fr = Frame::parse(&a).unwrap();
        assert_eq!((fr.service(), fr.cause(), fr.txn(), fr.ctrl1(), fr.ctrl2()), (4, 5, 0, 4000, 16000));
    }
}
