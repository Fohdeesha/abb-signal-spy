//! The typed sample payload: what a streaming frame's RAD carries.
//!
//! Layout, as read off the RW6 VC's frames (captured 2026-09-24 and 2026-09-25):
//!
//! ```text
//! RAD data (kind 0x20, format 4):
//!   u32  status                     0 on every sample frame seen
//!   u32  subscription id            what the SUBSCRIBE reply returned
//!   u64  time                       two u32 words, low then high: a FILETIME on the VC
//!   "protobuf"
//!   u32  length                     big-endian; the records AND the end marker
//!   records:
//!     u8   type                     1 LogsrvFloatMsg, 2 LogsrvIntMsg, 3 LogsrvStringMsg, 100 end
//!     u16  length                   big-endian
//!     message (protobuf):
//!       field 1  varint             stream id (the controller's, not the -Channel asked for)
//!       field 2  varints            one controller-clock timestamp (ms) per sample
//!       field 3  float:  packed little-endian float32
//!                int:    varint int32 (a negative is a ten-byte varint)
//!                string: one length-delimited string per sample
//!   (then, on the VC, one stray byte after the end marker)
//! ```
//!
//! Both earlier decoders scanned for a float-message byte pattern instead. That read
//! float records only, so every integer-typed signal looked dead (54 of them on the
//! VC, all among the cell's 94 "valid but no data"), and it threw the timestamps
//! away. This decoder parses the records by type and pairs every sample with its
//! stamp, and rejects any record whose stamp and value counts disagree rather than
//! guess which sample a stamp belongs to.
//!
//! Every length is compared against the bytes remaining, never added to a position
//! first (the bridge's audit M34 rule).

use crate::wire::read_varint;

/// The marker that separates the event header from the records.
pub const MARKER: &[u8; 8] = b"protobuf";
/// Where the marker sits on the VC: after status, subscription id and the time.
pub const MARKER_OFFSET: usize = 16;
/// How far into a RAD the marker is looked for when it is not at its usual place.
/// Wider than the VC needs on purpose: the IRC5's header is not yet confirmed
/// (Phase 0 check 1), and a marker found elsewhere is reported, not hidden.
pub const MARKER_SEARCH: usize = 64;

pub const TYPE_FLOAT: u8 = 1;
pub const TYPE_INT: u8 = 2;
pub const TYPE_STRING: u8 = 3;
pub const TYPE_END: u8 = 100;

/// Longest string sample kept, in bytes. No string signal is known; this only
/// bounds what a hostile one can make us hold.
pub const MAX_STRING: usize = 256;

/// Record type as the controller labelled it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ValueKind {
    Float,
    Int,
    String,
}

impl ValueKind {
    pub fn label(self) -> &'static str {
        match self {
            ValueKind::Float => "float",
            ValueKind::Int => "int",
            ValueKind::String => "string",
        }
    }
}

/// One decoded record: every sample one stream contributed to one frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub stream: u32,
    pub kind: ValueKind,
    /// Controller clock, milliseconds, as sent. See [`crate::ctime`] for wrapping.
    pub stamps: Vec<u64>,
    pub values: RecordValues,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RecordValues {
    Float(Vec<f32>),
    Int(Vec<i64>),
    String(Vec<String>),
}

