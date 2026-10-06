use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

pub type Hostnames = Arc<Mutex<HashMap<String, Option<String>>>>;

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

pub const WINDOW_SLOTS: &str = "Local\\ABB-Signal-Spy-window-";
const SLOTS: usize = 16;

pub struct Windows {
    prefix: Option<String>,
    mine: Option<(usize, usize)>,
}

impl Windows {
    #[cfg(test)]
    pub fn none() -> Windows {
        Windows { prefix: None, mine: None }
    }

    pub fn join(prefix: &str) -> Windows {
        let mine = (0..SLOTS).find_map(|i| slot_take(&slot_name(prefix, i)).map(|h| (i, h)));
        Windows { prefix: Some(prefix.to_string()), mine }
    }

    pub fn others(&self) -> Option<usize> {
        let prefix = self.prefix.as_deref()?;
        Some((0..SLOTS).filter(|&i| self.mine.is_none_or(|(m, _)| m != i)).filter(|&i| slot_held_elsewhere(&slot_name(prefix, i))).count())
    }
}

impl Drop for Windows {
    fn drop(&mut self) {
        if let Some((_, h)) = self.mine.take() {
            slot_give_back(h);
        }
    }
}

fn slot_name(prefix: &str, i: usize) -> Vec<u16> {
    format!("{prefix}{i}").encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn slot_take(name: &[u16]) -> Option<usize> {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
    unsafe {
        let h = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        if h.is_null() {
            return None;
        }
        let r = WaitForSingleObject(h, 0);
        if r == WAIT_OBJECT_0 || r == WAIT_ABANDONED {
            Some(h as usize)
        } else {
            CloseHandle(h);
            None
        }
    }
}

#[cfg(windows)]
fn slot_held_elsewhere(name: &[u16]) -> bool {
    match slot_take(name) {
        Some(h) => {
            slot_give_back(h);
            false
        }
        None => true,
    }
}

#[cfg(windows)]
fn slot_give_back(h: usize) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::ReleaseMutex;
    unsafe {
        ReleaseMutex(h as windows_sys::Win32::Foundation::HANDLE);
        CloseHandle(h as windows_sys::Win32::Foundation::HANDLE);
    }
}

#[cfg(not(windows))]
fn slot_take(_name: &[u16]) -> Option<usize> {
    None
}

#[cfg(not(windows))]
fn slot_held_elsewhere(_name: &[u16]) -> bool {
    false
}

#[cfg(not(windows))]
fn slot_give_back(_h: usize) {}

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

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn each_window_knows_how_many_others_are_open_and_when_one_closes() {
        let prefix = format!("Local\\ABB-Signal-Spy-test-{}-", std::process::id());
        let (to_a, a_rx) = mpsc::channel::<()>();
        let (from_a, a_said) = mpsc::channel::<Option<usize>>();
        let p = prefix.clone();
        let a = std::thread::spawn(move || {
            let w = Windows::join(&p);
            from_a.send(w.others()).unwrap();
            a_rx.recv().unwrap();
            from_a.send(w.others()).unwrap();
            a_rx.recv().unwrap();
        });
        assert_eq!(a_said.recv().unwrap(), Some(0), "the first window alone");
        let b = Windows::join(&prefix);
        assert_eq!(b.others(), Some(1), "the second sees the first");
        to_a.send(()).unwrap();
        assert_eq!(a_said.recv().unwrap(), Some(1), "the first sees the second, which opened after it");
        to_a.send(()).unwrap();
        a.join().unwrap();
        assert_eq!(b.others(), Some(0), "the first closed: the notice goes");
        assert_eq!(Windows::none().others(), None);
    }
}
