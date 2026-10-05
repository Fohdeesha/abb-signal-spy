use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const MAX_REQUEST: usize = 8 * 1024;
const MAX_CONNECTIONS: usize = 16;
const IO_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_DEADLINE: Duration = Duration::from_secs(5);
const PORT_TRIES: u16 = 10;
const FROZEN_MS: u64 = 1000;

fn frozen(body: &str) -> String {
    let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(body) else { return body.to_string() };
    if let Some(chans) = doc["channels"].as_array_mut() {
        for c in chans {
            if c["on_target"] == true {
                c["value"] = c["number"].clone();
                c["units"] = c["number_units"].clone();
                c["on_target"] = false.into();
            }
            c["stale"] = true.into();
            c["status"] = "NOT CURRENT".into();
        }
    }
    doc.to_string()
}

#[derive(Default)]
pub struct Snapshot {
    pub body: String,
    pub built: Option<Instant>,
}

pub struct PhoneServer {
    pub addr: SocketAddr,
    pub urls: Vec<String>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl PhoneServer {
    pub fn start(port: u16, snapshot: Arc<Mutex<Snapshot>>) -> Result<PhoneServer, String> {
        let tries = if port == 0 { 1 } else { PORT_TRIES };
        let mut last = String::new();
        for p in (0..tries).filter_map(|i| port.checked_add(i)) {
            match TcpListener::bind(("0.0.0.0", p)) {
                Ok(l) => return PhoneServer::serve_on(l, snapshot, REQUEST_DEADLINE),
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => last = format!("ports {port} to {p} are all in use by other programs"),
                Err(e) => return Err(format!("cannot listen on port {p}: {e}")),
            }
        }
        Err(last)
    }

    fn serve_on(listener: TcpListener, snapshot: Arc<Mutex<Snapshot>>, deadline: Duration) -> Result<PhoneServer, String> {
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let names = own_names();
        let thread = std::thread::Builder::new()
            .name("phone-view".into())
            .spawn(move || accept_loop(listener, snapshot, stop2, names, deadline))
            .map_err(|e| format!("cannot start the phone view: {e}"))?;
        let urls = local_addresses().iter().map(|a| format!("http://{a}:{}/", addr.port())).collect();
        Ok(PhoneServer { addr, urls, stop, thread: Some(thread) })
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }
}

impl Drop for PhoneServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn accept_loop(listener: TcpListener, snapshot: Arc<Mutex<Snapshot>>, stop: Arc<AtomicBool>, names: Arc<Vec<String>>, deadline: Duration) {
    let active = Arc::new(AtomicUsize::new(0));
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                if active.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                    drop(stream);
                    continue;
                }
                active.fetch_add(1, Ordering::SeqCst);
                let (snap, act, names) = (snapshot.clone(), active.clone(), names.clone());
                let spawned = std::thread::Builder::new().name("phone-conn".into()).spawn(move || {
                    let _ = serve(stream, &snap, &names, deadline);
                    act.fetch_sub(1, Ordering::SeqCst);
                });
                if spawned.is_err() {
                    active.fetch_sub(1, Ordering::SeqCst);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn respond(s: &mut TcpStream, status: &str, ctype: &str, body: &[u8], head_only: bool) -> std::io::Result<()> {
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'\r\n\r\n",
        body.len()
    );
    s.write_all(header.as_bytes())?;
    if !head_only {
        s.write_all(body)?;
    }
    s.flush()
}

fn own_names() -> Arc<Vec<String>> {
    let mut v = vec!["localhost".to_string()];
    if let Ok(n) = std::env::var("COMPUTERNAME") {
        v.push(n.to_ascii_lowercase());
    }
    Arc::new(v)
}

fn host_ok(host: Option<&str>, names: &[String]) -> bool {
    let Some(h) = host else { return true };
    let h = h.trim();
    let name = if let Some(rest) = h.strip_prefix('[') {
        match rest.split_once(']') {
            Some((ip, _)) => return ip.parse::<std::net::Ipv6Addr>().is_ok(),
            None => return false,
        }
    } else {
        h.rsplit_once(':').map_or(h, |(n, port)| if port.bytes().all(|b| b.is_ascii_digit()) { n } else { h })
    };
    let name = name.to_ascii_lowercase();
    name.parse::<std::net::Ipv4Addr>().is_ok() || names.iter().any(|n| name == *n || name.strip_prefix(n.as_str()).is_some_and(|rest| rest.starts_with('.')))
}

fn serve(mut s: TcpStream, snapshot: &Mutex<Snapshot>, names: &[String], deadline: Duration) -> std::io::Result<()> {
    s.set_nonblocking(false)?;
    s.set_write_timeout(Some(IO_TIMEOUT))?;
    let end = Instant::now() + deadline;
    let mut req = Vec::with_capacity(1024);
    let mut buf = [0u8; 1024];
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return respond(&mut s, "408 Request Timeout", "text/plain", b"too slow\n", false);
        }
        s.set_read_timeout(Some(left.min(IO_TIMEOUT)))?;
        let n = match s.read(&mut buf) {
            Ok(n) => n,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
            Err(e) => return Err(e),
        };
        if n == 0 {
            return Ok(());
        }
        req.extend_from_slice(&buf[..n]);
        if req.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if req.len() > MAX_REQUEST {
            return respond(&mut s, "431 Request Header Fields Too Large", "text/plain", b"request too large\n", false);
        }
    }
    let line_end = req.iter().position(|&b| b == b'\r').unwrap_or(req.len());
    let line = String::from_utf8_lossy(&req[..line_end]).to_string();
    let mut parts = line.split(' ');
    let (method, path) = match (parts.next(), parts.next(), parts.next()) {
        (Some(m), Some(p), Some(v)) if v.starts_with("HTTP/1.") => (m, p),
        _ => return respond(&mut s, "400 Bad Request", "text/plain", b"bad request\n", false),
    };
    let head = method == "HEAD";
    if method != "GET" && !head {
        return respond(&mut s, "405 Method Not Allowed", "text/plain", b"read-only\n", false);
    }
    let text = String::from_utf8_lossy(&req);
    let host = text.lines().skip(1).take_while(|l| !l.is_empty()).find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case("host")).map(|(_, v)| v.trim().to_string()));
    if !host_ok(host.as_deref(), names) {
        return respond(&mut s, "421 Misdirected Request", "text/plain", b"not this PC's name\n", false);
    }
    let path = path.split('?').next().unwrap_or("");
    match path {
        "/" | "/index.html" => respond(&mut s, "200 OK", "text/html; charset=utf-8", PAGE.as_bytes(), head),
        "/data" => {
            let body = {
                let g = snapshot.lock().unwrap_or_else(|e| e.into_inner());
                let age = g.built.map(|b| b.elapsed().as_millis() as u64);
                match age {
                    Some(age) if !g.body.is_empty() => format!("{{\"age_ms\":{age},\"data\":{}}}", if age > FROZEN_MS { frozen(&g.body) } else { g.body.clone() }),
                    _ => "{\"age_ms\":null,\"data\":null}".to_string(),
                }
            };
            respond(&mut s, "200 OK", "application/json", body.as_bytes(), head)
        }
        _ => respond(&mut s, "404 Not Found", "text/plain", b"not found\n", head),
    }
}

