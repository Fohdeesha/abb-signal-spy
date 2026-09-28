//! An in-process stand-in for an IRC5's RWS 1.0, the resources the RWS extras read,
//! answered as the RW6 VC answered them (tunemaster-testsignals.md s25 item 5): a
//! Digest login then a session cookie, JSON documents, the event log newest first by
//! `limit` and `start`. It records every request, so a test can check that nothing but
//! GETs was ever sent. Loopback only.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::rws::{digest_params, md5};

#[derive(Debug, Clone)]
pub struct FakeEvent {
    pub id: u64,
    pub code: u32,
    pub severity: u8,
    /// The controller's clock, seconds since 1970 read as if UTC.
    pub time: i64,
    pub title: String,
}

#[derive(Debug, Clone)]
pub struct RwsBehaviour {
    pub user: String,
    pub password: String,
    pub name: String,
    pub rw_version: String,
    pub system_id: String,
    pub ctrl_name: String,
    pub ctrl_type: String,
    /// The controller's clock minus this PC's UTC, seconds.
    pub clock_offset_s: i64,
    /// Oldest first.
    pub events: Vec<FakeEvent>,
    /// `MOTOR_CALIB` instances: name, com_offset, valid, cal_offset, valid.
    pub calib: Vec<(String, f64, bool, f64, bool)>,
    /// Log an event right after serving the event log's first page: one logged
    /// between two page reads of a client.
    pub log_between_pages: bool,
}

impl Default for RwsBehaviour {
    fn default() -> RwsBehaviour {
        RwsBehaviour {
            user: crate::rws::DEFAULT_USER.into(),
            password: "robotics".into(),
            name: "IRB2600".into(),
            rw_version: "6.16.2027".into(),
            system_id: "{00000000-FA4E-4C0E-8000-000000000001}".into(),
            ctrl_name: "FAKE".into(),
            ctrl_type: "Virtual Controller".into(),
            clock_offset_s: -4 * 3600,
            events: Vec::new(),
            calib: (1..=6).map(|a| (format!("rob1_{a}"), 1.5707999, true, 0.1 * f64::from(a), true)).collect(),
            log_between_pages: false,
        }
    }
}

#[derive(Debug, Default)]
struct State {
    b: RwsBehaviour,
    sessions: HashSet<String>,
    nonces: HashSet<String>,
    /// (method, path) of every request.
    requests: Vec<(String, String)>,
    logins: u32,
    /// The nonce count (`nc`) of every Digest answer.
    nonce_counts: Vec<String>,
}

pub struct FakeRws {
    port: u16,
    state: Arc<Mutex<State>>,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

fn lock(s: &Mutex<State>) -> MutexGuard<'_, State> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

fn pc_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn stamp(t: i64) -> String {
    let (y, m, d) = crate::util::civil_from_days(t.div_euclid(86_400));
    let s = t.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02} T {:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

impl FakeRws {
    pub fn start(b: RwsBehaviour) -> std::io::Result<FakeRws> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let state = Arc::new(Mutex::new(State { b, ..State::default() }));
        let running = Arc::new(AtomicBool::new(true));
        let (st, run) = (state.clone(), running.clone());
        let thread = std::thread::spawn(move || {
            while run.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((s, _)) => serve(s, &st),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(5)),
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        });
        Ok(FakeRws { port, state, running, thread: Some(thread) })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut RwsBehaviour) -> R) -> R {
        f(&mut lock(&self.state).b)
    }

    /// A new event, stamped now on the controller's clock; its id.
    pub fn push_event(&self, code: u32, severity: u8, title: &str) -> u64 {
        let mut s = lock(&self.state);
        let id = s.b.events.last().map_or(1000, |e| e.id + 1);
        let time = pc_now() + s.b.clock_offset_s;
        s.b.events.push(FakeEvent { id, code, severity, time, title: title.into() });
        id
    }

    /// Every request so far, (method, path).
    pub fn requests(&self) -> Vec<(String, String)> {
        lock(&self.state).requests.clone()
    }

    pub fn logins(&self) -> u32 {
        lock(&self.state).logins
    }

    /// Forget every session, as a controller does when one times out.
    pub fn expire_sessions(&self) {
        lock(&self.state).sessions.clear();
    }

    /// Another controller behind the same address (a cable moved between two IRC5s'
    /// service ports, both 192.168.125.1): its own system id and event log, no session.
    pub fn replace_controller(&self, system_id: &str) {
        let mut s = lock(&self.state);
        s.b.system_id = system_id.into();
        s.b.events.clear();
        s.sessions.clear();
    }

    /// The nonce count of every Digest answer so far.
    pub fn nonce_counts(&self) -> Vec<String> {
        lock(&self.state).nonce_counts.clone()
    }

    pub fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for FakeRws {
    fn drop(&mut self) {
        self.stop();
    }
}