impl RecordValues {
    pub fn len(&self) -> usize {
        match self {
            RecordValues::Float(v) => v.len(),
            RecordValues::Int(v) => v.len(),
            RecordValues::String(v) => v.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The part of a sample RAD before the records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EventHeader {
    pub status: u32,
    pub subscription: u32,
    /// The two words after the subscription id, low word first. On the VC they are
    /// a Windows FILETIME; unconfirmed on an IRC5, so only reported, never used.
    pub time: u64,
    /// Where the marker was found; [`MARKER_OFFSET`] on every frame seen so far.
    pub marker_at: usize,
}

/// What went wrong with a sample RAD or one of its records. Counted per session and
/// shown in the diagnostics; none of them ends the session, because each record is
/// length-delimited and a bad one can be stepped over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Defect {
    /// The records' length word points past the RAD.
    LengthPastEnd,
    /// A record's length points past the records.
    RecordPastEnd,
    /// The records ran out without the type-100 end marker.
    NoEndMarker,
    /// A record type other than float, int, string or end. Stepped over.
    UnknownType(u8),
    /// A message that is not valid protobuf, or lacks a stream id.
    BadMessage,
    /// Stamps and values disagree in number; the record is dropped whole.
    CountMismatch,
    /// An int sample outside int32. Dropped with its record.
    IntRange,
    /// A timestamp of [`MAX_STAMP_MS`] or more: no controller has been up that long.
    /// Dropped with its record.
    ImplausibleStamp,
}

/// Timestamps are the controller's uptime in milliseconds (3.7e9 on the measured
/// cell after 42.9 days); 2^40 is 35 years. A stamp beyond it is garbage, and as a
/// session's first stamp it would set the chart timeline's origin.
pub const MAX_STAMP_MS: u64 = 1 << 40;

impl std::fmt::Display for Defect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Defect::LengthPastEnd => f.write_str("sample payload length points past its RAD"),
            Defect::RecordPastEnd => f.write_str("a sample record runs past the payload"),
            Defect::NoEndMarker => f.write_str("sample payload has no end marker"),
            Defect::UnknownType(t) => write!(f, "unknown sample record type {t}"),
            Defect::BadMessage => f.write_str("malformed sample record"),
            Defect::CountMismatch => f.write_str("a sample record's stamps and values disagree in number"),
            Defect::IntRange => f.write_str("an integer sample outside int32"),
            Defect::ImplausibleStamp => f.write_str("a timestamp beyond any controller's uptime"),
        }
    }
}

/// Whether a RAD is a sample payload, and where its marker is. `None` for anything
/// else (a command reply, a notice), which is how samples are recognised whatever
/// service carries them.
pub fn find_marker(rad: &[u8]) -> Option<usize> {
    if rad.len() >= MARKER_OFFSET + MARKER.len() && &rad[MARKER_OFFSET..MARKER_OFFSET + MARKER.len()] == MARKER {
        return Some(MARKER_OFFSET);
    }
    let window = &rad[..rad.len().min(MARKER_SEARCH + MARKER.len())];
    window.windows(MARKER.len()).position(|w| w == MARKER)
}

/// Decode one sample RAD. Good records go to `records`, problems to `defects`. The
/// header is returned whenever the marker was found, even if no record survived.
pub fn decode(rad: &[u8], records: &mut Vec<Record>, defects: &mut Vec<Defect>) -> Option<EventHeader> {
    let marker_at = find_marker(rad)?;
    let mut header = EventHeader { marker_at, ..EventHeader::default() };
    if marker_at == MARKER_OFFSET {
        header.status = u32::from_be_bytes(rad[0..4].try_into().unwrap());
        header.subscription = u32::from_be_bytes(rad[4..8].try_into().unwrap());
        let lo = u32::from_be_bytes(rad[8..12].try_into().unwrap());
        let hi = u32::from_be_bytes(rad[12..16].try_into().unwrap());
        header.time = (u64::from(hi) << 32) | u64::from(lo);
    }

    let mut pos = marker_at + MARKER.len();
    if rad.len() - pos < 4 {
        defects.push(Defect::LengthPastEnd);
        return Some(header);
    }
    let declared = u32::from_be_bytes(rad[pos..pos + 4].try_into().unwrap()) as usize;
    pos += 4;
    if declared > rad.len() - pos {
        defects.push(Defect::LengthPastEnd);
        return Some(header);
    }
    let body = &rad[pos..pos + declared];

    let mut at = 0usize;
    loop {
        if at >= body.len() {
            defects.push(Defect::NoEndMarker);
            break;
        }
        let typ = body[at];
        at += 1;
        if typ == TYPE_END {
            break;
        }
        if body.len() - at < 2 {
            defects.push(Defect::RecordPastEnd);
            break;
        }
        let len = u16::from_be_bytes([body[at], body[at + 1]]) as usize;
        at += 2;
        if len > body.len() - at {
            defects.push(Defect::RecordPastEnd);
            break;
        }
        let msg = &body[at..at + len];
        at += len;
        let kind = match typ {
            TYPE_FLOAT => ValueKind::Float,
            TYPE_INT => ValueKind::Int,
            TYPE_STRING => ValueKind::String,
            other => {
                defects.push(Defect::UnknownType(other));
                continue;
            }
        };
        match decode_message(kind, msg) {
            Ok(r) => records.push(r),
            Err(d) => defects.push(d),
        }
    }
    Some(header)
}

