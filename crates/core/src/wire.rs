//! RobAPI framing: the RLM/RAM headers, the RADs inside a frame, and protobuf varints.
//!
//! None of this is documented by ABB; all of it was checked byte for byte against a
//! real IRC5 and the RW6 virtual controller, and everything read off the socket is
//! treated as hostile. The rule: a length read off the wire is only ever compared with
//! the bytes that REMAIN, never added to a position first, so no value can wrap an
//! index; and every loop makes progress on every path.
//!
//! ```text
//! RLM header (12): u16 0xA1A2 | u8 version 1 | u8 header size 12 | u32 total length |
//!                  u16 transaction id | u8 service | u8 reserved
//! RAM header (12): u8 12 | u8 reserved | u8 cause | u8 RAD count | u32 ctrl1 | u32 ctrl2
//! RAD            : u16 length (header included) | u8 kind | u8 format << 1 | data
//! trailer    (1) : 0xDE
//! ```
//!
//! All integers are big-endian. The protobuf sample payload inside a RAD is the one
//! little-endian part, and it is decoded in [`crate::sample`].

/// First two bytes of every frame.
pub const MAGIC: u16 = 0xA1A2;
/// RLM (12) + RAM (12).
pub const HEADER_LEN: usize = 24;
/// The smallest frame that exists: the two headers and the trailer. The AYA
/// keepalive captured off the cell is exactly this long.
pub const MIN_FRAME: u32 = 25;
/// The largest frame accepted. Real frames are tiny (a sample frame carries a few
/// tens of bytes per stream); a header claiming more is a desync, never "wait".
pub const MAX_FRAME: u32 = 512 * 1024;
/// Last byte of every frame seen so far, from the cell and the VC.
pub const TRAILER: u8 = 0xDE;
/// Byte the controller pads some RADs with to a 4-byte boundary (ASCII `p`).
pub const PAD: u8 = 0x70;

/// RLM service numbers.
pub mod service {
    pub const REQUEST: u8 = 1;
    pub const RESPONSE: u8 = 2;
    /// Subscription notices (on the VC: a small notice after SUBSCRIBE, and event
    /// log sequence numbers). The notes once said samples arrive here; they do not.
    pub const SEND: u8 = 3;
    /// "Are you alive" keepalive. Must be answered, or the controller drops the
    /// connection after about 16 s.
    pub const AYA: u8 = 4;
    pub const CONTROL: u8 = 6;
    /// Sample frames on the RW6 VC (2026-09-24 and 2026-09-25). Samples are
    /// accepted whatever the service, so this is informational.
    pub const EVENT: u8 = 8;
}

/// RAM cause numbers.
pub mod cause {
    pub const EVENT: u8 = 1;
    pub const NOTICE: u8 = 2;
    pub const CMD: u8 = 4;
    pub const RESPONSE: u8 = 5;
    pub const AYA: u8 = 7;
}

/// RAD kind byte: requests use 0x23, command replies and sample events 0x20, the
/// handshake's system id 0x21.
pub mod rad_kind {
    pub const REPLY: u8 = 0x20;
    pub const SYSTEM_ID: u8 = 0x21;
    pub const REQUEST: u8 = 0x23;
}

/// RAD format (the byte carries it shifted left by one).
pub mod rad_format {
    /// Plain Latin-1 text, NUL terminated.
    pub const TEXT: u8 = 1;
    /// A u32 status word, then text.
    pub const STATUS_TEXT: u8 = 2;
    /// A subscription request.
    pub const SUBSCRIBE: u8 = 3;
    /// An event: status, subscription id, a 64-bit time, then the payload.
    pub const EVENT: u8 = 4;
}

#[inline]
pub fn be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}

#[inline]
pub fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// Why a buffer can never become a frame. There is no resync marker in this
/// protocol, so the caller drops the session rather than guess at a boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Desync {
    BadMagic(u16),
    /// Below [`MIN_FRAME`]: the bytes would sit in front of every later frame forever.
    TooShort(u32),
    /// Above [`MAX_FRAME`]: can never complete inside the receive buffer's cap.
    TooLong(u32),
    /// A header whose size fields are not the 12/12 every frame carries.
    BadHeader,
}

