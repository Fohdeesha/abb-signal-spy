//! What the controller says back: command replies, the SUBSCRIBE reply, and the
//! handshake announce with its connected-client list.
//!
//! Measured on the RW6 VC (2026-09-25):
//!
//! ```text
//! command reply  RAD kind 0x20, format 2: u32 status, then NUL-terminated text
//!                  0x00048000 "" / "-StreamId 215 -SampleTime 4.032"      (accepted)
//!                  0x00000000 ""                                          (StreamConnect)
//!                  0xC004FFFE "ERROR: ...rdh_infostream.cpp[379]: code: 0xc004fffe
//!                              Failed to define moc signal, status -50228 streamId -1; "
//! SUBSCRIBE      RAD kind 0x23, format 2: 0x00000000 "TRUE 0 155974524"
//! handshake      service 6, two RADs: kind 0x21 "{system id}", then kind 0x23 format 1
//!                  "<i><cs><c a=127.0.0.1/><c a=127.0.0.1/></cs></i>"
//! ```
//!
//! The status word's top bit marks a failure, the way an HRESULT does. The
//! controller's own reason is the `status` number inside the text.

use crate::wire::{rad_format, rad_kind, Rad};

/// Longest reply text kept. A real reply is one line; this bounds what a flood of
/// frames echoing one transaction id can make us hold.
pub const MAX_REPLY_TEXT: usize = 1024;
/// Most client entries read from a handshake. ABB documents three LAN clients.
pub const MAX_CLIENTS: usize = 64;

/// Latin-1 text up to the first NUL, capped.
pub fn latin1(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    b[..end.min(MAX_REPLY_TEXT)].iter().map(|&c| c as char).collect()
}

/// A reply to one request, from its RAD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// The u32 at the front of a format-2 RAD; `None` for any other format.
    pub status: Option<u32>,
    pub text: String,
}

impl Reply {
    pub fn from_rad(rad: &Rad<'_>) -> Reply {
        if rad.format == rad_format::STATUS_TEXT && rad.data.len() >= 4 {
            Reply {
                status: Some(u32::from_be_bytes(rad.data[0..4].try_into().unwrap())),
                text: latin1(&rad.data[4..]),
            }
        } else {
            Reply { status: None, text: latin1(rad.data) }
        }
    }

    /// The controller said no. Either the status word's failure bit, or the text
    /// itself: a define refusal always carries "streamId -1" and a status.
    pub fn is_failure(&self) -> bool {
        self.status.is_some_and(|s| s & 0x8000_0000 != 0) || self.text.starts_with("ERROR")
    }

    /// The controller's reason code (`status -50228`), if the text carries one.
    pub fn controller_status(&self) -> Option<i64> {
        find_signed_after(&self.text, "status")
    }

    pub fn stream_id(&self) -> Option<u32> {
        parse_stream_id(&self.text)
    }

    /// The native sample time the controller reported, in milliseconds.
    pub fn sample_time_ms(&self) -> Option<f64> {
        let key = "-SampleTime";
        let mut from = 0;
        while let Some(p) = self.text[from..].find(key) {
            let after = from + p + key.len();
            let rest = &self.text[after..];
            let trimmed = rest.trim_start();
            if trimmed.len() != rest.len() {
                let num: String = trimmed.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
                if let Ok(v) = num.parse::<f64>()
                    && v.is_finite() && v > 0.0 && v < 1.0e6 {
                        return Some(v);
                    }
            }
            from = after;
        }
        None
    }

    /// Text safe to show a person: the build-machine path ABB puts in front of
    /// every error is noise to a user and is cut down to the message.
    pub fn summary(&self) -> String {
        let t = self.text.trim();
        if let Some(p) = t.find("]: ")
            && t.starts_with("ERROR") {
                return t[p + 3..].trim().trim_end_matches(';').trim().to_string();
            }
        t.to_string()
    }
}

/// "-StreamId" + whitespace + decimal digits, within u32: the reference's
/// `-StreamId\s+(\d+)`, first occurrence that parses. A garbled reply maps nothing
/// (the bridge's old strtol mapped "-StreamId abc" to stream 0), and a refusal's
/// "streamId -1" is not a stream.
pub fn parse_stream_id(text: &str) -> Option<u32> {
    let key = "-StreamId";
    let mut from = 0;
    while let Some(p) = text[from..].find(key) {
        let after = from + p + key.len();
        from = after;
        let rest = &text[after..];
        let trimmed = rest.trim_start();
        if trimmed.len() == rest.len() {
            continue; // "-StreamIdX": another word
        }
        let digits: String = trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() || digits.len() > 10 {
            continue;
        }
        if let Ok(v) = digits.parse::<u64>()
            && let Ok(id) = u32::try_from(v) {
                return Some(id);
            }
    }
    None
}

