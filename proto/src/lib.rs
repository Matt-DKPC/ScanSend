//! ScanSend wire protocol.
//!
//! Discovery (UDP, like LocalSend):
//!   phone  -> multicast 239.255.83.83:53318 and broadcast 255.255.255.255:53318
//!             `{"t":"query","v":1}`
//!   PC     -> unicast reply to the sender's address
//!             `{"t":"announce","v":1,"id":"...","name":"Flaky USB","port":53319}`
//!
//! The phone only *sends* multicast and receives a plain unicast reply, which
//! avoids Android's MulticastLock and most of iOS's multicast restrictions.
//!
//! Phones that can't use UDP (iPhone without Apple's multicast entitlement)
//! find computers LocalSend-style instead: `GET /api/v1/info` on every address
//! of the phone's /24 at DEFAULT_HTTP_PORT.
//!
//! Pairing + scans (HTTP on the announced port):
//!   POST /api/v1/connect         ConnectRequest -> PairResponse   (held open until
//!                                the computer's user taps Accept: 403 = declined,
//!                                408 = nobody answered, 429 = too many waiting)
//!   POST /api/v1/connect/cancel  CancelRequest  -> 204
//!   POST /api/v1/scan  Scan  (Bearer token)   -> 204
//!   POST /api/v1/disconnect  (Bearer token)   -> 204  (phone tapped Forget)
//!   GET  /api/v1/info                         -> Info
//!
//! Older PIN pairing (hidden in the apps for now, kept in case it's needed):
//!   POST /api/v1/pair  PairRequest            -> PairResponse
//! or scanning the QR the PC shows, which contains
//! `scansend://pair?h=<ip>[,<ip>...]&p=<port>&pin=<pin>&n=<name>`.
//! Several addresses are listed when the PC has more than one network
//! (Wi-Fi + Ethernet, Tailscale...); the phone uses the first that answers.

use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

pub const PROTOCOL_VERSION: u32 = 1;
pub const DISCOVERY_PORT: u16 = 53318;
pub const DEFAULT_HTTP_PORT: u16 = 53319;
/// Administratively scoped (LAN-only) multicast group.
pub const MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(239, 255, 83, 83);
/// Longest scan payload accepted. QR codes top out around 7 KB of numeric data.
pub const MAX_SCAN_LEN: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum Discovery {
    Query { v: u32 },
    Announce {
        v: u32,
        id: String,
        name: String,
        port: u16,
        /// The computer's real name and system ("Front-Desk-PC", "Mac"), shown under the
        /// fun name so a computer can't be mistaken for a phone. Older computers leave them out.
        #[serde(default)]
        device: Option<String>,
        #[serde(default)]
        os: Option<String>,
    },
}

impl Discovery {
    pub fn query() -> Self {
        Discovery::Query { v: PROTOCOL_VERSION }
    }
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("discovery serializes")
    }
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        serde_json::from_slice(b).ok()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairRequest {
    pub pin: String,
    pub device_name: String,
    /// Stable random id of the phone, so reconnecting replaces its old entry.
    #[serde(default)]
    pub phone_id: Option<String>,
}

/// Longest the computer waits for its user to tap Accept or Decline.
pub const CONNECT_WAIT_SECS: u64 = 60;