impl std::fmt::Display for Desync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Desync::BadMagic(m) => write!(f, "bad frame magic 0x{m:04X}"),
            Desync::TooShort(t) => write!(f, "frame length {t} is below the minimum {MIN_FRAME}"),
            Desync::TooLong(t) => write!(f, "frame length {t} is above the maximum {MAX_FRAME}"),
            Desync::BadHeader => write!(f, "frame header sizes are not 12/12"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameStatus {
    NeedMore,
    /// A whole frame of this many bytes starts at the front of the buffer. The
    /// length is within `[MIN_FRAME, buf.len()]`.
    Complete(usize),
    Desync(Desync),
}

/// Classify the frame at the front of `buf`.
pub fn frame_at(buf: &[u8]) -> FrameStatus {
    if buf.len() < 2 {
        return FrameStatus::NeedMore;
    }
    let magic = be16(buf);
    if magic != MAGIC {
        return FrameStatus::Desync(Desync::BadMagic(magic));
    }
    if buf.len() < 8 {
        return FrameStatus::NeedMore;
    }
    // Byte 3 is the RLM header size. Every frame from both controllers says 12; a
    // different value means the stream is not what we think it is.
    if buf[3] != 12 {
        return FrameStatus::Desync(Desync::BadHeader);
    }
    let total = be32(&buf[4..]);
    if total < MIN_FRAME {
        return FrameStatus::Desync(Desync::TooShort(total));
    }
    if total > MAX_FRAME {
        return FrameStatus::Desync(Desync::TooLong(total));
    }
    // The RAM header size, checked as soon as it is buffered.
    if buf.len() > 12 && buf[12] != 12 {
        return FrameStatus::Desync(Desync::BadHeader);
    }
    if (total as usize) > buf.len() {
        return FrameStatus::NeedMore;
    }
    FrameStatus::Complete(total as usize)
}

/// A borrowed view of one complete frame. Construct it only from a slice that
/// [`frame_at`] reported `Complete`; the accessors rely on the 25-byte minimum.
#[derive(Clone, Copy)]
pub struct Frame<'a> {
    bytes: &'a [u8],
}

impl<'a> Frame<'a> {
    /// `None` unless `bytes` is exactly one complete, well-formed frame.
    pub fn parse(bytes: &'a [u8]) -> Option<Frame<'a>> {
        match frame_at(bytes) {
            FrameStatus::Complete(n) if n == bytes.len() => Some(Frame { bytes }),
            _ => None,
        }
    }

    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }
    pub fn txn(&self) -> u16 {
        be16(&self.bytes[8..])
    }
    pub fn service(&self) -> u8 {
        self.bytes[10]
    }
    pub fn cause(&self) -> u8 {
        self.bytes[14]
    }
    pub fn rad_count(&self) -> u8 {
        self.bytes[15]
    }
    pub fn ctrl1(&self) -> u32 {
        be32(&self.bytes[16..])
    }
    pub fn ctrl2(&self) -> u32 {
        be32(&self.bytes[20..])
    }
    /// Whether the last byte is the 0xDE every observed frame ends with. A frame
    /// without it is still used (the length field is authoritative) but counted,
    /// so an unexpected controller shows up in the diagnostics rather than
    /// silently.
    pub fn has_trailer(&self) -> bool {
        self.bytes[self.bytes.len() - 1] == TRAILER
    }

    pub fn rads(&self) -> RadIter<'a> {
        RadIter {
            frame: self.bytes,
            // RADs live between the headers and the trailer byte; in a frame
            // without one, up to its end.
            end: self.bytes.len() - usize::from(self.has_trailer()),
            off: HEADER_LEN,
            left: self.rad_count(),
        }
    }
}

/// One RAD's kind, format and payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rad<'a> {
    pub kind: u8,
    pub format: u8,
    pub data: &'a [u8],
}