/// The signed integer after `word` (case-insensitive, whole word), e.g. the -50228
/// in "... status -50228 streamId -1".
fn find_signed_after(text: &str, word: &str) -> Option<i64> {
    let lower = text.to_ascii_lowercase();
    let word = word.to_ascii_lowercase();
    let mut from = 0;
    while let Some(p) = lower[from..].find(&word) {
        let start = from + p;
        let after = start + word.len();
        from = after;
        let boundary_before = start == 0 || !lower.as_bytes()[start - 1].is_ascii_alphanumeric();
        if !boundary_before {
            continue;
        }
        let rest = &text[after..];
        let trimmed = rest.trim_start();
        if trimmed.len() == rest.len() {
            continue;
        }
        let bytes = trimmed.as_bytes();
        let mut end = 0;
        if end < bytes.len() && (bytes[end] == b'-' || bytes[end] == b'+') {
            end += 1;
        }
        let digits_start = end;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end == digits_start || end - digits_start > 12 {
            continue;
        }
        if let Ok(v) = trimmed[..end].parse::<i64>() {
            return Some(v);
        }
    }
    None
}

/// What a controller status code means, for a person. The -50xxx codes are MOC
/// errors offset by 50000 (ABB's TestSignalLogger), the -30x ones the log server's.
pub fn describe_status(code: i64) -> Option<&'static str> {
    Some(match code {
        -50228 => "unknown signal number: this controller does not produce it (robot axes use the 4-digit numbers)",
        -50229 => "unknown mechanical unit: no unit of that name on this controller",
        -50231 => "the mechanical unit is not active",
        -50348 => "no channel available: the controller has no free test-signal channel",
        -50133 => "no such signal for this axis",
        -50461 => "the test-signal service is not installed on this controller",
        -303 => "no joint found: that axis does not exist on this mechanical unit",
        -301 => "taken by another RobAPI client",
        -302 => "taken by another external client",
        -304 => "no RobAPI client connected",
        _ => return None,
    })
}

/// A refused define costs an entry in the controller's event log (for an unknown
/// signal number, 50228 "Unknown log signal number"). Said wherever a refusal is
/// shown, so the entries are not a mystery to whoever reads that log next.
pub const EVENT_LOG_NOTE: &str = "a refused define writes an entry into the controller's event log (50228 for an unknown signal number)";

/// The SUBSCRIBE reply: "TRUE 0 <subscription id>".
pub fn parse_subscription(text: &str) -> Option<u32> {
    let mut it = text.split_whitespace();
    if !it.next()?.eq_ignore_ascii_case("TRUE") {
        return None;
    }
    let _ = it.next()?;
    it.next()?.parse::<u32>().ok()
}

/// One entry of the handshake's connected-client list.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ClientEntry {
    /// The `a` attribute: the client's address as the controller sees it.
    pub address: String,
    /// Every attribute, in order, so a controller that reports more than the VC's
    /// address (a name, a client type) shows it rather than losing it.
    pub attributes: Vec<(String, String)>,
}

/// The handshake reply.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Announce {
    pub system_id: Option<String>,
    pub clients: Vec<ClientEntry>,
    /// The client-list text as received, for the diagnostics.
    pub raw_client_list: Option<String>,
    pub ctrl1: u32,
}

impl Announce {
    pub fn from_rads<'a>(rads: impl Iterator<Item = Rad<'a>>, ctrl1: u32) -> Announce {
        let mut a = Announce { ctrl1, ..Announce::default() };
        for rad in rads {
            let text = latin1(rad.data);
            if rad.kind == rad_kind::SYSTEM_ID && a.system_id.is_none() {
                let t = text.trim();
                if !t.is_empty() {
                    a.system_id = Some(t.chars().take(128).collect());
                }
            } else if text.contains("<cs") {
                a.clients = parse_client_list(&text);
                a.raw_client_list = Some(text);
            }
        }
        a
    }
}

/// `<i><cs><c a=127.0.0.1/>...</cs></i>`. Attribute values may be unquoted (the VC)
/// or quoted; anything malformed ends the list rather than guessing.
pub fn parse_client_list(text: &str) -> Vec<ClientEntry> {
    let mut out = Vec::new();
    let Some(start) = text.find("<cs") else { return out };
    let mut rest = &text[start + 3..];
    while out.len() < MAX_CLIENTS {
        let Some(p) = rest.find("<c") else { break };
        let after = &rest[p + 2..];
        // "<cs" again, or "<cx": not a client element.
        match after.chars().next() {
            Some(c) if c.is_whitespace() || c == '/' || c == '>' => {}
            _ => {
                rest = after;
                continue;
            }
        }
        let Some(close) = after.find('>') else { break };
        let body = after[..close].trim_end_matches('/');
        let attributes = parse_attributes(body);
        let address = attributes.iter().find(|(k, _)| k == "a").map(|(_, v)| v.clone()).unwrap_or_default();
        out.push(ClientEntry { address, attributes });
        rest = &after[close + 1..];
    }
    out
}

