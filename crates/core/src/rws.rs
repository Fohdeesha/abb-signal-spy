//! Read-only RWS 1.0 (IRC5, RobotWare 6) for the extras: the controller's name and
//! RobotWare version, its event log on the chart timeline, and a motor's calibration
//! values. **GETs only**: nothing here writes, and no request body is ever sent.
//!
//! What it rests on, measured on the RW6 VC 2026-09-27 and on an IRC5 2026-09-29: a
//! Digest login (RFC 2617, qop auth) answered with a session cookie that
//! carries later requests; JSON documents (`?json=1`) whose items are in
//! `_embedded._state`; times as the controller's local clock in whole seconds with no
//! zone (`2026-09-27 T 22:26:09`); the event log newest first, `limit` a page and
//! `start` the page number; and a `/logout` the VC refuses, so one session is kept.
//!
//! Credentials are never stored: the caller holds them for the session.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde_json::Value;

/// RWS 1.0's port on an IRC5 (a virtual controller's may differ).
pub const DEFAULT_PORT: u16 = 80;
/// RobotWare's default login, which most cells keep.
pub const DEFAULT_USER: &str = "Default User";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const IO_TIMEOUT: Duration = Duration::from_secs(5);
/// The largest answer read: an event-log page of 50 is about 32 KB.
const MAX_RESPONSE: usize = 4 << 20;

#[derive(Debug, Clone, PartialEq)]
pub enum RwsError {
    /// No connection, or it broke.
    Connect(String),
    /// The login was refused.
    Login,
    /// An HTTP status other than 200, with the controller's message where it gave one.
    Status(u16, String),
    /// An answer this client could not read.
    Format(String),
    /// RWS answered for another controller than the one expected (system ids).
    OtherController { found: String, expected: String },
}

impl std::fmt::Display for RwsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RwsError::Connect(e) => write!(f, "no connection to RWS ({e})"),
            RwsError::Login => write!(f, "the controller refused the login (user name or password)"),
            RwsError::Status(s, m) if m.is_empty() => write!(f, "the controller answered {s}"),
            RwsError::Status(s, m) => write!(f, "the controller answered {s}: {m}"),
            RwsError::Format(e) => write!(f, "an answer that could not be read ({e})"),
            RwsError::OtherController { found, expected } => write!(f, "a different controller answers (system id {found}), not the one expected ({expected})"),
        }
    }
}

// ------------------------------------------------------------------ MD5 (RFC 1321)

/// MD5, for the Digest login only (RFC 2617 needs it; nothing else here does).
pub fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6,
        10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64).map(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32).collect();
    let (mut a0, mut b0, mut c0, mut d0): (u32, u32, u32, u32) = (0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476);
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_le_bytes());
    for chunk in msg.chunks(64) {
        let m: Vec<u32> = chunk.chunks(4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])).collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    for (i, w) in [a0, b0, c0, d0].iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
    out
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn md5_hex(s: &str) -> String {
    hex(&md5(s.as_bytes()))
}

// ------------------------------------------------------------------ Digest (RFC 2617)

/// A Digest challenge (`WWW-Authenticate: Digest realm=..., nonce=..., qop=...`).
#[derive(Debug, Clone, PartialEq)]
pub struct Challenge {
    pub realm: String,
    pub nonce: String,
    pub qop: Option<String>,
    pub opaque: Option<String>,
}

/// The parameters of a `Digest` header, quoted or not; commas inside quotes kept. The
/// scheme in any case, and spaces around `=` (RFC 7235 allows both).
pub fn digest_params(header: &str) -> Option<Vec<(String, String)>> {
    let h = header.trim();
    let scheme = h.get(..6)?;
    let rest = &h[6..];
    if !scheme.eq_ignore_ascii_case("Digest") || !(rest.is_empty() || rest.starts_with(char::is_whitespace)) {
        return None;
    }
    let mut out = Vec::new();
    let mut chars = rest.trim_start().chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| *c == ',' || c.is_whitespace()) {
            chars.next();
        }
        let key: String = std::iter::from_fn(|| chars.next_if(|c| *c != '=' && *c != ',')).collect();
        if key.trim().is_empty() {
            break;
        }
        if chars.next() != Some('=') {
            return None;
        }
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let value = if chars.peek() == Some(&'"') {
            chars.next();
            let mut v = String::new();
            loop {
                match chars.next()? {
                    '\\' => v.push(chars.next()?),
                    '"' => break,
                    c => v.push(c),
                }
            }
            v
        } else {
            std::iter::from_fn(|| chars.next_if(|c| *c != ',')).collect::<String>().trim().to_string()
        };
        out.push((key.trim().to_ascii_lowercase(), value));
    }
    Some(out)
}