/// Walks the RADs of a frame. Stops at the first one whose length does not fit,
/// so a hostile count or length can only shorten the walk.
///
/// PADDING. Some RADs are padded to a 4-byte boundary with `p` bytes (measured on
/// the RW6 VC 2026-09-25: the handshake's system-id RAD and every kind-0x23 reply
/// are; kind-0x20 command replies are not). The padding is not counted in the RAD
/// length, so a walker that ignores it reads the next RAD one to three bytes early.
/// That is why both earlier clients never saw the handshake's second RAD, the
/// connected-client list: they read `70 00` as a 28672-byte length and stopped.
/// Here the next RAD starts at the 4-byte boundary when the gap is all `p` and a
/// plausible RAD header sits there; otherwise immediately after the previous one.
pub struct RadIter<'a> {
    frame: &'a [u8],
    end: usize,
    off: usize,
    left: u8,
}

impl<'a> RadIter<'a> {
    /// A RAD header at `at` whose length fits before `end`.
    fn header_fits(&self, at: usize) -> Option<usize> {
        if at > self.end || self.end - at < 4 {
            return None;
        }
        let rl = be16(&self.frame[at..]) as usize;
        if rl < 4 || rl > self.end - at {
            return None;
        }
        Some(rl)
    }
}

impl<'a> Iterator for RadIter<'a> {
    type Item = Rad<'a>;

    fn next(&mut self) -> Option<Rad<'a>> {
        if self.left == 0 {
            return None;
        }
        let rl = self.header_fits(self.off)?;
        let at = self.off;
        self.left -= 1;
        self.off = at + rl;

        // Where the next RAD starts, if there is one.
        if self.left > 0 {
            let aligned = (self.off + 3) & !3;
            if aligned != self.off
                && aligned <= self.end
                && self.frame[self.off..aligned].iter().all(|&b| b == PAD)
                && self.header_fits(aligned).is_some()
            {
                self.off = aligned;
            }
        }

        Some(Rad {
            kind: self.frame[at + 2],
            format: self.frame[at + 3] >> 1,
            data: &self.frame[at + 4..at + rl],
        })
    }
}