/// Skip one field of wire type `wt`. Groups (3, 4) never occur in these messages
/// and are refused rather than walked.
fn skip_field(msg: &[u8], pos: &mut usize, wt: u64) -> Result<(), Defect> {
    match wt {
        0 => read_varint(msg, pos).map(|_| ()).ok_or(Defect::BadMessage),
        1 => advance(msg, pos, 8),
        2 => {
            let n = read_varint(msg, pos).ok_or(Defect::BadMessage)?;
            advance(msg, pos, n)
        }
        5 => advance(msg, pos, 4),
        _ => Err(Defect::BadMessage),
    }
}

fn advance(msg: &[u8], pos: &mut usize, n: u64) -> Result<(), Defect> {
    if n > (msg.len() - *pos) as u64 {
        return Err(Defect::BadMessage);
    }
    *pos += n as usize;
    Ok(())
}

/// A length-delimited field's bytes.
fn delimited<'a>(msg: &'a [u8], pos: &mut usize) -> Result<&'a [u8], Defect> {
    let n = read_varint(msg, pos).ok_or(Defect::BadMessage)?;
    if n > (msg.len() - *pos) as u64 {
        return Err(Defect::BadMessage);
    }
    let s = &msg[*pos..*pos + n as usize];
    *pos += n as usize;
    Ok(s)
}

/// int32 as protobuf carries it: a varint of the sign-extended 64-bit value.
fn as_int32(v: u64) -> Result<i64, Defect> {
    let s = v as i64;
    if s < i64::from(i32::MIN) || s > i64::from(i32::MAX) {
        return Err(Defect::IntRange);
    }
    Ok(s)
}

/// One message. Repeated scalar fields are accepted packed or unpacked, as any
/// protobuf reader must; the VC sends them packed.
fn decode_message(kind: ValueKind, msg: &[u8]) -> Result<Record, Defect> {
    let mut stream: Option<u64> = None;
    let mut stamps = Vec::new();
    let mut values = match kind {
        ValueKind::Float => RecordValues::Float(Vec::new()),
        ValueKind::Int => RecordValues::Int(Vec::new()),
        ValueKind::String => RecordValues::String(Vec::new()),
    };
    let mut pos = 0usize;
    while pos < msg.len() {
        let tag = read_varint(msg, &mut pos).ok_or(Defect::BadMessage)?;
        let (field, wt) = (tag >> 3, tag & 7);
        match (field, wt) {
            (1, 0) => {
                // A repeated stream id would make the record ambiguous.
                if stream.is_some() {
                    return Err(Defect::BadMessage);
                }
                stream = Some(read_varint(msg, &mut pos).ok_or(Defect::BadMessage)?);
            }
            (2, 0) => stamps.push(read_varint(msg, &mut pos).ok_or(Defect::BadMessage)?),
            (2, 2) => {
                let packed = delimited(msg, &mut pos)?;
                let mut p = 0;
                while p < packed.len() {
                    stamps.push(read_varint(packed, &mut p).ok_or(Defect::BadMessage)?);
                }
            }
            (3, _) => match (&mut values, wt) {
                (RecordValues::Float(v), 5) => {
                    if msg.len() - pos < 4 {
                        return Err(Defect::BadMessage);
                    }
                    v.push(f32::from_le_bytes(msg[pos..pos + 4].try_into().unwrap()));
                    pos += 4;
                }
                (RecordValues::Float(v), 2) => {
                    let packed = delimited(msg, &mut pos)?;
                    if packed.len() % 4 != 0 {
                        return Err(Defect::BadMessage);
                    }
                    v.extend(packed.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())));
                }
                (RecordValues::Int(v), 0) => {
                    v.push(as_int32(read_varint(msg, &mut pos).ok_or(Defect::BadMessage)?)?);
                }
                (RecordValues::Int(v), 2) => {
                    let packed = delimited(msg, &mut pos)?;
                    let mut p = 0;
                    while p < packed.len() {
                        v.push(as_int32(read_varint(packed, &mut p).ok_or(Defect::BadMessage)?)?);
                    }
                }
                (RecordValues::String(v), 2) => {
                    let s = delimited(msg, &mut pos)?;
                    let s = &s[..s.len().min(MAX_STRING)];
                    // Latin-1, like every other text this protocol carries.
                    v.push(s.iter().map(|&b| b as char).collect());
                }
                _ => return Err(Defect::BadMessage),
            },
            _ => skip_field(msg, &mut pos, wt)?,
        }
    }
    let stream = stream.ok_or(Defect::BadMessage)?;
    // A uint32 field: a stream id beyond 32 bits is not a stream id, and truncating
    // it would alias another stream.
    let stream = u32::try_from(stream).map_err(|_| Defect::BadMessage)?;
    if stamps.len() != values.len() {
        return Err(Defect::CountMismatch);
    }
    if stamps.iter().any(|&s| s >= MAX_STAMP_MS) {
        return Err(Defect::ImplausibleStamp);
    }
    Ok(Record { stream, kind, stamps, values })
}