impl Challenge {
    pub fn parse(header: &str) -> Option<Challenge> {
        let p = digest_params(header)?;
        let get = |k: &str| p.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        if get("algorithm").is_some_and(|a| !a.eq_ignore_ascii_case("MD5")) {
            return None;
        }
        // qop "auth" (possibly listed with others); none: RFC 2069's form.
        let qop = get("qop").map(|q| if q.split(',').any(|x| x.trim() == "auth") { Some("auth".to_string()) } else { None });
        let qop = match qop {
            Some(None) => return None,
            Some(Some(q)) => Some(q),
            None => None,
        };
        Some(Challenge { realm: get("realm")?, nonce: get("nonce")?, qop, opaque: get("opaque") })
    }

    /// The `Authorization` header for a GET of `uri`.
    pub fn answer(&self, user: &str, password: &str, uri: &str, nc: u32, cnonce: &str) -> String {
        let ha1 = md5_hex(&format!("{user}:{}:{password}", self.realm));
        let ha2 = md5_hex(&format!("GET:{uri}"));
        let q = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let mut h = format!("Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", algorithm=MD5", q(user), q(&self.realm), q(&self.nonce), q(uri));
        match &self.qop {
            Some(qop) => {
                let response = md5_hex(&format!("{ha1}:{}:{nc:08x}:{cnonce}:{qop}:{ha2}", self.nonce));
                h += &format!(", response=\"{response}\", qop={qop}, nc={nc:08x}, cnonce=\"{cnonce}\"");
            }
            None => {
                let response = md5_hex(&format!("{ha1}:{}:{ha2}", self.nonce));
                h += &format!(", response=\"{response}\"");
            }
        }
        if let Some(o) = &self.opaque {
            h += &format!(", opaque=\"{}\"", q(o));
        }
        h
    }
}

// ------------------------------------------------------------------ HTTP

#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub status: u16,
    /// Names lower-cased, in the order sent.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }
}

/// A whole HTTP/1.1 response: the body by Content-Length, chunks, or to the end.
pub fn parse_response(raw: &[u8]) -> Result<Response, RwsError> {
    parse_prefix(raw, true)?.ok_or_else(|| RwsError::Format("an answer cut short".into()))
}

/// The response in `raw` once it is whole by its own framing (Content-Length, or the
/// last chunk), `None` while more is due. One framed by the close only (neither) is
/// whole at `at_end`.
fn parse_prefix(raw: &[u8], at_end: bool) -> Result<Option<Response>, RwsError> {
    let bad = |m: &str| RwsError::Format(m.to_string());
    let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") else { return Ok(None) };
    let head = std::str::from_utf8(&raw[..end]).map_err(|_| bad("headers not text"))?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().ok_or_else(|| bad("no status line"))?;
    let mut parts = status_line.splitn(3, ' ');
    if !parts.next().is_some_and(|v| v.starts_with("HTTP/1.")) {
        return Err(bad("not HTTP/1.x"));
    }
    let status: u16 = parts.next().and_then(|s| s.parse().ok()).ok_or_else(|| bad("no status code"))?;
    let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':')).map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_string())).collect();
    let rest = &raw[end + 4..];
    let chunked = headers.iter().any(|(n, v)| n == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked"));
    let body = if chunked {
        let mut body = Vec::new();
        let mut at = 0;
        loop {
            // Every index checked: a connection dropped mid-answer cuts anywhere.
            let Some(line_end) = rest.get(at..).and_then(|r| r.windows(2).position(|w| w == b"\r\n")).map(|p| p + at) else { return Ok(None) };
            let size_text = std::str::from_utf8(&rest[at..line_end]).map_err(|_| bad("a chunk size not text"))?;
            let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16).map_err(|_| bad("a chunk size not a number"))?;
            at = line_end + 2;
            if size == 0 {
                break;
            }
            let Some(chunk) = rest.get(at..at.checked_add(size).ok_or_else(|| bad("a chunk too large"))?) else { return Ok(None) };
            body.extend_from_slice(chunk);
            at += size + 2;
        }
        body
    } else if let Some(n) = headers.iter().find(|(n, _)| n == "content-length").map(|(_, v)| v.parse::<usize>().map_err(|_| bad("a Content-Length not a number"))).transpose()? {
        let Some(b) = rest.get(..n) else { return Ok(None) };
        b.to_vec()
    } else if at_end {
        rest.to_vec()
    } else {
        return Ok(None);
    };
    Ok(Some(Response { status, headers, body }))
}

/// The Digest challenge among an answer's `WWW-Authenticate` headers (a server may
/// offer Basic first).
pub fn challenge_of(r: &Response) -> Option<Challenge> {
    r.headers.iter().filter(|(n, _)| n == "www-authenticate").find_map(|(_, v)| Challenge::parse(v))
}

// ------------------------------------------------------------------ the controller's clock

/// How far before a recording's start a controller event is still kept: more than
/// the placing error of [`event_utc_ms`] (under a second either way, plus half a clock
/// read's round trip, at most [`MAX_CLOCK_RTT_MS`]).
pub const EVENT_SLACK_MS: i64 = 2000;