/// Protobuf varint at `b[*pos]`, at most 10 bytes, the tenth carrying only bit 63.
/// Advances `pos` past it. `None` on truncation or an over-long or overflowing
/// varint (`pos` is then unspecified).
pub fn read_varint(b: &[u8], pos: &mut usize) -> Option<u64> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    loop {
        let &c = b.get(*pos)?;
        *pos += 1;
        // The tenth byte lands at bit 63, so only its lowest bit fits and it must
        // be the last. Anything else overflows 64 bits: malformed, not large.
        if shift == 63 && c & 0xFE != 0 {
            return None;
        }
        result |= u64::from(c & 0x7F) << shift;
        if c & 0x80 == 0 {
            return Some(result);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

/// Append a protobuf varint.
pub fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// One RAD to encode.
pub struct RadOut<'a> {
    pub kind: u8,
    pub format: u8,
    pub data: &'a [u8],
}

/// Encode one frame. Requests are sent unpadded, the way both working clients
/// have always sent them to the cell and the VC.
pub fn encode_frame(txn: u16, service: u8, cause: u8, ctrl1: u32, ctrl2: u32, rads: &[RadOut<'_>]) -> Vec<u8> {
    let rad_bytes: usize = rads.iter().map(|r| 4 + r.data.len()).sum();
    let total = HEADER_LEN + rad_bytes + 1;
    assert!(total <= MAX_FRAME as usize, "an outgoing frame of {total} bytes is not a frame");
    assert!(rads.len() <= u8::MAX as usize);
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&MAGIC.to_be_bytes());
    out.push(1); // version
    out.push(12); // RLM header size
    out.extend_from_slice(&(total as u32).to_be_bytes());
    out.extend_from_slice(&txn.to_be_bytes());
    out.push(service);
    out.push(0);
    out.push(12); // RAM header size
    out.push(0);
    out.push(cause);
    out.push(rads.len() as u8);
    out.extend_from_slice(&ctrl1.to_be_bytes());
    out.extend_from_slice(&ctrl2.to_be_bytes());
    for r in rads {
        let len = 4 + r.data.len();
        assert!(len <= u16::MAX as usize, "a RAD of {len} bytes does not fit its length field");
        out.extend_from_slice(&(len as u16).to_be_bytes());
        out.push(r.kind);
        out.push(r.format << 1);
        out.extend_from_slice(r.data);
    }
    out.push(TRAILER);
    debug_assert_eq!(out.len(), total);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn hex(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    // The keepalive exactly as captured off the cell (2026-09-03) and the VC.
    const AYA: &str = "a1a2010c00000019000004000c00070000000fa000003e80de";
    // The RW6 VC's handshake reply, 2026-09-25 (its system id replaced by a made-up
    // one of the same length): a system-id RAD padded with one
    // 'p', then the client list padded with 'ppp', then one unexplained byte (0x3b)
    // before the trailer.
    const HELLO: &str = "a1a2010c0000007e000106000c0005020000010100000000002b21027b31323334353637382d394142432d344445462d383132332d3435363738394142434445467d0070003523023c693e3c63733e3c6320613d3132372e302e302e312f3e3c6320613d3132372e302e302e312f3e3c2f63733e3c2f693e007070703bde";

    #[test]
    fn varint_edges() {
        let rv = |b: &[u8]| {
            let mut p = 0;
            read_varint(b, &mut p).map(|v| (v, p))
        };
        assert_eq!(rv(&[0x00]), Some((0, 1)));
        assert_eq!(rv(&[0x96, 0x01]), Some((150, 2)));
        assert_eq!(rv(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01]), Some((u64::MAX, 10)));
        // A tenth byte carrying more than bit 63 overflows.
        assert_eq!(rv(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x02]), None);
        // Eleven bytes: the tenth still says "more".
        assert_eq!(rv(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x81, 0x00]), None);
        assert_eq!(rv(&[]), None);
        assert_eq!(rv(&[0x80]), None);
        assert_eq!(rv(&[0xFF, 0xFF, 0xFF]), None);
        for v in [0u64, 1, 127, 128, 300, 1 << 32, u64::MAX - 1, u64::MAX] {
            let mut b = Vec::new();
            put_varint(&mut b, v);
            assert_eq!(rv(&b), Some((v, b.len())), "round trip of {v}");
        }
    }

    #[test]
    fn frame_at_classifies() {
        let hdr = |total: u32| {
            let mut b = hex("a1a2010c");
            b.extend_from_slice(&total.to_be_bytes());
            b.extend_from_slice(&hex("00070100 0c000400 00000000 00000000 de"));
            b
        };
        assert_eq!(frame_at(&[]), FrameStatus::NeedMore);
        assert_eq!(frame_at(&[0xA1]), FrameStatus::NeedMore);
        assert_eq!(frame_at(&hdr(25)[..7]), FrameStatus::NeedMore);
        assert_eq!(frame_at(&[0x00, 0x00]), FrameStatus::Desync(Desync::BadMagic(0)));
        // THE STALL: a header whose length can never be a frame used to read as
        // "need more" and park every later frame behind it.
        assert_eq!(frame_at(&hdr(0)), FrameStatus::Desync(Desync::TooShort(0)));
        assert_eq!(frame_at(&hdr(24)), FrameStatus::Desync(Desync::TooShort(24)));
        assert_eq!(frame_at(&hdr(MAX_FRAME + 1)), FrameStatus::Desync(Desync::TooLong(MAX_FRAME + 1)));
        assert_eq!(frame_at(&hdr(u32::MAX)), FrameStatus::Desync(Desync::TooLong(u32::MAX)));
        assert_eq!(frame_at(&hdr(25)), FrameStatus::Complete(25));
        assert_eq!(frame_at(&hdr(26)), FrameStatus::NeedMore);
        assert_eq!(frame_at(&hdr(MAX_FRAME)), FrameStatus::NeedMore);
        let mut bad = hdr(25);
        bad[3] = 16;
        assert_eq!(frame_at(&bad), FrameStatus::Desync(Desync::BadHeader));
        let mut bad = hdr(25);
        bad[12] = 11;
        assert_eq!(frame_at(&bad), FrameStatus::Desync(Desync::BadHeader));
    }

    #[test]
    fn captured_aya_decodes() {
        let b = hex(AYA);
        let f = Frame::parse(&b).unwrap();
        assert_eq!(f.service(), service::AYA);
        assert_eq!(f.cause(), cause::AYA);
        assert_eq!(f.txn(), 0);
        assert_eq!(f.ctrl1(), 4000);
        assert_eq!(f.ctrl2(), 16000);
        assert_eq!(f.rads().count(), 0);
        assert!(f.has_trailer());
        // And encoding the same fields reproduces it byte for byte.
        assert_eq!(encode_frame(0, service::AYA, cause::AYA, 4000, 16000, &[]), b);
    }

    #[test]
    fn padded_rads_are_both_found() {
        let b = hex(HELLO);
        let f = Frame::parse(&b).unwrap();
        assert_eq!(f.service(), service::CONTROL);
        assert_eq!(f.cause(), cause::RESPONSE);
        let rads: Vec<_> = f.rads().collect();
        assert_eq!(rads.len(), 2, "the padded second RAD (the client list) must be found");
        assert_eq!(rads[0].kind, rad_kind::SYSTEM_ID);
        assert_eq!(rads[0].data, b"{12345678-9ABC-4DEF-8123-456789ABCDEF}\0");
        assert_eq!(rads[1].kind, rad_kind::REQUEST);
        assert_eq!(rads[1].format, rad_format::TEXT);
        assert_eq!(rads[1].data, b"<i><cs><c a=127.0.0.1/><c a=127.0.0.1/></cs></i>\0");
    }

    #[test]
    fn rad_walk_is_bounded() {
        let frame_with = |nrad: u8, body: &[u8]| {
            let mut o = hex("a1a2010c");
            o.extend_from_slice(&((24 + body.len() + 1) as u32).to_be_bytes());
            o.extend_from_slice(&hex("0001 0100 0c000400 00000000 00000000"));
            o[15] = nrad;
            o.extend_from_slice(body);
            o.push(TRAILER);
            o
        };
        let rad = |s: &[u8]| {
            let mut r = ((s.len() + 4) as u16).to_be_bytes().to_vec();
            r.extend_from_slice(&[0x23, 0x02]);
            r.extend_from_slice(s);
            r
        };
        let mut two = rad(b"ab");
        two.extend(rad(b"cde"));
        let b = frame_with(2, &two);
        let got: Vec<_> = Frame::parse(&b).unwrap().rads().map(|r| r.data.to_vec()).collect();
        assert_eq!(got, vec![b"ab".to_vec(), b"cde".to_vec()]);
        // A count larger than what is present stops at the frame's end.
        let b = frame_with(255, &rad(b"x"));
        assert_eq!(Frame::parse(&b).unwrap().rads().count(), 1);
        // A RAD length below its own header, or past the frame, stops the walk.
        let b = frame_with(1, &[0x00, 0x03, 0x23, 0x02]);
        assert_eq!(Frame::parse(&b).unwrap().rads().count(), 0);
        let b = frame_with(1, &[0xFF, 0xFF, 0x23, 0x02, b'z']);
        assert_eq!(Frame::parse(&b).unwrap().rads().count(), 0);
        // A RAD may not swallow the trailer.
        let b = frame_with(1, &[0x00, 0x05, 0x23, 0x02]);
        assert_eq!(Frame::parse(&b).unwrap().rads().count(), 0);
        // Padding that is not 'p' is not padding: the walk stays unaligned and
        // then finds nothing plausible.
        let mut odd = rad(b"abc"); // 7 bytes, next boundary is 1 byte on
        odd.push(0x71);
        odd.extend(rad(b"z"));
        let b = frame_with(2, &odd);
        assert_eq!(Frame::parse(&b).unwrap().rads().count(), 1);
        // A frame without the trailer (used, and counted): its last RAD runs to the
        // frame's end and is not cut short by a byte.
        let mut b = frame_with(2, &two);
        b.pop();
        let total = b.len() as u32;
        b[4..8].copy_from_slice(&total.to_be_bytes());
        let f = Frame::parse(&b).unwrap();
        assert!(!f.has_trailer());
        let got: Vec<_> = f.rads().map(|r| r.data.to_vec()).collect();
        assert_eq!(got, vec![b"ab".to_vec(), b"cde".to_vec()]);
    }

    #[test]
    fn parse_rejects_partial_and_extra() {
        let b = hex(AYA);
        assert!(Frame::parse(&b[..24]).is_none());
        let mut longer = b.clone();
        longer.push(0);
        assert!(Frame::parse(&longer).is_none());
    }
}
