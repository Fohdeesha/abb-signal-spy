//! Finding the local virtual controllers, and a handshake-only probe.
//!
//! A RobotStudio virtual controller serves RobAPI InfoStream on a port it picks at
//! every start (the RW6 VC's was 17475 on 2026-08-28 and 45198 on 2026-09-24), not
//! on 5515. So the app asks Windows which TCP ports the VC processes listen on (the
//! IP Helper API; no administrator rights needed), and handshakes each one: the
//! ports that answer with the RobAPI magic are the ones to offer.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use crate::reply::Announce;
use crate::request;
use crate::wire::{self, service, Frame, FrameStatus};

/// VC host processes: RobotWare 6 and RobotWare 7.
pub const VC_PROCESSES: &[&str] = &["RobVC.exe", "Vrchost64.exe"];
/// Ports a VC also listens on that are known not to be RobAPI (RWS).
const NOT_ROBAPI: &[u16] = &[80, 443, 90, 91];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listener {
    pub pid: u32,
    pub process: String,
    pub port: u16,
}

#[derive(Debug, Clone)]
pub struct LocalController {
    pub process: String,
    pub pid: u32,
    pub port: u16,
    /// The handshake's answer, or why there was none.
    pub hello: Result<Announce, String>,
}

/// Every TCP port a VC process listens on, over IPv4.
#[cfg(windows)]
pub fn vc_listeners() -> Result<Vec<Listener>, String> {
    Ok(listening_ports()?.into_iter().filter(|l| VC_PROCESSES.iter().any(|p| p.eq_ignore_ascii_case(&l.process))).collect())
}

#[cfg(not(windows))]
pub fn vc_listeners() -> Result<Vec<Listener>, String> {
    Ok(Vec::new())
}

#[cfg(windows)]
fn listening_ports() -> Result<Vec<Listener>, String> {
    use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
    use windows_sys::Win32::NetworkManagement::IpHelper::{GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER};
    use windows_sys::Win32::Networking::WinSock::AF_INET;

    let mut size: u32 = 0;
    let mut buf: Vec<u64> = Vec::new();
    // The table can grow between the size query and the read; retry a few times.
    for _ in 0..5 {
        let rc = unsafe { GetExtendedTcpTable(buf.as_mut_ptr().cast(), &mut size, 0, u32::from(AF_INET), TCP_TABLE_OWNER_PID_LISTENER, 0) };
        if rc == NO_ERROR {
            break;
        }
        if rc != ERROR_INSUFFICIENT_BUFFER {
            return Err(format!("GetExtendedTcpTable failed ({rc})"));
        }
        // u64 elements keep the table 8-byte aligned.
        buf = vec![0u64; (size as usize).div_ceil(8) + 1];
    }
    if buf.is_empty() {
        return Ok(Vec::new());
    }
    let bytes = buf.len() * 8;
    let base = buf.as_ptr().cast::<u8>();
    // MIB_TCPTABLE_OWNER_PID: u32 dwNumEntries, then the rows.
    let n = unsafe { base.cast::<u32>().read() } as usize;
    let row_size = std::mem::size_of::<MIB_TCPROW_OWNER_PID>();
    let rows_at = std::mem::align_of::<MIB_TCPROW_OWNER_PID>().max(4);
    if rows_at + n.saturating_mul(row_size) > bytes {
        return Err("GetExtendedTcpTable returned a truncated table".into());
    }
    let mut out = Vec::new();
    for i in 0..n {
        let row = unsafe { base.add(rows_at + i * row_size).cast::<MIB_TCPROW_OWNER_PID>().read_unaligned() };
        // The port is in network byte order in the low 16 bits.
        let port = u16::from_be((row.dwLocalPort & 0xFFFF) as u16);
        out.push(Listener { pid: row.dwOwningPid, process: process_name(row.dwOwningPid).unwrap_or_default(), port });
    }
    Ok(out)
}

#[cfg(windows)]
fn process_name(pid: u32) -> Option<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
    if pid == 0 || pid == 4 {
        return None;
    }
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut name = vec![0u16; 1024];
        let mut len = name.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, name.as_mut_ptr(), &mut len);
        CloseHandle(h);
        if ok == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&name[..len as usize]);
        Some(path.rsplit(['\\', '/']).next().unwrap_or(&path).to_string())
    }
}