fn respond(s: &mut TcpStream, status: &str, extra: &str, body: &str) {
    let _ = s.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}", body.len()).as_bytes());
}

fn serve(mut s: TcpStream, st: &Mutex<State>) {
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = s.set_nonblocking(false);
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
        match s.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => raw.extend_from_slice(&buf[..n]),
        }
        if raw.len() > 64 * 1024 {
            return;
        }
    }
    let text = String::from_utf8_lossy(&raw).to_string();
    let mut lines = text.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let (method, path) = (first.next().unwrap_or("").to_string(), first.next().unwrap_or("").to_string());
    let header = |name: &str| text.split("\r\n").skip(1).filter_map(|l| l.split_once(':')).find(|(n, _)| n.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_string());
    let mut g = lock(st);
    g.requests.push((method.clone(), path.clone()));
    if method != "GET" {
        return respond(&mut s, "405 Method Not Allowed", "", "{}");
    }
    let session = header("cookie").and_then(|c| c.split(';').find_map(|p| p.trim().strip_prefix("-http-session-=").map(str::to_string)));
    let mut set_cookie = String::new();
    if let Some(nc) = header("authorization").and_then(|a| digest_params(&a)).and_then(|p| p.into_iter().find(|(n, _)| n == "nc").map(|(_, v)| v)) {
        g.nonce_counts.push(nc);
    }
    if !session.is_some_and(|v| g.sessions.contains(&v)) {
        // Not in a session: a Digest answer to a nonce it gave, or a challenge.
        let answered = header("authorization").and_then(|a| digest_params(&a)).is_some_and(|p| {
            let get = |k: &str| p.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default();
            let hex = |b: [u8; 16]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
            let ha1 = hex(md5(format!("{}:{}:{}", g.b.user, get("realm"), g.b.password).as_bytes()));
            let ha2 = hex(md5(format!("GET:{}", get("uri")).as_bytes()));
            let want = hex(md5(format!("{ha1}:{}:{}:{}:{}:{ha2}", get("nonce"), get("nc"), get("cnonce"), get("qop")).as_bytes()));
            get("username") == g.b.user && get("realm") == "validusers@robapi.abb" && g.nonces.contains(&get("nonce")) && get("uri") == path && get("response") == want
        });
        if !answered {
            let nonce = format!("{:x}", pc_now() as u64 ^ (g.requests.len() as u64) << 20);
            g.nonces.insert(nonce.clone());
            return respond(&mut s, "401 Unauthorized", &format!("WWW-Authenticate: Digest realm=\"validusers@robapi.abb\", qop=\"auth\", nonce=\"{nonce}\", algorithm=MD5\r\n"), "");
        }
        g.logins += 1;
        let id = format!("{}::http.session::{:x}", g.logins, pc_now() as u64 ^ 0x5a5a);
        g.sessions.insert(id.clone());
        set_cookie = format!("Set-Cookie: -http-session-={id}; path=/; httponly\r\nSet-Cookie: ABBCX={}; path=/; httponly\r\n", g.requests.len());
    }
    let (p, query) = path.split_once('?').unwrap_or((path.as_str(), ""));
    let q = |k: &str| query.split('&').find_map(|kv| kv.strip_prefix(&format!("{k}="))).and_then(|v| v.parse::<usize>().ok());
    let doc = |items: String| format!("{{\"_links\":{{\"base\": {{ \"href\": \"http://127.0.0.1/\" }}}},\"_embedded\" :{{ \"_state\":[ {items} ] }}}}");
    let b = g.b.clone();
    drop(g);
    let body = match p {
        "/rw/system" => doc(format!("{{\"_type\":\"sys-system-li\",\"_title\":\"system\",\"name\":\"{}\",\"rwversion\":\"{}\",\"sysid\":\"{}\"}}", b.name, b.rw_version, b.system_id)),
        "/ctrl/identity" => doc(format!("{{\"_type\":\"ctrl-identity-info\",\"_title\":\"identity\",\"ctrl-name\":\"{}\",\"ctrl-type\":\"{}\"}}", b.ctrl_name, b.ctrl_type)),
        "/ctrl/clock" => doc(format!("{{\"_type\":\"ctrl-clock-info\",\"_title\":\"clock\",\"datetime\":\"{}\"}}", stamp(pc_now() + b.clock_offset_s))),
        "/rw/elog/0" => {
            let (limit, page) = (q("limit").unwrap_or(50).clamp(1, 50), q("start").unwrap_or(1).max(1));
            let items: Vec<String> = b
                .events
                .iter()
                .rev()
                .skip((page - 1) * limit)
                .take(limit)
                .map(|e| format!("{{\"_type\":\"elog-message\",\"_title\":\"/rw/elog/0/{}\",\"msgtype\":\"{}\",\"code\":\"{}\",\"tstamp\":\"{}\",\"title\":{}}}", e.id, e.severity, e.code, stamp(e.time), serde_json::Value::String(e.title.clone())))
                .collect();
            if page == 1 && b.log_between_pages {
                let mut g = lock(st);
                let id = g.b.events.last().map_or(1000, |e| e.id + 1);
                let time = pc_now() + g.b.clock_offset_s;
                g.b.events.push(FakeEvent { id, code: 99999, severity: 1, time, title: "between pages".into() });
            }
            doc(items.join(","))
        }
        _ => match p.strip_prefix("/rw/cfg/MOC/MOTOR_CALIB/instances/").and_then(|i| b.calib.iter().find(|c| c.0 == i)) {
            Some((name, com, com_valid, cal, cal_valid)) => {
                let a = |k: &str, v: String| format!("{{\"_type\":\"cfg-ia-t-li\",\"_title\":\"{k}\",\"value\":\"{v}\"}}");
                doc(format!(
                    "{{\"_type\":\"cfg-dt-instance-li\",\"_title\":\"{name}\",\"attrib\":[{},{},{},{},{}]}}",
                    a("name", name.clone()),
                    a("com_offset", com.to_string()),
                    a("valid_com_offset", com_valid.to_string()),
                    a("cal_offset", cal.to_string()),
                    a("valid_cal_offset", cal_valid.to_string())
                ))
            }
            None => return respond(&mut s, "400 Bad Request", &set_cookie, "{\"_embedded\":{\"status\":{\"code\":-1,\"msg\":\"no such resource\"},\"_state\":[]}}"),
        },
    };
    respond(&mut s, "200 OK", &set_cookie, &body);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rws::{Client, EventPoll, RwsError};

    #[test]
    fn the_client_logs_in_once_and_reads_everything_with_gets_only() {
        let f = FakeRws::start(RwsBehaviour::default()).unwrap();
        let mut c = Client::new("127.0.0.1", f.port(), crate::rws::DEFAULT_USER, "robotics");
        let s = c.system().unwrap();
        assert_eq!((s.name.as_str(), s.rw_version.as_str()), ("IRB2600", "6.16.2027"));
        assert_eq!(c.identity().unwrap().kind, "Virtual Controller");
        let offset = c.clock().unwrap() - pc_now();
        assert!((offset + 4 * 3600).abs() <= 1, "{offset}");
        let m = c.motor_calib("rob1_2").unwrap();
        assert_eq!((m.com_offset, m.com_valid), (1.5707999, true));
        assert!(matches!(c.motor_calib("rob9_9"), Err(RwsError::Status(400, _))));
        assert!(c.motor_calib("../x").is_err(), "not a name: never sent");
        assert_eq!(f.logins(), 1, "one session for all of it");
        assert!(f.requests().iter().all(|(m, _)| m == "GET"));
        assert!(!f.requests().iter().any(|(_, p)| p.contains("..")));

        // A session the controller forgot: logged in again, once.
        f.expire_sessions();
        c.system().unwrap();
        assert_eq!(f.logins(), 2);
        // A wrong password: refused, and said so.
        let mut bad = Client::new("127.0.0.1", f.port(), crate::rws::DEFAULT_USER, "nope");
        assert_eq!(bad.system(), Err(RwsError::Login));
    }

    #[test]
    fn every_login_counts_its_nonce_from_one() {
        // RFC 2617 counts requests per nonce, and each login answers a fresh one. (The RW6
        // VC accepted any count, tunemaster-testsignals.md s25 item 6; a stricter server
        // would refuse a second login's 2 and blame the password.)
        let f = FakeRws::start(RwsBehaviour::default()).unwrap();
        let mut c = Client::new("127.0.0.1", f.port(), crate::rws::DEFAULT_USER, "robotics");
        c.system().unwrap();
        f.expire_sessions();
        c.system().unwrap();
        f.expire_sessions();
        c.clock().unwrap();
        assert_eq!(f.logins(), 3);
        assert_eq!(f.nonce_counts(), ["00000001", "00000001", "00000001"]);
    }

    #[test]
    fn another_controller_behind_the_address_gets_nothing_read() {
        let f = FakeRws::start(RwsBehaviour::default()).unwrap();
        let id = f.with(|b| b.system_id.clone());
        let mut c = Client::new("127.0.0.1", f.port(), crate::rws::DEFAULT_USER, "robotics").expect_system(&id);
        c.system().unwrap();
        let mut poll = EventPoll::default();
        f.push_event(10000, 1, "this controller's");
        assert_eq!(poll.look(&mut c).unwrap().events.len(), 1);
        // The session forgotten by the same controller: logged in again, carrying on.
        f.expire_sessions();
        f.push_event(10001, 1, "this controller's, later");
        assert_eq!(poll.look(&mut c).unwrap().events.iter().map(|e| e.code).collect::<Vec<_>>(), [10001]);
        assert_eq!(f.logins(), 2);

        // Another controller at the same address, with the same default login.
        f.replace_controller("{22222222-2222-4222-8222-222222222222}");
        f.push_event(20205, 3, "the other controller's");
        let other = |r: Result<(), RwsError>| matches!(r, Err(RwsError::OtherController { found, .. }) if found.starts_with("{2222"));
        assert!(other(poll.look(&mut c).map(|_| ())), "its event log read");
        // Nor anything after, though the new session would carry every request.
        assert!(other(c.clock().map(|_| ())));
        assert!(other(c.events(10, 1).map(|_| ())));
        assert!(other(c.motor_calib("rob1_1").map(|_| ())));
    }

    #[test]
    fn the_event_log_is_read_back_to_the_last_event_seen() {
        let f = FakeRws::start(RwsBehaviour::default()).unwrap();
        for i in 0..25 {
            f.push_event(10000 + i, 1, &format!("old {i}"));
        }
        let mut c = Client::new("127.0.0.1", f.port(), crate::rws::DEFAULT_USER, "robotics");
        let mut poll = EventPoll::default();
        let first = poll.look(&mut c).unwrap();
        assert_eq!(first.events.len(), crate::rws::POLL_LIMIT as usize, "the first look: the newest page");
        assert_eq!(first.events.last().unwrap().title, "old 24", "oldest first");
        assert!(poll.look(&mut c).unwrap().events.is_empty(), "nothing new");
        // 13 new: across two pages, oldest first, each once.
        for i in 0..13 {
            f.push_event(20000 + i, if i == 5 { 3 } else { 1 }, &format!("new {i}"));
        }
        let got = poll.look(&mut c).unwrap();
        assert_eq!(got.events.iter().map(|e| e.code).collect::<Vec<_>>(), (20000..20013).collect::<Vec<_>>());
        assert!(!got.skipped);
        assert_eq!(got.events[5].severity, 3);
        // More than a look reads: said.
        for i in 0..(crate::rws::POLL_LIMIT * crate::rws::POLL_PAGES + 3) {
            f.push_event(30000 + i, 2, "burst");
        }
        let burst = poll.look(&mut c).unwrap();
        assert!(burst.skipped && burst.events.len() == (crate::rws::POLL_LIMIT * crate::rws::POLL_PAGES) as usize);
        assert!(poll.look(&mut c).unwrap().events.is_empty(), "and none twice");

        // An event logged between two page reads: nothing read twice, nothing lost.
        for i in 0..13 {
            f.push_event(40000 + i, 1, "paged");
        }
        f.with(|b| b.log_between_pages = true);
        let got = poll.look(&mut c).unwrap();
        f.with(|b| b.log_between_pages = false);
        let codes: Vec<u32> = got.events.iter().map(|e| e.code).collect();
        assert_eq!(codes, (40000..40013).collect::<Vec<_>>(), "each once, in order");
        let next = poll.look(&mut c).unwrap();
        assert_eq!(next.events.iter().map(|e| e.code).collect::<Vec<_>>(), vec![99999], "the one logged meanwhile, next look");
    }
}