/// A clock read whose round trip took longer than this cannot say when the controller
/// read its clock closely enough to place events within the slack: not used.
pub const MAX_CLOCK_RTT_MS: i64 = 1000;

/// The controller's clock minus this PC's UTC, in ms, from a clock read of `ctrl_s`
/// (whole seconds, as the controller gives it) between two PC times. Its seconds cut
/// the true time down by up to a second: the middle of that second is taken, and the
/// middle of the request. `None` for a read that took longer than [`MAX_CLOCK_RTT_MS`].
pub fn clock_offset_ms(ctrl_s: i64, pc_before_ms: i64, pc_after_ms: i64) -> Option<i64> {
    let rtt = pc_after_ms.checked_sub(pc_before_ms)?;
    if !(0..=MAX_CLOCK_RTT_MS).contains(&rtt) {
        return None;
    }
    let pc_mid = pc_before_ms + rtt / 2;
    Some(ctrl_s.saturating_mul(1000).saturating_add(500).saturating_sub(pc_mid))
}

/// When an event stamped `stamp_s` on the controller's clock happened, in UTC ms: the
/// middle of its whole second, through the offset. Never more than a second (and half
/// the clock read's round trip) from when it happened.
pub fn event_utc_ms(stamp_s: i64, offset_ms: i64) -> i64 {
    stamp_s.saturating_mul(1000).saturating_add(500).saturating_sub(offset_ms)
}

/// The longest one request may take, however its bytes trickle in (each read has its
/// own timeout too).
const REQUEST_DEADLINE: Duration = Duration::from_secs(10);

/// One GET on a connection of its own (`Connection: close`), read until whole: the
/// VC closes as asked (measured 2026-09-28), but one that kept the connection open would
/// otherwise cost every request its timeout, and then the answer.
fn get_raw(host: &str, port: u16, path: &str, cookies: &str, auth: Option<&str>) -> Result<Response, RwsError> {
    get_raw_within(host, port, path, cookies, auth, REQUEST_DEADLINE)
}

fn get_raw_within(host: &str, port: u16, path: &str, cookies: &str, auth: Option<&str>, deadline: Duration) -> Result<Response, RwsError> {
    let until = std::time::Instant::now() + deadline;
    let addr = (host, port).to_socket_addrs().map_err(|e| RwsError::Connect(e.to_string()))?.next().ok_or_else(|| RwsError::Connect(format!("{host} has no address")))?;
    let mut s = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT.min(deadline)).map_err(|e| RwsError::Connect(e.to_string()))?;
    s.set_write_timeout(Some(IO_TIMEOUT)).map_err(|e| RwsError::Connect(e.to_string()))?;
    let mut req = format!("GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nAccept: application/json\r\nConnection: close\r\n");
    if !cookies.is_empty() {
        req += &format!("Cookie: {cookies}\r\n");
    }
    if let Some(a) = auth {
        req += &format!("Authorization: {a}\r\n");
    }
    req += "\r\n";
    s.write_all(req.as_bytes()).map_err(|e| RwsError::Connect(e.to_string()))?;
    let mut raw = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    let late = || RwsError::Connect(format!("no whole answer within {} s", deadline.as_secs_f64()));
    loop {
        let left = until.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Err(late());
        }
        s.set_read_timeout(Some(left.min(IO_TIMEOUT))).map_err(|e| RwsError::Connect(e.to_string()))?;
        match s.read(&mut buf) {
            Ok(0) => return parse_response(&raw),
            Ok(n) => raw.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) if std::time::Instant::now() >= until => return Err(late()),
            Err(e) => return Err(RwsError::Connect(e.to_string())),
        }
        if raw.len() > MAX_RESPONSE {
            return Err(RwsError::Format(format!("an answer over {} MB", MAX_RESPONSE >> 20)));
        }
        if let Some(r) = parse_prefix(&raw, false)? {
            return Ok(r);
        }
    }
}

// ------------------------------------------------------------------ the client

/// A read-only RWS session with one controller.
pub struct Client {
    host: String,
    port: u16,
    user: String,
    password: String,
    cookies: Vec<(String, String)>,
    logins: u32,
    expect: Option<String>,
    /// The session the cookies carry has been checked against `expect`.
    checked: bool,
}

const SYSTEM_PATH: &str = "/rw/system?json=1";

impl Client {
    pub fn new(host: &str, port: u16, user: &str, password: &str) -> Client {
        Client { host: host.to_string(), port, user: user.to_string(), password: password.to_string(), cookies: Vec::new(), logins: 0, expect: None, checked: false }
    }