/// Handshake and nothing else: connect, send the control frame, read the answer,
/// close. Nothing is defined and no InfoStream session is opened, but for the
/// moment it is connected this program is one of the controller's RobAPI clients.
pub fn hello(addr: SocketAddr, timeout: Duration) -> Result<Announce, String> {
    let mut s = TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("cannot connect: {e}"))?;
    let _ = s.set_nodelay(true);
    let _ = s.set_write_timeout(Some(timeout));
    s.write_all(&request::hello(1)).map_err(|e| format!("cannot send the handshake: {e}"))?;
    let deadline = Instant::now() + timeout;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("no RobAPI answer (timed out)".into());
        }
        let _ = s.set_read_timeout(Some(left));
        let n = match s.read(&mut chunk) {
            Ok(0) => return Err("the port closed the connection without a RobAPI answer".into()),
            Ok(n) => n,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => return Err("no RobAPI answer (timed out)".into()),
            Err(e) => return Err(format!("read failed: {e}")),
        };
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > 64 * 1024 {
            return Err("too much data without a RobAPI answer".into());
        }
        loop {
            match wire::frame_at(&buf) {
                FrameStatus::NeedMore => break,
                FrameStatus::Desync(d) => return Err(format!("not a RobAPI port ({d})")),
                FrameStatus::Complete(len) => {
                    let f: Vec<u8> = buf.drain(..len).collect();
                    let frame = Frame::parse(&f).ok_or("malformed frame")?;
                    if frame.service() == service::CONTROL && frame.txn() == 1 {
                        let a = Announce::from_rads(frame.rads(), frame.ctrl1());
                        let _ = s.shutdown(std::net::Shutdown::Both);
                        return Ok(a);
                    }
                    // Anything else (an AYA) is not the answer; keep reading.
                }
            }
        }
    }
}

/// Every local VC port that answers the RobAPI handshake, and the ones that did
/// not (with why), handshaken in parallel.
pub fn local_controllers(timeout: Duration) -> Result<Vec<LocalController>, String> {
    let listeners = vc_listeners()?;
    let handles: Vec<_> = listeners
        .into_iter()
        .filter(|l| !NOT_ROBAPI.contains(&l.port))
        .map(|l| {
            std::thread::spawn(move || {
                let addr = SocketAddr::from(([127, 0, 0, 1], l.port));
                LocalController { process: l.process, pid: l.pid, port: l.port, hello: hello(addr, timeout) }
            })
        })
        .collect();
    let mut out: Vec<LocalController> = handles.into_iter().filter_map(|h| h.join().ok()).collect();
    out.sort_by_key(|c| (c.hello.is_err(), c.port));
    Ok(out)
}

#[cfg(all(test, feature = "fake"))]
mod tests {
    use super::*;
    use crate::fake::{Behaviour, FakeController};

    #[test]
    fn hello_reads_the_padded_client_list() {
        let fake = FakeController::start(Behaviour::default()).unwrap();
        let a = hello(fake.addr(), Duration::from_secs(2)).unwrap();
        assert_eq!(a.system_id.as_deref(), Some(crate::fake::SYSTEM_ID));
        assert_eq!(a.clients.len(), 1, "{a:?}");
        assert_eq!(a.clients[0].address, "127.0.0.1");
        assert_eq!(a.ctrl1, 257);
        assert!(fake.seen().is_empty(), "a hello must not send any command");
    }

    #[test]
    fn hello_names_a_port_that_is_not_robapi() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            if let Ok((mut c, _)) = l.accept() {
                let _ = c.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
                std::thread::sleep(Duration::from_millis(300));
            }
        });
        let e = hello(addr, Duration::from_secs(2)).unwrap_err();
        assert!(e.contains("not a RobAPI port"), "{e}");
        t.join().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn the_listener_table_is_readable() {
        // This process's own listener must show up with its port.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let all = listening_ports().unwrap();
        let me = all.iter().find(|x| x.port == port).expect("own listener not found");
        assert_eq!(me.pid, std::process::id());
        assert!(me.process.to_ascii_lowercase().ends_with(".exe"), "{me:?}");
    }
}