fn parse_attributes(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let b = body.as_bytes();
    let mut i = 0;
    while i < b.len() && out.len() < 16 {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let ks = i;
        while i < b.len() && b[i] != b'=' && !b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() || b[i] != b'=' || i == ks {
            break;
        }
        let key = body[ks..i].to_string();
        i += 1;
        let value = if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
            let q = b[i];
            i += 1;
            let vs = i;
            while i < b.len() && b[i] != q {
                i += 1;
            }
            let v = body[vs..i].to_string();
            i = (i + 1).min(b.len());
            v
        } else {
            let vs = i;
            while i < b.len() && !b[i].is_ascii_whitespace() {
                i += 1;
            }
            body[vs..i].to_string()
        };
        out.push((key.chars().take(32).collect(), value.chars().take(128).collect()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(status: u32, text: &str) -> Reply {
        let mut d = status.to_be_bytes().to_vec();
        d.extend_from_slice(text.as_bytes());
        d.push(0);
        Reply::from_rad(&Rad { kind: 0x20, format: 2, data: &d })
    }

    #[test]
    fn captured_replies() {
        let ok = reply(0x0004_8000, "-StreamId 215 -SampleTime 4.032");
        assert!(!ok.is_failure());
        assert_eq!(ok.stream_id(), Some(215));
        assert_eq!(ok.sample_time_ms(), Some(4.032));
        let slow = reply(0x0004_8000, "-StreamId 212 -SampleTime 24.192");
        assert_eq!(slow.sample_time_ms(), Some(24.192));

        let refused = reply(0xC004_FFFE, "ERROR: C:\\BUILDAGENTS\\SEABB-IS-13906.1\\_work\\71\\s\\RW\\Areas\\RobApi\\Components\\RobDomainHooks\\rdh_infostream.cpp[379]: code: 0xc004fffe Failed to define moc signal, status -50228 streamId -1; ");
        assert!(refused.is_failure());
        assert_eq!(refused.stream_id(), None, "streamId -1 is not a stream");
        assert_eq!(refused.controller_status(), Some(-50228));
        assert_eq!(refused.summary(), "code: 0xc004fffe Failed to define moc signal, status -50228 streamId -1");
        assert!(describe_status(-50228).is_some());
        assert_eq!(reply(0xC004_FFFE, "ERROR: x[1]: status -303 streamId -1;").controller_status(), Some(-303));

        let plain = reply(0, "");
        assert!(!plain.is_failure());
        assert_eq!(plain.stream_id(), None);
    }

    #[test]
    fn stream_id_is_strict() {
        assert_eq!(parse_stream_id("-StreamId 41"), Some(41));
        assert_eq!(parse_stream_id("Ok -StreamId\t7 -SampleTime 4"), Some(7));
        assert_eq!(parse_stream_id("-StreamId 4294967295"), Some(u32::MAX));
        assert_eq!(parse_stream_id("-StreamId abc"), None);
        assert_eq!(parse_stream_id("-StreamId -5"), None);
        assert_eq!(parse_stream_id("-StreamId"), None);
        assert_eq!(parse_stream_id("-StreamId 4294967296"), None);
        assert_eq!(parse_stream_id("-StreamId 99999999999999999999999"), None);
        assert_eq!(parse_stream_id("-StreamIdX 5"), None);
        assert_eq!(parse_stream_id(""), None);
        assert_eq!(parse_stream_id("-StreamId ? | -StreamId 5"), Some(5));
    }

    #[test]
    fn subscription_reply() {
        assert_eq!(parse_subscription("TRUE 0 155974524"), Some(155_974_524));
        assert_eq!(parse_subscription("FALSE 0 1"), None);
        assert_eq!(parse_subscription("TRUE 0"), None);
        assert_eq!(parse_subscription("TRUE 0 -1"), None);
    }

    #[test]
    fn client_list() {
        let c = parse_client_list("<i><cs><c a=127.0.0.1/><c a=127.0.0.1/></cs></i>");
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].address, "127.0.0.1");
        let c = parse_client_list("<i><cs><c a=\"192.0.2.27\" n='RobotStudio'/><c a=10.0.0.5 t=2 /></cs></i>");
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].address, "192.0.2.27");
        assert_eq!(c[0].attributes[1], ("n".to_string(), "RobotStudio".to_string()));
        assert_eq!(c[1].attributes, vec![("a".into(), "10.0.0.5".into()), ("t".into(), "2".into())]);
        assert!(parse_client_list("").is_empty());
        assert!(parse_client_list("<i><cs></cs></i>").is_empty());
        // Unterminated: stops, does not panic.
        assert!(parse_client_list("<cs><c a=1.2.3.4").is_empty());
        // A flood is capped.
        let many = format!("<cs>{}</cs>", "<c a=1.1.1.1/>".repeat(1000));
        assert_eq!(parse_client_list(&many).len(), MAX_CLIENTS);
    }
}