    /// Read only from the controller with this system id. Nothing is returned from a
    /// session until `/rw/system` has been read in it and matched: at the start, after
    /// any new login (the controller forgot the session), and after a check that
    /// failed, whatever the reason. Another controller behind the same address (every
    /// IRC5's service port is 192.168.125.1) takes the same default login, and would
    /// otherwise be read as this one until the InfoStream side noticed. A session that
    /// failed its check stays unchecked, so every request after it fails too.
    pub fn expect_system(mut self, system_id: &str) -> Client {
        self.expect = Some(system_id.to_string());
        self
    }

    fn cookie_header(&self) -> String {
        self.cookies.iter().map(|(n, v)| format!("{n}={v}")).collect::<Vec<_>>().join("; ")
    }

    fn keep_cookies(&mut self, r: &Response) {
        for (n, v) in &r.headers {
            if n != "set-cookie" {
                continue;
            }
            if let Some((name, value)) = v.split(';').next().and_then(|p| p.split_once('=')) {
                let (name, value) = (name.trim().to_string(), value.trim().to_string());
                self.cookies.retain(|(n, _)| *n != name);
                self.cookies.push((name, value));
            }
        }
    }

    /// One GET, the JSON document. Logs in when the controller asks (a first request,
    /// or a session that expired), once per request, and then checks the controller
    /// (see [`Client::expect_system`]).
    pub fn get(&mut self, path: &str) -> Result<Value, RwsError> {
        if self.expect.is_none() {
            return self.fetch(path).map(|(doc, _)| doc);
        }
        if !self.checked {
            let sys = self.check()?;
            if path == SYSTEM_PATH {
                return Ok(sys);
            }
        }
        let (doc, logged_in) = self.fetch(path)?;
        if logged_in {
            // A new session began with this very request (the login left it unchecked):
            // what it read waits for the check.
            self.check()?;
        }
        Ok(doc)
    }

    /// Read `/rw/system` in the current session (logging in if asked) and match it
    /// against the expected system id; the document when it matches.
    fn check(&mut self) -> Result<Value, RwsError> {
        let expected = self.expect.clone().unwrap_or_default();
        let (sys, _) = self.fetch(SYSTEM_PATH)?;
        let found = system_in(&sys)?.system_id;
        if !found.eq_ignore_ascii_case(&expected) {
            return Err(RwsError::OtherController { found, expected });
        }
        self.checked = true;
        Ok(sys)
    }

    /// One GET, and whether it took a login.
    fn fetch(&mut self, path: &str) -> Result<(Value, bool), RwsError> {
        let mut auth: Option<String> = None;
        for _ in 0..2 {
            let r = get_raw(&self.host, self.port, path, &self.cookie_header(), auth.as_deref())?;
            self.keep_cookies(&r);
            match r.status {
                200 => return serde_json::from_slice(&r.body).map(|d| (d, auth.is_some())).map_err(|e| RwsError::Format(e.to_string())),
                401 if auth.is_none() => {
                    let challenge = challenge_of(&r).ok_or_else(|| RwsError::Format("a login it asks for in a way this client does not speak".into()))?;
                    // An expired session's cookie would only confuse the new login;
                    // whatever this answer set is kept for it.
                    self.cookies.clear();
                    self.keep_cookies(&r);
                    self.checked = false;
                    self.logins += 1;
                    let cnonce = format!("{:016x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0) ^ (u64::from(self.logins) << 48));
                    // A fresh nonce, answered once: its count is 1 (RFC 2617).
                    auth = Some(challenge.answer(&self.user, &self.password, path, 1, &cnonce));
                }
                401 => return Err(RwsError::Login),
                s => return Err(RwsError::Status(s, status_message(&r.body))),
            }
        }
        Err(RwsError::Login)
    }

    pub fn system(&mut self) -> Result<System, RwsError> {
        let doc = self.get(SYSTEM_PATH)?;
        system_in(&doc)
    }

    pub fn identity(&mut self) -> Result<Identity, RwsError> {
        let doc = self.get("/ctrl/identity?json=1")?;
        let s = items(&doc).first().ok_or_else(|| RwsError::Format("no identity".into()))?;
        Ok(Identity { name: text(s, "ctrl-name"), kind: text(s, "ctrl-type") })
    }

    /// The controller's clock, as seconds since 1970 read as if it were UTC (it has no
    /// zone: compare it with this PC's clock for the offset).
    pub fn clock(&mut self) -> Result<i64, RwsError> {
        let doc = self.get("/ctrl/clock?json=1")?;
        let s = items(&doc).first().ok_or_else(|| RwsError::Format("no clock".into()))?;
        let t = text(s, "datetime");
        controller_time(&t).ok_or_else(|| RwsError::Format(format!("the clock \"{t}\"")))
    }

    /// One page of the event log (domain 0: every event), newest first. An entry that
    /// cannot be read (an odd stamp or name) is counted and left out, not allowed to
    /// hide the rest of the page for as long as it stays on it.
    pub fn events(&mut self, limit: u32, page: u32) -> Result<Page, RwsError> {
        let doc = self.get(&format!("/rw/elog/0?lang=en&json=1&limit={limit}&start={page}"))?;
        let mut p = Page::default();
        for i in items(&doc).iter().filter(|i| i["_type"] == "elog-message") {
            match Event::from_json(i) {
                Ok(e) => p.events.push(e),
                Err(_) => p.unreadable += 1,
            }
        }
        Ok(p)
    }

    /// A motor's calibration (`MOC/MOTOR_CALIB`), by instance name (`rob1_2`).
    pub fn motor_calib(&mut self, instance: &str) -> Result<MotorCalib, RwsError> {
        if instance.is_empty() || !instance.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err(RwsError::Format(format!("\"{instance}\" is not a configuration instance name")));
        }
        let doc = self.get(&format!("/rw/cfg/MOC/MOTOR_CALIB/instances/{instance}?json=1"))?;
        let s = items(&doc).first().ok_or_else(|| RwsError::Format(format!("no {instance} in MOTOR_CALIB")))?;
        let attr = |name: &str| s["attrib"].as_array().and_then(|a| a.iter().find(|x| x["_title"] == name)).and_then(|x| x["value"].as_str()).map(str::to_string);
        let num = |name: &str| attr(name).and_then(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite()).ok_or_else(|| RwsError::Format(format!("{instance} has no number for {name}")));
        Ok(MotorCalib {
            instance: instance.to_string(),
            com_offset: num("com_offset")?,
            com_valid: attr("valid_com_offset").as_deref() == Some("true"),
            cal_offset: num("cal_offset")?,
            cal_valid: attr("valid_cal_offset").as_deref() == Some("true"),
        })
    }
}

