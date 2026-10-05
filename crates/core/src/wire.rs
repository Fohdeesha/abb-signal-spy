pub const MAGIC: u16 = 0xA1A2;
pub const HEADER_LEN: usize = 24;
pub const MIN_FRAME: u32 = 25;
pub const MAX_FRAME: u32 = 512 * 1024;
pub const TRAILER: u8 = 0xDE;
pub const PAD: u8 = 0x70;

pub mod service {
    pub const REQUEST: u8 = 1;
    pub const RESPONSE: u8 = 2;
    pub const SEND: u8 = 3;
    pub const AYA: u8 = 4;
    pub const CONTROL: u8 = 6;
    pub const EVENT: u8 = 8;
}

pub mod cause {
    pub const EVENT: u8 = 1;
    pub const NOTICE: u8 = 2;
    pub const CMD: u8 = 4;
    pub const RESPONSE: u8 = 5;
    pub const AYA: u8 = 7;
}

pub mod rad_kind {
    pub const REPLY: u8 = 0x20;
    pub const SYSTEM_ID: u8 = 0x21;
    pub const REQUEST: u8 = 0x23;
}

pub mod rad_format {
    pub const TEXT: u8 = 1;
    pub const STATUS_TEXT: u8 = 2;
    pub const SUBSCRIBE: u8 = 3;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Desync {
    BadMagic(u16),
    TooShort(u32),
    TooLong(u32),
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
    Complete(usize),
    Desync(Desync),
}

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
    if buf.len() > 12 && buf[12] != 12 {
        return FrameStatus::Desync(Desync::BadHeader);
    }
    if (total as usize) > buf.len() {
        return FrameStatus::NeedMore;
    }
    FrameStatus::Complete(total as usize)
}

#[derive(Clone, Copy)]
pub struct Frame<'a> {
    bytes: &'a [u8],
}

impl<'a> Frame<'a> {
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
    pub fn has_trailer(&self) -> bool {
        self.bytes[self.bytes.len() - 1] == TRAILER
    }

    pub fn rads(&self) -> RadIter<'a> {
        RadIter {
            frame: self.bytes,
            end: self.bytes.len() - usize::from(self.has_trailer()),
            off: HEADER_LEN,
            left: self.rad_count(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rad<'a> {
    pub kind: u8,
    pub format: u8,
    pub data: &'a [u8],
}

pub struct RadIter<'a> {
    frame: &'a [u8],
    end: usize,
    off: usize,
    left: u8,
}

impl<'a> RadIter<'a> {
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

pub fn read_varint(b: &[u8], pos: &mut usize) -> Option<u64> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    loop {
        let &c = b.get(*pos)?;
        *pos += 1;
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

pub struct RadOut<'a> {
    pub kind: u8,
    pub format: u8,
    pub data: &'a [u8],
}

pub fn encode_frame(txn: u16, service: u8, cause: u8, ctrl1: u32, ctrl2: u32, rads: &[RadOut<'_>]) -> Vec<u8> {
    let rad_bytes: usize = rads.iter().map(|r| 4 + r.data.len()).sum();
    let total = HEADER_LEN + rad_bytes + 1;
    assert!(total <= MAX_FRAME as usize, "an outgoing frame of {total} bytes is not a frame");
    assert!(rads.len() <= u8::MAX as usize);
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&MAGIC.to_be_bytes());
    out.push(1);
    out.push(12);
    out.extend_from_slice(&(total as u32).to_be_bytes());
    out.extend_from_slice(&txn.to_be_bytes());
    out.push(service);
    out.push(0);
    out.push(12);
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

    const AYA: &str = "a1a2010c00000019000004000c00070000000fa000003e80de";
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
        assert_eq!(rv(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x02]), None);
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
        let b = frame_with(255, &rad(b"x"));
        assert_eq!(Frame::parse(&b).unwrap().rads().count(), 1);
        let b = frame_with(1, &[0x00, 0x03, 0x23, 0x02]);
        assert_eq!(Frame::parse(&b).unwrap().rads().count(), 0);
        let b = frame_with(1, &[0xFF, 0xFF, 0x23, 0x02, b'z']);
        assert_eq!(Frame::parse(&b).unwrap().rads().count(), 0);
        let b = frame_with(1, &[0x00, 0x05, 0x23, 0x02]);
        assert_eq!(Frame::parse(&b).unwrap().rads().count(), 0);
        let mut odd = rad(b"abc");
        odd.push(0x71);
        odd.extend(rad(b"z"));
        let b = frame_with(2, &odd);
        assert_eq!(Frame::parse(&b).unwrap().rads().count(), 1);
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
