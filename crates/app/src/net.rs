//! Small OS helpers: reverse DNS for the other-clients question (it names a
//! hostname when one is found), and a check for a second copy of this program.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

/// Hostnames looked up so far: `None` while pending or when there is none.
pub type Hostnames = Arc<Mutex<HashMap<String, Option<String>>>>;

/// Look an address up in the background, once. The answer lands in `cache`.
pub fn lookup(cache: &Hostnames, address: &str, repaint: impl Fn() + Send + 'static) {
    {
        let mut g = cache.lock().unwrap_or_else(|e| e.into_inner());
        if g.contains_key(address) {
            return;
        }
        g.insert(address.to_string(), None);
    }
    let Ok(ip) = address.parse::<IpAddr>() else { return };
    let cache = cache.clone();
    let address = address.to_string();
    let _ = std::thread::Builder::new().name("reverse-dns".into()).spawn(move || {
        let name = reverse(ip);
        cache.lock().unwrap_or_else(|e| e.into_inner()).insert(address, name);
        repaint();
    });
}

#[cfg(windows)]
fn reverse(ip: IpAddr) -> Option<String> {
    use windows_sys::Win32::Networking::WinSock::{GetNameInfoW, AF_INET, AF_INET6, NI_NAMEREQD, SOCKADDR, SOCKADDR_IN, SOCKADDR_IN6};
    // std has already started Winsock by the time a controller has been reached.
    let mut host = [0u16; 1025];
    let rc = unsafe {
        match ip {
            IpAddr::V4(v4) => {
                let mut sa: SOCKADDR_IN = std::mem::zeroed();
                sa.sin_family = AF_INET;
                sa.sin_addr.S_un.S_addr = u32::from_ne_bytes(v4.octets());
                GetNameInfoW(&sa as *const _ as *const SOCKADDR, std::mem::size_of::<SOCKADDR_IN>() as i32, host.as_mut_ptr(), host.len() as u32, std::ptr::null_mut(), 0, NI_NAMEREQD as i32)
            }
            IpAddr::V6(v6) => {
                let mut sa: SOCKADDR_IN6 = std::mem::zeroed();
                sa.sin6_family = AF_INET6;
                sa.sin6_addr.u.Byte = v6.octets();
                GetNameInfoW(&sa as *const _ as *const SOCKADDR, std::mem::size_of::<SOCKADDR_IN6>() as i32, host.as_mut_ptr(), host.len() as u32, std::ptr::null_mut(), 0, NI_NAMEREQD as i32)
            }
        }
    };
    if rc != 0 {
        return None;
    }
    let len = host.iter().position(|&c| c == 0).unwrap_or(host.len());
    let name = String::from_utf16_lossy(&host[..len]);
    (!name.is_empty() && name != ip.to_string()).then_some(name)
}

#[cfg(not(windows))]
fn reverse(_ip: IpAddr) -> Option<String> {
    None
}

/// True when another copy of this program is already running for this user. Two
/// copies on one controller break each other's streams (InfoStream is
/// single-tenant), so the window says so; it does not refuse to start, because two
/// copies on two different controllers are fine.
#[cfg(windows)]
pub fn another_instance() -> bool {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name: Vec<u16> = "Local\\ABB-Signal-Spy-instance".encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        // The handle is kept open for the life of the process on purpose.
        let h = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        !h.is_null() && GetLastError() == ERROR_ALREADY_EXISTS
    }
}

#[cfg(not(windows))]
pub fn another_instance() -> bool {
    false
}

/// A message box for failures before any window exists.
#[cfg(windows)]
pub fn fatal_box(text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let t: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let c: Vec<u16> = "ABB Signal Spy".encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        MessageBoxW(std::ptr::null_mut(), t.as_ptr(), c.as_ptr(), MB_OK | MB_ICONERROR);
    }
}

#[cfg(not(windows))]
pub fn fatal_box(text: &str) {
    eprintln!("{text}");
}