fn system_in(doc: &Value) -> Result<System, RwsError> {
    let s = items(doc).iter().find(|i| i["_type"] == "sys-system-li").ok_or_else(|| RwsError::Format("no system in /rw/system".into()))?;
    Ok(System { name: text(s, "name"), rw_version: text(s, "rwversion"), system_id: text(s, "sysid") })
}

/// The controller's message in an error document, where it gave one.
fn status_message(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body).ok().and_then(|v| v["_embedded"]["status"]["msg"].as_str().map(str::to_string)).unwrap_or_else(|| String::from_utf8_lossy(body).trim().chars().take(200).collect())
}

fn items(doc: &Value) -> &[Value] {
    doc["_embedded"]["_state"].as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn text(v: &Value, key: &str) -> String {
    match &v[key] {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// `2026-09-27 T 22:26:09` (the controller's clock and its event stamps) as seconds
/// since 1970 read as if UTC.
pub fn controller_time(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, time) = s.split_once('T')?;
    let mut d = date.trim().split('-');
    let mut t = time.trim().split(':');
    let num = |it: &mut std::str::Split<'_, char>| -> Option<i64> {
        let x = it.next()?.trim();
        (!x.is_empty() && x.bytes().all(|b| b.is_ascii_digit())).then(|| x.parse().ok()).flatten()
    };
    let (y, mo, da) = (num(&mut d)?, num(&mut d)?, num(&mut d)?);
    let (h, mi, se) = (num(&mut t)?, num(&mut t)?, num(&mut t)?);
    // A year no controller's clock shows is refused before any arithmetic: SystemTime
    // panics past year 30827 on Windows, and a huge year overflows the day count.
    if d.next().is_some() || t.next().is_some() || !(1970..=9999).contains(&y) || !(1..=12).contains(&mo) || !(1..=31).contains(&da) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let days = crate::util::days_from_civil(y, mo as u32, da as u32);
    (crate::util::civil_from_days(days) == (y, mo as u32, da as u32)).then_some(days * 86_400 + h * 3600 + mi * 60 + se)
}

#[derive(Debug, Clone, PartialEq)]
pub struct System {
    pub name: String,
    pub rw_version: String,
    /// The same form as the InfoStream handshake's system id (measured).
    pub system_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Identity {
    pub name: String,
    /// "Virtual Controller" for a VC.
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// Rises with time (the last part of its `_title`).
    pub id: u64,
    pub code: u32,
    /// 1 information, 2 warning, 3 error.
    pub severity: u8,
    /// The controller's clock, as [`controller_time`] reads it.
    pub time: i64,
    pub title: String,
}

impl Event {
    fn from_json(v: &Value) -> Result<Event, RwsError> {
        let title = text(v, "_title");
        let id = title.rsplit('/').next().and_then(|x| x.parse().ok()).ok_or_else(|| RwsError::Format(format!("an event named \"{title}\"")))?;
        let stamp = text(v, "tstamp");
        Ok(Event {
            id,
            code: text(v, "code").parse().unwrap_or(0),
            severity: text(v, "msgtype").parse().unwrap_or(0),
            time: controller_time(&stamp).ok_or_else(|| RwsError::Format(format!("an event stamped \"{stamp}\"")))?,
            title: text(v, "title"),
        })
    }

    pub fn severity_word(&self) -> &'static str {
        severity_word(self.severity)
    }
}

/// An event log entry's `msgtype` for people.
pub fn severity_word(severity: u8) -> &'static str {
    match severity {
        1 => "information",
        2 => "warning",
        3 => "error",
        _ => "event",
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MotorCalib {
    pub instance: String,
    /// Commutator Offset (rad): what the resolver reads at the commutation position.
    pub com_offset: f64,
    pub com_valid: bool,
    pub cal_offset: f64,
    pub cal_valid: bool,
}

/// The `MOTOR_CALIB` instance of a robot's joint: `rob1_2` for ROB_1 axis 2. `None` for
/// a unit not named `ROB_<n>` (an additional axis's instance is named otherwise).
pub fn calib_instance(unit: &str, axis: u8) -> Option<String> {
    let n = unit.strip_prefix("ROB_")?;
    (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())).then(|| format!("rob{n}_{axis}"))
}

// ------------------------------------------------------------------ the event log, polled

/// Pages read at most per look: a burst larger than this in one look is said, not read.
pub const POLL_PAGES: u32 = 5;
/// Events a page, per look.
pub const POLL_LIMIT: u32 = 10;

/// One page of the event log.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Page {
    /// Newest first.
    pub events: Vec<Event>,
    /// Entries that could not be read, left out.
    pub unreadable: usize,
}