/// "Let this phone connect?" — shown on the computer with Accept / Decline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectRequest {
    /// Random per attempt, so the phone can cancel it.
    pub request_id: String,
    pub device_name: String,
    /// Stable random id of the phone, so reconnecting replaces its old entry.
    #[serde(default)]
    pub phone_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelRequest {
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairResponse {
    pub token: String,
    pub receiver_id: String,
    pub receiver_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scan {
    /// The decoded barcode / QR text.
    pub text: String,
    /// Barcode symbology as reported by the scanner, e.g. "QR_CODE", "EAN_13".
    #[serde(default)]
    pub format: Option<String>,
    /// Random id per scan so a retried request is not typed twice.
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Info {
    pub v: u32,
    pub id: String,
    pub name: String,
    /// See `Discovery::Announce`.
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub os: Option<String>,
}

/// "Mac", "Windows" or "Linux": what a receiver reports as `os`.
pub fn os_label() -> &'static str {
    match std::env::consts::OS {
        "macos" => "Mac",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}

/// Contents of the pairing QR code shown by the receiver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairUri {
    /// Best address first.
    pub hosts: Vec<String>,
    pub port: u16,
    pub pin: String,
    pub name: String,
}

pub const PAIR_URI_PREFIX: &str = "scansend://pair";

impl PairUri {
    pub fn to_uri(&self) -> String {
        let mut u = url::Url::parse(PAIR_URI_PREFIX).expect("static url");
        u.query_pairs_mut()
            .append_pair("h", &self.hosts.join(","))
            .append_pair("p", &self.port.to_string())
            .append_pair("pin", &self.pin)
            .append_pair("n", &self.name);
        u.to_string()
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if !s.starts_with(PAIR_URI_PREFIX) {
            return None;
        }
        let u = url::Url::parse(s).ok()?;
        let (mut hosts, mut port, mut pin, mut name) = (Vec::new(), None, None, None);
        for (k, v) in u.query_pairs() {
            match k.as_ref() {
                "h" => {
                    hosts = v
                        .split(',')
                        .map(|h| h.trim().to_string())
                        .filter(|h| h.parse::<std::net::IpAddr>().is_ok())
                        .collect()
                }
                "p" => port = v.parse().ok(),
                "pin" => pin = Some(v.into_owned()),
                "n" => name = Some(v.into_owned()),
                _ => {}
            }
        }
        if hosts.is_empty() {
            return None;
        }
        Some(PairUri {
            hosts,
            port: port.unwrap_or(DEFAULT_HTTP_PORT),
            pin: pin?,
            name: name.unwrap_or_else(|| "Computer".into()),
        })
    }
}

const NAME_ADJECTIVES: &[&str] = &[
    "Flaky", "Dumb", "Sleepy", "Grumpy", "Wobbly", "Spicy", "Sneaky", "Dusty", "Crusty", "Bouncy",
    "Fuzzy", "Sassy", "Cranky", "Soggy", "Tiny", "Chunky", "Glitchy", "Laggy", "Loose", "Overclocked",
    "Sticky", "Jittery", "Moody", "Shiny", "Clumsy", "Squeaky", "Lazy", "Toasty", "Wonky", "Bent",
];
const NAME_GADGETS: &[&str] = &[
    "USB", "HDMI", "Dongle", "Floppy", "Modem", "Router", "Toaster", "Keyboard", "Mouse", "Cable",
    "Capacitor", "Resistor", "Fan", "Heatsink", "Pixel", "Byte", "Printer", "Webcam", "Joystick", "Battery",
    "Charger", "Adapter", "Antenna", "Jumper", "Breadboard", "Firmware", "Ethernet", "Thumbdrive", "Pager", "Diode",
];

/// A friendly device name like "Flaky USB" (pass any random number).
pub fn funny_name(seed: u64) -> String {
    let a = NAME_ADJECTIVES[(seed % NAME_ADJECTIVES.len() as u64) as usize];
    let g = NAME_GADGETS[((seed / NAME_ADJECTIVES.len() as u64) % NAME_GADGETS.len() as u64) as usize];
    format!("{a} {g}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn funny_names_are_two_words() {
        for seed in [0, 1, 29, 30, 899, u64::MAX] {
            assert_eq!(funny_name(seed).split(' ').count(), 2);
        }
        assert_eq!(funny_name(0), "Flaky USB");
    }

    #[test]
    fn pair_uri_round_trip() {
        let p = PairUri {
            hosts: vec!["192.168.1.20".into(), "100.101.1.2".into()],
            port: 53319,
            pin: "042137".into(),
            name: "Front Desk PC & stuff".into(),
        };
        assert_eq!(PairUri::parse(&p.to_uri()), Some(p));
        assert_eq!(PairUri::parse("hello world"), None);
        // v1 QR codes (single host) still parse.
        let v1 = PairUri::parse("scansend://pair?h=192.168.1.5&p=53319&pin=123456&n=PC").unwrap();
        assert_eq!(v1.hosts, vec!["192.168.1.5".to_string()]);
        assert_eq!(PairUri::parse("scansend://pair?h=not-an-ip&pin=1"), None);
    }

    #[test]
    fn discovery_json() {
        let q = Discovery::query();
        assert_eq!(String::from_utf8(q.to_bytes()).unwrap(), r#"{"t":"query","v":1}"#);
        assert_eq!(Discovery::from_bytes(&q.to_bytes()), Some(q));
    }
}