pub fn local_addresses() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Ok(name) = std::env::var("COMPUTERNAME")
        && let Ok(addrs) = (name.as_str(), 0).to_socket_addrs() {
            for a in addrs {
                if let std::net::IpAddr::V4(v4) = a.ip()
                    && !v4.is_loopback() && !v4.is_link_local() && !out.contains(&v4.to_string()) {
                        out.push(v4.to_string());
                    }
            }
        }
    if let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0")
        && sock.connect("192.0.2.1:9").is_ok()
            && let Ok(a) = sock.local_addr() {
                let ip = a.ip().to_string();
                if !a.ip().is_unspecified() && !out.contains(&ip) {
                    out.push(ip);
                }
            }
    out
}

const PAGE: &str = r#"<!doctype html><html lang=en><head><meta charset=utf-8>
<meta name=viewport content="width=device-width,initial-scale=1">
<title>ABB Signal Spy</title><style>
*{box-sizing:border-box}
body{margin:0;background:#0d0f12;color:#e8eaed;font:16px/1.3 -apple-system,Segoe UI,Roboto,sans-serif;padding:14px;-webkit-text-size-adjust:100%}
h1{font-size:13px;letter-spacing:.12em;text-transform:uppercase;color:#8b93a1;margin:0 0 4px;font-weight:600}
.sub{color:#8b93a1;font-size:12px;margin-bottom:12px}
.card{background:#161a20;border:1px solid #242a33;border-radius:12px;padding:12px 14px;margin-bottom:10px}
.name{font-size:13px;color:#aeb6c2}
.val{font:700 clamp(30px,10vw,56px)/1.1 ui-monospace,Consolas,monospace;font-variant-numeric:tabular-nums}
.unit{font-size:15px;color:#8b93a1;margin-left:6px}
.stale .val{opacity:.35}
.badge{display:inline-block;font-size:11px;padding:1px 6px;border-radius:6px;margin-left:6px;background:#5a4410;color:#f0c050}
#banner{display:none;background:#2a1414;border:1px solid #7d2f2f;color:#ff9f9f;border-radius:12px;padding:10px 14px;margin-bottom:10px;font-size:14px}
footer{color:#5f6672;font-size:12px;margin-top:12px}
</style></head><body>
<h1>ABB Signal Spy</h1>
<div class=sub id=sub>connecting...</div>
<div id=banner></div>
<div id=cards></div>
<footer>Read-only view. Nothing here can change the controller.</footer>
<script>
var lastOk = 0;
function esc(s){return String(s).replace(/[&<>"]/g,function(c){return {'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c];});}
function show(msg){var b=document.getElementById('banner');b.style.display=msg?'':'none';b.textContent=msg||'';
  document.getElementById('cards').style.opacity=msg?'.45':'1';}
async function tick(){
  try{
    var r=await fetch('data',{cache:'no-store'}); var s=await r.json();
    if(!s.data){show('Waiting for the program to publish data.');return;}
    var d=s.data;
    document.getElementById('sub').textContent=(d.controller||'')+'  '+(d.state||'');
    var h='';
    (d.channels||[]).forEach(function(c){
      h+='<div class="card'+(c.stale?' stale':'')+'"><div class=name>'+esc(c.name)+(c.stale?'<span class=badge>'+esc(c.status||'STALE')+'</span>':'')+'</div>'+
         '<div><span class=val>'+esc(c.value)+'</span><span class=unit>'+esc(c.units)+'</span></div></div>';
    });
    if(!h)h='<div class=card><div class=name>No channels.</div></div>';
    document.getElementById('cards').innerHTML=h;
    if(s.age_ms>3000){show('The values below are frozen: the program has not updated them for '+Math.round(s.age_ms/1000)+' s.');}
    else if(d.state!=='Streaming'){show('Not streaming ('+d.state+'): the values below are the last ones received.');}
    else{show('');}
    lastOk=Date.now();
  }catch(e){show('Lost contact with the PC running ABB Signal Spy. The values below are frozen.');}
}
tick(); setInterval(tick,500);
</script></body></html>"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn get(port: u16, req: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        s.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    }

    #[test]
    fn serves_the_page_and_data_and_nothing_else() {
        let snap = Arc::new(Mutex::new(Snapshot::default()));
        let srv = PhoneServer::start(0, snap.clone()).unwrap();
        let p = srv.port();
        let page = get(p, &format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\n\r\n"));
        assert!(page.starts_with("HTTP/1.1 200 OK") && page.contains("Read-only view"), "{page}");
        let data = get(p, "GET /data HTTP/1.1\r\n\r\n");
        assert!(data.contains("\"data\":null"), "no snapshot yet: {data}");
        {
            let mut g = snap.lock().unwrap();
            g.body = r#"{"state":"Streaming","channels":[]}"#.into();
            g.built = Some(Instant::now());
        }
        let data = get(p, "GET /data?t=1 HTTP/1.1\r\n\r\n");
        assert!(data.contains("\"age_ms\":") && data.contains("Streaming"), "{data}");
        assert!(get(p, "POST /data HTTP/1.1\r\nContent-Length: 0\r\n\r\n").starts_with("HTTP/1.1 405"));
        assert!(get(p, "GET /../../etc/passwd HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 404"));
        assert!(get(p, "garbage\r\n\r\n").starts_with("HTTP/1.1 400"));
        let big = format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "a".repeat(20_000));
        assert!(get(p, &big).starts_with("HTTP/1.1 431"));
        let head = get(p, "HEAD / HTTP/1.1\r\n\r\n");
        assert!(head.starts_with("HTTP/1.1 200") && !head.contains("<html"));
        drop(srv);
        assert!(TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], p)), Duration::from_millis(500)).is_err(), "switched off means not listening");
    }

    #[test]
    fn a_taken_port_moves_to_the_next_free_one() {
        let start = 20_000 + (std::process::id() % 10_000) as u16;
        let (taken, p) = (start..start + 2_000)
            .step_by(3)
            .find_map(|p| {
                let a = TcpListener::bind(("0.0.0.0", p)).ok()?;
                drop(TcpListener::bind(("0.0.0.0", p + 1)).ok()?);
                Some((a, p))
            })
            .expect("no free pair of ports");
        let srv = PhoneServer::start(p, Arc::new(Mutex::new(Snapshot::default()))).unwrap();
        assert_eq!(srv.port(), p + 1);
        assert!(get(srv.port(), "GET / HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 200"));
        drop(taken);
    }

    #[test]
    fn only_this_pcs_names_are_answered() {
        let names = vec!["localhost".to_string(), "mypc".to_string()];
        for ok in [None, Some("127.0.0.1:8090"), Some("192.168.1.5"), Some("[fe80::1]:8090"), Some("localhost:8090"), Some("MYPC:8090"), Some("mypc.lan"), Some("mypc.local:8090")] {
            assert!(host_ok(ok, &names), "{ok:?}");
        }
        for bad in [Some("evil.example"), Some("evil.example:8090"), Some("notmypc"), Some("mypcx:8090"), Some("[not-an-ip]:8090"), Some("")] {
            assert!(!host_ok(bad, &names), "{bad:?}");
        }
        let srv = PhoneServer::start(0, Arc::new(Mutex::new(Snapshot::default()))).unwrap();
        assert!(get(srv.port(), "GET /data HTTP/1.1\r\nHost: evil.example\r\n\r\n").starts_with("HTTP/1.1 421"));
        assert!(get(srv.port(), &format!("GET /data HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n", srv.port())).starts_with("HTTP/1.1 200"));
    }

    #[test]
    fn a_snapshot_the_window_stopped_updating_shows_nothing_as_current() {
        let snap = Arc::new(Mutex::new(Snapshot::default()));
        let srv = PhoneServer::start(0, snap.clone()).unwrap();
        let body = r#"{"state":"Streaming","channels":[{"name":"Turn to target  5138 ROB_1 J1","value":"ON TARGET","units":"","stale":false,"status":"LIVE","on_target":true,"number":"+0.100","number_units":"deg"},{"name":"Torque","value":"3.50000","units":"Nm","stale":false,"status":"LIVE"}]}"#;
        let fetch = |age_ms: u64| {
            {
                let mut g = snap.lock().unwrap();
                g.body = body.into();
                g.built = Instant::now().checked_sub(Duration::from_millis(age_ms));
            }
            let data = get(srv.port(), &format!("GET /data HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n", srv.port()));
            let json = data.split("\r\n\r\n").nth(1).unwrap().to_string();
            serde_json::from_str::<serde_json::Value>(&json).unwrap()
        };
        let fresh = fetch(0);
        assert_eq!(fresh["data"]["channels"][0]["value"], "ON TARGET", "a fresh snapshot as the window built it");
        let old = fetch(1500);
        let ch = &old["data"]["channels"];
        assert_eq!(ch[0]["value"], "+0.100", "ON TARGET shown from a frozen snapshot");
        assert_eq!(ch[0]["units"], "deg");
        for c in ch.as_array().unwrap() {
            assert_eq!((c["stale"].as_bool(), c["status"].as_str()), (Some(true), Some("NOT CURRENT")), "{c}");
        }
    }

    #[test]
    fn a_client_trickling_its_request_is_cut_off() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let srv = PhoneServer::serve_on(listener, Arc::new(Mutex::new(Snapshot::default())), Duration::from_millis(800)).unwrap();
        let mut s = TcpStream::connect(("127.0.0.1", srv.port())).unwrap();
        s.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
        let t0 = Instant::now();
        let mut ended = None;
        for b in b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n".iter().cycle() {
            if s.write_all(&[*b]).is_err() || probe_closed(&mut s) {
                ended = Some(t0.elapsed());
                break;
            }
            std::thread::sleep(Duration::from_millis(300));
            if t0.elapsed() > Duration::from_secs(5) {
                break;
            }
        }
        let ended = ended.expect("the server never cut the trickling client off");
        assert!(ended < Duration::from_millis(2500), "held for {ended:?}; the deadline is 0.8 s");
    }

    #[allow(clippy::unused_io_amount)]
    fn probe_closed(s: &mut TcpStream) -> bool {
        let mut b = [0u8; 64];
        match s.read(&mut b) {
            Ok(_) => true,
            Err(e) => !matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut),
        }
    }

    #[test]
    fn a_silent_client_does_not_block_others() {
        let snap = Arc::new(Mutex::new(Snapshot::default()));
        let srv = PhoneServer::start(0, snap).unwrap();
        let _idle: Vec<TcpStream> = (0..4).map(|_| TcpStream::connect(("127.0.0.1", srv.port())).unwrap()).collect();
        let t0 = Instant::now();
        assert!(get(srv.port(), "GET / HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 200"));
        assert!(t0.elapsed() < Duration::from_secs(2));
    }
}