/// What is new in the event log since the last look.
#[derive(Debug, Default)]
pub struct EventPoll {
    newest: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewEvents {
    /// Oldest first.
    pub events: Vec<Event>,
    /// More arrived than one look reads: the oldest of them were not read.
    pub skipped: bool,
    /// Entries that could not be read, left out.
    pub unreadable: usize,
    /// The log's newest entry is older than the newest seen: it was cleared or
    /// renumbered, and this look read it afresh.
    pub renumbered: bool,
}

impl EventPoll {
    /// A poll that has seen up to `newest` already (a login again: what happened
    /// meanwhile is read back to it, not just the newest page).
    pub fn after(newest: u64) -> EventPoll {
        EventPoll { newest: Some(newest) }
    }

    /// Where the poll stands, to go back to (a look whose events cannot be used).
    pub fn mark(&self) -> Option<u64> {
        self.newest
    }

    /// Back to a mark: the next look reads again what was read since.
    pub fn restore(&mut self, mark: Option<u64>) {
        self.newest = mark;
    }

    /// The first look takes the newest page as it stands; later ones read back until
    /// an event already seen. A log whose newest entry is older than the newest seen
    /// was cleared or renumbered (its ids start again): read afresh, and said, rather
    /// than every later event taken for one already seen.
    pub fn look(&mut self, c: &mut Client) -> Result<NewEvents, RwsError> {
        let mut fresh: Vec<Event> = Vec::new();
        let mut reached = false;
        let mut unreadable = 0;
        let mut renumbered = false;
        for page in 1..=POLL_PAGES {
            let got = c.events(POLL_LIMIT, page)?;
            let n = got.events.len() + got.unreadable;
            unreadable += got.unreadable;
            if page == 1
                && let (Some(seen), Some(top)) = (self.newest, got.events.iter().map(|e| e.id).max())
                && top < seen
            {
                renumbered = true;
                self.newest = None;
            }
            for e in got.events {
                if self.newest.is_some_and(|seen| e.id <= seen) {
                    reached = true;
                    break;
                }
                // An event logged between two pages' reads shifts the next page down
                // by one: its first entry is then the last one already read.
                if fresh.iter().any(|f| f.id == e.id) {
                    continue;
                }
                fresh.push(e);
            }
            // Reached what was seen, a first look's page, or the log's end. Read afresh
            // after a clear, a full page may have had more behind it.
            if reached || self.newest.is_none() || n < POLL_LIMIT as usize {
                reached = !(renumbered && n >= POLL_LIMIT as usize);
                break;
            }
        }
        if let Some(top) = fresh.iter().map(|e| e.id).max() {
            self.newest = Some(self.newest.map_or(top, |s| s.max(top)));
        }
        fresh.reverse();
        Ok(NewEvents { events: fresh, skipped: !reached, unreadable, renumbered })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_as_rfc_1321_gives_it() {
        for (input, want) in [
            ("", "d41d8cd98f00b204e9800998ecf8427e"),
            ("a", "0cc175b9c0f1b6a831c399e269772661"),
            ("abc", "900150983cd24fb0d6963f7d28e17f72"),
            ("message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
            ("abcdefghijklmnopqrstuvwxyz", "c3fcd3d76192e4007dfb496cca67e13b"),
            ("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789", "d174ab98d277d9f5a5611c2c9f419d9f"),
            ("12345678901234567890123456789012345678901234567890123456789012345678901234567890", "57edf4a22be3c955ac49da2e2107b67a"),
        ] {
            assert_eq!(md5_hex(input), want, "{input:?}");
        }
    }

    #[test]
    fn a_digest_answer_as_rfc_2617_gives_it() {
        // The RFC's own example.
        let c = Challenge::parse(r#"Digest realm="testrealm@host.com", qop="auth,auth-int", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", opaque="5ccc069c403ebaf9f0171e9517f40e41""#).unwrap();
        assert_eq!(c.qop.as_deref(), Some("auth"));
        let h = c.answer("Mufasa", "Circle Of Life", "/dir/index.html", 1, "0a4f113b");
        assert!(h.contains("response=\"6629fae49393a05397450978507c4ef1\""), "{h}");
        assert!(h.contains("nc=00000001") && h.contains("cnonce=\"0a4f113b\"") && h.contains("opaque=\"5ccc069c403ebaf9f0171e9517f40e41\""), "{h}");
        // Unquoted values, a comma inside quotes, and what is not Digest.
        let p = digest_params(r#"Digest realm="a, b", nonce=xyz, qop=auth"#).unwrap();
        assert_eq!(p, vec![("realm".into(), "a, b".into()), ("nonce".into(), "xyz".into()), ("qop".into(), "auth".into())]);
        assert!(Challenge::parse("Basic realm=\"x\"").is_none());
        assert!(Challenge::parse(r#"Digest realm="x", nonce="n", algorithm=SHA-256"#).is_none(), "an algorithm this client cannot answer");
        assert!(Challenge::parse(r#"Digest realm="x", nonce="n", qop="auth-int""#).is_none());
    }

    #[test]
    fn an_answer_reads_by_length_by_chunks_or_to_the_end() {
        let r = parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nSet-Cookie: a=1; path=/\r\n\r\nhello-extra").unwrap();
        assert_eq!((r.status, r.body.as_slice(), r.header("set-cookie")), (200, &b"hello"[..], Some("a=1; path=/")));
        let r = parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nwiki\r\n5;x=y\r\npedia\r\n0\r\n\r\n").unwrap();
        assert_eq!(r.body, b"wikipedia");
        let r = parse_response(b"HTTP/1.0 400 Bad\r\n\r\nRAPI Unidentified Error").unwrap();
        assert_eq!((r.status, r.body.as_slice()), (400, &b"RAPI Unidentified Error"[..]));
        assert!(parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 50\r\n\r\nshort").is_err(), "cut short");
        assert!(parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nffff\r\nab").is_err());
        assert!(parse_response(b"garbage").is_err());
    }

    #[test]
    fn a_chunked_answer_cut_after_a_chunk_is_refused_not_a_panic() {
        // What a connection dropped mid-answer leaves.
        for cut in [&b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nab"[..], b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nab\r", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nab\r\n"] {
            assert!(parse_response(cut).is_err(), "{}", String::from_utf8_lossy(cut));
        }
    }

    /// One answer to one request, written in parts 150 ms apart, then the connection
    /// kept open for 8 s (a server that ignores `Connection: close`) or closed; its port.
    fn one_answer_server(parts: &'static [&'static [u8]], hold: bool) -> u16 {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = l.accept() {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                for (i, p) in parts.iter().enumerate() {
                    if i > 0 {
                        std::thread::sleep(Duration::from_millis(150));
                    }
                    let _ = s.write_all(p);
                }
                if hold {
                    std::thread::sleep(Duration::from_secs(8));
                }
            }
        });
        port
    }

    #[test]
    fn an_answer_is_taken_once_whole_though_the_connection_stays_open() {
        let held: [&'static [&'static [u8]]; 2] = [&[b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: Keep-Alive\r\n\r\n{", b"}"], &[b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n", b"0\r\n\r\n"]];
        for parts in held {
            let port = one_answer_server(parts, true);
            let t0 = std::time::Instant::now();
            let r = get_raw("127.0.0.1", port, "/", "", None).unwrap();
            assert_eq!(r.body, b"{}");
            assert!(t0.elapsed() < Duration::from_secs(2), "waited {:?} for a close that never came", t0.elapsed());
        }
        // No length and no chunks: the body runs to the close, not to the first read.
        let port = one_answer_server(&[b"HTTP/1.0 200 OK\r\n\r\n{\"a\"", b": 1}"], false);
        assert_eq!(get_raw("127.0.0.1", port, "/", "", None).unwrap().body, b"{\"a\": 1}");
    }

    #[test]
    fn a_controller_that_never_asks_for_a_login_is_checked_too() {
        let port = one_answer_server(&[b"HTTP/1.1 200 OK\r\nContent-Length: 99\r\n\r\n{\"_embedded\":{\"_state\":[{\"_type\":\"sys-system-li\",\"name\":\"B\",\"rwversion\":\"6.16\",\"sysid\":\"{BBBB}\"}]}}"], false);
        let mut c = Client::new("127.0.0.1", port, DEFAULT_USER, "robotics").expect_system("{AAAA}");
        let r = c.system();
        assert!(matches!(&r, Err(RwsError::OtherController { found, .. }) if found == "{BBBB}"), "{r:?}");
    }

    #[test]
    fn the_controllers_clock_reads_as_it_writes_it() {
        assert_eq!(controller_time("2026-09-27 T 22:26:09"), Some(1_790_547_969));
        assert_eq!(controller_time("1970-01-01 T 00:00:00"), Some(0));
        // A year no clock shows: refused, not carried into time arithmetic that panics
        // (SystemTime past year 30827 on Windows) or overflows.
        for bad in ["2026-02-30 T 10:00:00", "2026-09-27 22:26:09", "2026-09-27 T 25:00:00", "2026-09-27 T 22:26", "x", "40000-01-01 T 00:00:00", "99999999999999999-01-01 T 00:00:00", "1969-12-31 T 23:59:59"] {
            assert_eq!(controller_time(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_digest_challenge_reads_with_spaces_any_case_and_among_others() {
        let c = Challenge::parse(r#"digest realm = "validusers@robapi.abb" , nonce = "n1", qop = "auth""#).unwrap();
        assert_eq!((c.realm.as_str(), c.nonce.as_str(), c.qop.as_deref()), ("validusers@robapi.abb", "n1", Some("auth")));
        let r = parse_response(b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"x\"\r\nWWW-Authenticate: Digest realm=\"r\", nonce=\"n2\", qop=\"auth\"\r\nContent-Length: 0\r\n\r\n").unwrap();
        assert_eq!(challenge_of(&r).map(|c| c.nonce), Some("n2".to_string()), "the Digest one of several");
    }

    #[test]
    fn a_trickling_answer_ends_at_the_deadline() {
        // A byte every 300 ms, for ever: each read is inside the read timeout.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = l.accept() {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                for b in b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\n\r\n".iter().chain(std::iter::repeat(&b'x')) {
                    if s.write_all(&[*b]).is_err() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(300));
                }
            }
        });
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(get_raw_within("127.0.0.1", port, "/", "", None, Duration::from_secs(1)));
        });
        let r = rx.recv_timeout(Duration::from_secs(4)).expect("still reading a trickle after 4 s");
        assert!(r.is_err());
    }

    #[test]
    fn an_event_is_placed_within_a_second_of_when_it_happened() {
        // The controller's clock and its stamps are whole seconds; the true offset can
        // be any fraction. Over every phase, the placing is never more than a second
        // (and the clock read's half round trip) out, which the recordings' slack covers.
        let mut worst = 0i64;
        for true_offset_ms in [-14_400_900i64, -14_400_000, -14_400_001, -14_399_999, 3_600_500, 0, 999] {
            for phase in (0..1000).step_by(37) {
                let pc_ms = 1_790_000_000_000 + phase;
                let ctrl_s = (pc_ms + true_offset_ms).div_euclid(1000);
                let offset = clock_offset_ms(ctrl_s, pc_ms - 12, pc_ms + 12).unwrap();
                for event_phase in (0..1000).step_by(53) {
                    let happened = 1_790_000_100_000 + event_phase;
                    let stamp = (happened + true_offset_ms).div_euclid(1000);
                    worst = worst.max((event_utc_ms(stamp, offset) - happened).abs());
                }
            }
        }
        assert!(worst <= 1012, "placed up to {worst} ms out");
        // Where in its round trip the controller read its clock is unknown: up to half
        // the round trip more, and a slower read is not used.
        assert!(EVENT_SLACK_MS >= worst + MAX_CLOCK_RTT_MS / 2, "a recording's slack ({EVENT_SLACK_MS} ms) is less than the placing error ({worst} ms and half a round trip)");
        assert_eq!(clock_offset_ms(1_790_000_000, 1_790_000_000_000, 1_790_000_000_000 + MAX_CLOCK_RTT_MS + 1), None, "a slow clock read is not used");
        assert_eq!(clock_offset_ms(1_790_000_000, 1_790_000_000_000, 1_789_999_999_000), None, "nor one whose PC clock stepped back");
    }

    #[test]
    fn an_event_reads_from_the_log_document() {
        let doc: Value = serde_json::from_str(r#"{"_embedded": {"_state": [{"_type": "elog-message", "_title": "/rw/elog/0/125465507", "msgtype": "2", "code": "10010", "tstamp": "2026-09-26 T 08:08:53", "title": "Motors OFF state"}]}}"#).unwrap();
        let e = Event::from_json(&items(&doc)[0]).unwrap();
        assert_eq!((e.id, e.code, e.severity, e.title.as_str()), (125_465_507, 10010, 2, "Motors OFF state"));
        assert_eq!(e.severity_word(), "warning");
        assert_eq!(calib_instance("ROB_2", 3).as_deref(), Some("rob2_3"));
        assert_eq!(calib_instance("STN_1", 1), None);
        assert_eq!(calib_instance("ROB_", 1), None);
    }
}