/// Build a sample RAD the way the RW6 VC does. Used by the fake controller and the
/// tests; the app never sends one.
pub fn encode_rad(subscription: u32, time: u64, records: &[Record], stray_byte: Option<u8>) -> Vec<u8> {
    use crate::wire::put_varint;
    let mut body = Vec::new();
    for r in records {
        let mut msg = Vec::new();
        msg.push(0x08);
        put_varint(&mut msg, u64::from(r.stream));
        let mut stamps = Vec::new();
        for &s in &r.stamps {
            put_varint(&mut stamps, s);
        }
        msg.push(0x12);
        put_varint(&mut msg, stamps.len() as u64);
        msg.extend_from_slice(&stamps);
        let typ = match &r.values {
            RecordValues::Float(v) => {
                msg.push(0x1A);
                put_varint(&mut msg, (v.len() * 4) as u64);
                for x in v {
                    msg.extend_from_slice(&x.to_le_bytes());
                }
                TYPE_FLOAT
            }
            RecordValues::Int(v) => {
                let mut packed = Vec::new();
                for &x in v {
                    put_varint(&mut packed, x as u64);
                }
                msg.push(0x1A);
                put_varint(&mut msg, packed.len() as u64);
                msg.extend_from_slice(&packed);
                TYPE_INT
            }
            RecordValues::String(v) => {
                for s in v {
                    msg.push(0x1A);
                    put_varint(&mut msg, s.len() as u64);
                    msg.extend(s.chars().map(|c| c as u32 as u8));
                }
                TYPE_STRING
            }
        };
        body.push(typ);
        body.extend_from_slice(&(msg.len() as u16).to_be_bytes());
        body.extend_from_slice(&msg);
    }
    body.push(TYPE_END);

    let mut rad = Vec::with_capacity(28 + body.len() + 1);
    rad.extend_from_slice(&0u32.to_be_bytes());
    rad.extend_from_slice(&subscription.to_be_bytes());
    rad.extend_from_slice(&(time as u32).to_be_bytes());
    rad.extend_from_slice(&((time >> 32) as u32).to_be_bytes());
    rad.extend_from_slice(MARKER);
    rad.extend_from_slice(&(body.len() as u32).to_be_bytes());
    rad.extend_from_slice(&body);
    if let Some(b) = stray_byte {
        rad.push(b);
    }
    rad
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{Frame, put_varint};

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    // Two sample frames from the RW6 VC, 2026-09-25: streams 213 (6040, TCP x),
    // 214 (4000 on axis 2) and 215 (6000) every 4 ms, plus 211 (9869) and 212 (9888),
    // integer-typed, every 24 ms.
    const FLOATS_ONLY: &str = "a1a2010c00000071000008000c00010100000000000000000058200800000000094bfb7caa5d7b8001dd4cf370726f746f6275660000003701000f08d5011204dd839f3d1a044f55713f01000f08d6011204dd839f3d1a04460b4e3501000f08d7011204dd839f3d1a040000000064adde";
    const WITH_INTS: &str = "a1a2010c0000008f000008000c00010100000000000000000076200800000000094bfb7caa5d7b8001dd4cf370726f746f6275660000005502000c08d3011204ed839f3d1a010002000c08d4011204ed839f3d1a010001000f08d5011204ed839f3d1a044f55713f01000f08d6011204ed839f3d1a04460b4e3501000f08d7011204ed839f3d1a040000000064adde";

    fn decode_frame(h: &str) -> (Vec<Record>, Vec<Defect>, EventHeader) {
        let b = hex(h);
        let f = Frame::parse(&b).expect("captured frame parses");
        let rad = f.rads().next().expect("one RAD");
        let (mut r, mut d) = (Vec::new(), Vec::new());
        let hdr = decode(rad.data, &mut r, &mut d).expect("a sample RAD");
        (r, d, hdr)
    }

    #[test]
    fn captured_float_frame() {
        let (recs, defects, hdr) = decode_frame(FLOATS_ONLY);
        assert!(defects.is_empty(), "{defects:?}");
        assert_eq!(hdr.subscription, 155_974_524);
        assert_eq!(hdr.status, 0);
        assert_eq!(hdr.marker_at, MARKER_OFFSET);
        assert_eq!(recs.len(), 3);
        assert_eq!(recs.iter().map(|r| r.stream).collect::<Vec<_>>(), vec![213, 214, 215]);
        for r in &recs {
            assert_eq!(r.kind, ValueKind::Float);
            assert_eq!(r.stamps, vec![128_434_653]);
        }
        let RecordValues::Float(v) = &recs[0].values else { panic!() };
        assert!((v[0] - 0.9427).abs() < 1e-3, "TCP x is {}", v[0]);
    }

    #[test]
    fn captured_int_records() {
        let (recs, defects, _) = decode_frame(WITH_INTS);
        assert!(defects.is_empty(), "{defects:?}");
        assert_eq!(recs.len(), 5);
        assert_eq!(recs[0].stream, 211);
        assert_eq!(recs[0].kind, ValueKind::Int);
        assert_eq!(recs[0].values, RecordValues::Int(vec![0]));
        assert_eq!(recs[1].stream, 212);
        assert_eq!(recs[1].kind, ValueKind::Int);
        // Same tick for every stream in the frame.
        assert!(recs.iter().all(|r| r.stamps == vec![128_434_669]));
    }

    #[test]
    fn not_a_sample_rad() {
        let (mut r, mut d) = (Vec::new(), Vec::new());
        assert!(decode(b"\x00\x04\x80\x00-StreamId 215 -SampleTime 4.032\x00", &mut r, &mut d).is_none());
        assert!(decode(b"", &mut r, &mut d).is_none());
        assert!(r.is_empty() && d.is_empty());
    }

    fn roundtrip(records: &[Record]) -> (Vec<Record>, Vec<Defect>) {
        let rad = encode_rad(7, 0x01DD_4CF3_AA5D_7B80, records, Some(0xAD));
        let (mut r, mut d) = (Vec::new(), Vec::new());
        let hdr = decode(&rad, &mut r, &mut d).unwrap();
        assert_eq!(hdr.subscription, 7);
        assert_eq!(hdr.time, 0x01DD_4CF3_AA5D_7B80);
        (r, d)
    }

    #[test]
    fn roundtrip_every_kind() {
        let recs = vec![
            Record { stream: 1, kind: ValueKind::Float, stamps: vec![10, 14, 18], values: RecordValues::Float(vec![1.5, -2.25, f32::NAN]) },
            Record { stream: 300, kind: ValueKind::Int, stamps: vec![10, 14], values: RecordValues::Int(vec![-1, i64::from(i32::MIN)]) },
            Record { stream: u32::MAX, kind: ValueKind::String, stamps: vec![22], values: RecordValues::String(vec!["zone".into()]) },
            Record { stream: 5, kind: ValueKind::Float, stamps: vec![], values: RecordValues::Float(vec![]) },
        ];
        let (got, defects) = roundtrip(&recs);
        assert!(defects.is_empty(), "{defects:?}");
        assert_eq!(got.len(), 4);
        assert_eq!(got[1], recs[1]);
        assert_eq!(got[2], recs[2]);
        let RecordValues::Float(v) = &got[0].values else { panic!() };
        assert_eq!(&v[..2], &[1.5, -2.25]);
        assert!(v[2].is_nan());
    }

    /// A message body with explicit fields, for the hostile cases.
    fn payload(records: &[(u8, Vec<u8>)], declared: Option<u32>, end: bool) -> Vec<u8> {
        let mut body = Vec::new();
        for (t, m) in records {
            body.push(*t);
            body.extend_from_slice(&(m.len() as u16).to_be_bytes());
            body.extend_from_slice(m);
        }
        if end {
            body.push(TYPE_END);
        }
        let mut rad = vec![0u8; 16];
        rad.extend_from_slice(MARKER);
        rad.extend_from_slice(&declared.unwrap_or(body.len() as u32).to_be_bytes());
        rad.extend_from_slice(&body);
        rad
    }

    fn run(rad: &[u8]) -> (Vec<Record>, Vec<Defect>) {
        let (mut r, mut d) = (Vec::new(), Vec::new());
        decode(rad, &mut r, &mut d).unwrap();
        (r, d)
    }

    fn float_msg(stream: u64, stamps: &[u64], vals: &[f32]) -> Vec<u8> {
        let mut m = vec![0x08];
        put_varint(&mut m, stream);
        let mut st = Vec::new();
        for &s in stamps {
            put_varint(&mut st, s);
        }
        m.push(0x12);
        put_varint(&mut m, st.len() as u64);
        m.extend(st);
        m.push(0x1A);
        put_varint(&mut m, (vals.len() * 4) as u64);
        for v in vals {
            m.extend_from_slice(&v.to_le_bytes());
        }
        m
    }

    #[test]
    fn hostile_lengths() {
        // Declared length past the RAD.
        let (r, d) = run(&payload(&[(1, float_msg(1, &[1], &[1.0]))], Some(u32::MAX), true));
        assert!(r.is_empty());
        assert_eq!(d, vec![Defect::LengthPastEnd]);
        // A record whose u16 length points past the payload: the good record before
        // it survives, the walk stops.
        let good = float_msg(1, &[1], &[1.0]);
        let mut rad = payload(&[(1, good.clone())], None, false);
        let before = rad.len();
        rad.extend_from_slice(&[1, 0xFF, 0xFF, 0x08]);
        let declared = (rad.len() - (MARKER_OFFSET + 12)) as u32;
        rad[MARKER_OFFSET + 8..MARKER_OFFSET + 12].copy_from_slice(&declared.to_be_bytes());
        assert!(rad.len() > before);
        let (r, d) = run(&rad);
        assert_eq!(r.len(), 1);
        assert_eq!(d, vec![Defect::RecordPastEnd]);
        // No end marker.
        let (r, d) = run(&payload(&[(1, good.clone())], None, false));
        assert_eq!(r.len(), 1);
        assert_eq!(d, vec![Defect::NoEndMarker]);
        // An unknown type is stepped over; the next record still decodes.
        let (r, d) = run(&payload(&[(9, vec![1, 2, 3]), (1, good.clone())], None, true));
        assert_eq!(r.len(), 1);
        assert_eq!(d, vec![Defect::UnknownType(9)]);
    }

    #[test]
    fn hostile_messages() {
        // Stamps and values disagree.
        let (r, d) = run(&payload(&[(1, float_msg(1, &[1, 2], &[1.0]))], None, true));
        assert!(r.is_empty());
        assert_eq!(d, vec![Defect::CountMismatch]);
        // No stream id.
        let mut m = float_msg(1, &[1], &[1.0]);
        m.drain(0..2);
        let (_, d) = run(&payload(&[(1, m)], None, true));
        assert_eq!(d, vec![Defect::BadMessage]);
        // A 33-bit stream id.
        let (_, d) = run(&payload(&[(1, float_msg(1 << 32, &[1], &[1.0]))], None, true));
        assert_eq!(d, vec![Defect::BadMessage]);
        // Packed floats that are not whole floats.
        let mut m = vec![0x08, 0x01, 0x12, 0x01, 0x01, 0x1A, 0x03, 1, 2, 3];
        let (_, d) = run(&payload(&[(1, m.clone())], None, true));
        assert_eq!(d, vec![Defect::BadMessage]);
        // A samples length that wraps (2^64 - 12): the M34 shape.
        m = vec![0x08, 0x29, 0x12, 0x00, 0x1A];
        put_varint(&mut m, u64::MAX - 11);
        m.extend_from_slice(&[0x55; 8]);
        let (_, d) = run(&payload(&[(1, m)], None, true));
        assert_eq!(d, vec![Defect::BadMessage]);
        // An int sample beyond int32.
        let mut m = vec![0x08, 0x01, 0x12, 0x01, 0x05, 0x1A];
        let mut packed = Vec::new();
        put_varint(&mut packed, 1 << 40);
        put_varint(&mut m, packed.len() as u64);
        m.extend(packed);
        let (_, d) = run(&payload(&[(2, m)], None, true));
        assert_eq!(d, vec![Defect::IntRange]);
        // A group wire type.
        let (_, d) = run(&payload(&[(1, vec![0x08, 0x01, 0x23])], None, true));
        assert_eq!(d, vec![Defect::BadMessage]);
        // Two stream ids.
        let (_, d) = run(&payload(&[(1, vec![0x08, 0x01, 0x08, 0x02])], None, true));
        assert_eq!(d, vec![Defect::BadMessage]);
    }

    #[test]
    fn a_timestamp_beyond_any_uptime_drops_its_record() {
        // A stamp near 2^64 as the first one of a session would pin the chart timeline
        // at its end for good; one of 35 years' uptime is no controller's clock.
        let good = float_msg(1, &[3_706_265_421, 3_706_265_425], &[1.0, 2.0]);
        let bad = float_msg(2, &[3_706_265_421, u64::MAX], &[1.0, 2.0]);
        let (r, d) = run(&payload(&[(1, bad), (1, good.clone())], None, true));
        assert_eq!(d, vec![Defect::ImplausibleStamp]);
        assert_eq!(r.len(), 1, "the good record after it survives");
        assert_eq!(r[0].stream, 1);
        let (r, d) = run(&payload(&[(1, float_msg(3, &[MAX_STAMP_MS], &[1.0]))], None, true));
        assert!(r.is_empty() && d == vec![Defect::ImplausibleStamp], "{d:?}");
        let (r, d) = run(&payload(&[(1, float_msg(3, &[MAX_STAMP_MS - 1], &[1.0]))], None, true));
        assert!(r.len() == 1 && d.is_empty(), "{d:?}");
    }

    #[test]
    fn unpacked_fields_and_unknown_fields() {
        // field 2 and 3 unpacked (wire types 0 and 5), plus an unknown field 7.
        let mut m = vec![0x08, 0x05, 0x10, 0x0A, 0x1D];
        m.extend_from_slice(&2.5f32.to_le_bytes());
        m.extend_from_slice(&[0x38, 0x01]);
        m.extend_from_slice(&[0x10, 0x0E, 0x1D]);
        m.extend_from_slice(&3.5f32.to_le_bytes());
        let (r, d) = run(&payload(&[(1, m)], None, true));
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(r[0].stamps, vec![10, 14]);
        assert_eq!(r[0].values, RecordValues::Float(vec![2.5, 3.5]));
    }

    #[test]
    fn every_truncation_is_safe() {
        let rad = encode_rad(1, 0, &[Record { stream: 41, kind: ValueKind::Float, stamps: vec![4, 8, 12], values: RecordValues::Float(vec![1.0, 2.0, 3.0]) }], None);
        for len in 0..rad.len() {
            let (mut r, mut d) = (Vec::new(), Vec::new());
            let _ = decode(&rad[..len], &mut r, &mut d);
            assert!(r.is_empty() || len == rad.len(), "a truncation at {len} produced a record");
        }
    }

    /// Seeded fuzz near the grammar: structural bytes, varint continuation bytes, and
    /// spliced fragments of valid payloads. Nothing may panic, and every record that
    /// comes out must be internally consistent.
    #[test]
    fn fuzz() {
        let mut state = 0x2026_0925_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let alphabet = [0x00u8, 0x01, 0x02, 0x03, 0x08, 0x10, 0x12, 0x1A, 0x1D, 0x64, 0x7F, 0x80, 0xFF];
        let seed_rad = encode_rad(9, 1, &[Record { stream: 3, kind: ValueKind::Int, stamps: vec![1, 2], values: RecordValues::Int(vec![-5, 6]) }], Some(0));
        for _ in 0..30_000 {
            let len = (next() % 160) as usize;
            let mut rad: Vec<u8> = (0..len)
                .map(|_| if next() % 4 == 0 { next() as u8 } else { alphabet[(next() % alphabet.len() as u64) as usize] })
                .collect();
            match next() % 3 {
                0 => {
                    let mut h = vec![0u8; 16];
                    h.extend_from_slice(MARKER);
                    h.extend_from_slice(&((next() % 200) as u32).to_be_bytes());
                    h.extend(rad);
                    rad = h;
                }
                1 => {
                    let at = if rad.is_empty() { 0 } else { (next() as usize) % rad.len() };
                    let cut = (next() as usize) % seed_rad.len();
                    rad.splice(at..at, seed_rad[..cut].iter().copied());
                }
                _ => {}
            }
            let (mut r, mut d) = (Vec::new(), Vec::new());
            let _ = decode(&rad, &mut r, &mut d);
            for rec in r {
                assert_eq!(rec.stamps.len(), rec.values.len());
            }
        }
    }
}
