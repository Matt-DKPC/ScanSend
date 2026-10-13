//! Rust core of the ScanSend phone app. The camera scanning itself is done by
//! tauri-plugin-barcode-scanner (called from the UI); this side finds
//! computers on the Wi-Fi, pairs with them and delivers each scan.

use rand::RngCore;
use scansend_proto::*;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{Manager, State};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Receiver {
    id: String,
    name: String,
    host: String,
    port: u16,
    token: String,
    /// Every address from the pairing QR, tried if `host` stops answering.
    #[serde(default)]
    alt_hosts: Vec<String>,
    /// The computer's system and real name, for the icon and subtitle ("Mac", "Front-Desk-PC").
    #[serde(default)]
    os: Option<String>,
    #[serde(default)]
    device: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    receivers: Vec<Receiver>,
    #[serde(default)]
    active: Option<String>,
    #[serde(default)]
    device_name: Option<String>,
    /// Random id for this phone, sent when connecting so a computer replaces
    /// (instead of duplicating) this phone's entry when it reconnects.
    #[serde(default)]
    phone_id: Option<String>,
}

/// What the UI sees (tokens never leave Rust).
#[derive(Serialize)]
struct PublicReceiver {
    id: String,
    name: String,
    address: String,
    host: String,
    active: bool,
    os: Option<String>,
    device: Option<String>,
}

#[derive(Serialize)]
struct PublicState {
    receivers: Vec<PublicReceiver>,
    device_name: String,
    /// This phone's Wi-Fi address (the one it uses to reach the computer).
    phone_ip: Option<String>,
    version: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct Found {
    id: String,
    name: String,
    host: String,
    port: u16,
    paired: bool,
    /// The computer's real name and system, when it says ("Front-Desk-PC", "Mac").
    device: Option<String>,
    os: Option<String>,
}

struct AppState {
    path: PathBuf,
    saved: Mutex<Saved>,
}

impl AppState {
    fn save(&self, s: &Saved) -> Result<(), String> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let bytes = serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?;
        std::fs::write(&self.path, bytes).map_err(|e| e.to_string())
    }

    fn public(&self) -> PublicState {
        let s = self.saved.lock().unwrap();
        PublicState {
            receivers: s
                .receivers
                .iter()
                .map(|r| PublicReceiver {
                    id: r.id.clone(),
                    name: r.name.clone(),
                    address: format!("{}:{}", r.host, r.port),
                    host: r.host.clone(),
                    os: r.os.clone(),
                    device: r.device.clone(),
                    active: s.active.as_deref() == Some(r.id.as_str()),
                })
                .collect(),
            device_name: s.device_name.clone().unwrap_or_else(|| "Phone".into()),
            phone_ip: local_ip(s.receivers.iter().find(|r| s.active.as_deref() == Some(r.id.as_str()))),
            version: env!("CARGO_PKG_VERSION"),
        }
    }
}

/// Which local address the phone would use to reach `to` (or the LAN in general).
/// Connecting a UDP socket sends nothing; it only picks a route.
fn local_ip(to: Option<&Receiver>) -> Option<String> {
    let target = to.map(|r| format!("{}:{}", r.host, r.port)).unwrap_or_else(|| "10.255.255.255:9".into());
    let sock = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    sock.connect(target).ok()?;
    let ip = sock.local_addr().ok()?.ip();
    (!ip.is_unspecified()).then(|| ip.to_string())
}

/// What to add to a "can't reach" message on iPhone, where a denied Local
/// Network permission looks exactly like a firewall (connections just time out).
#[cfg(target_os = "ios")]
const IOS_HINT: &str = "\nOn iPhone, also check Settings → Privacy & Security → Local Network → ScanSend is on. If ScanSend isn't listed there, delete and reinstall the app.";
#[cfg(not(target_os = "ios"))]
const IOS_HINT: &str = "";

/// iPhone only asks for Local Network access the first time the app sends
/// something to the LAN, and that first attempt fails while the question is on
/// screen. Send one harmless packet so the question comes up at a good moment.
#[tauri::command]
fn request_local_network() {
    let Some(ip) = local_ip(None) else { return };
    let Ok(IpAddr::V4(ip)) = ip.parse::<IpAddr>() else { return };
    let [a, b, c, _] = ip.octets();
    if let Ok(sock) = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) {
        // The discard port on the router's usual address; nothing needs to answer.
        let _ = sock.send_to(b"scansend", (Ipv4Addr::new(a, b, c, 1), 9));
    }
}

/// A fun name that isn't the same as any computer the phone knows, so it's
/// always clear which side is which.
fn random_name(taken: &[&str]) -> String {
    let clash = |n: &str| taken.iter().any(|t| t.eq_ignore_ascii_case(n));
    for _ in 0..100 {
        let n = funny_name(rand::random());
        if !clash(&n) {
            return n;
        }
    }
    format!("{} {}", funny_name(rand::random()), rand::random::<u16>() % 100)
}

fn computer_names(s: &Saved) -> Vec<&str> {
    s.receivers.iter().map(|r| r.name.as_str()).collect()
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(6))
        .build()
        .expect("http client")
}

fn friendly(e: reqwest::Error) -> String {
    if e.is_connect() || e.is_timeout() {
        "Can't reach the computer. Is ScanSend running there, and are you on the same Wi-Fi?".into()
    } else {
        e.to_string()
    }
}

/// Why one address didn't answer, in words that point at the fix.
fn why(e: &reqwest::Error) -> &'static str {
    use std::error::Error as _;
    let mut src = e.source();
    while let Some(s) = src {
        if let Some(io) = s.downcast_ref::<std::io::Error>() {
            return match io.kind() {
                std::io::ErrorKind::ConnectionRefused => "refused: ScanSend isn't running there",
                std::io::ErrorKind::NetworkUnreachable | std::io::ErrorKind::HostUnreachable => {
                    "unreachable: phone is on a different network"
                }
                std::io::ErrorKind::TimedOut => "no answer: the computer's firewall is probably blocking it",
                _ => "couldn't connect",
            };
        }
        src = s.source();
    }
    if e.is_timeout() {
        "no answer: the computer's firewall is probably blocking it"
    } else {
        "couldn't connect"
    }
}

/// Try every address at once; return the first that is really a ScanSend
/// receiver, or an explanation per address.
async fn reach(hosts: &[String], port: u16) -> Result<(String, String), String> {
    let mut set = tokio::task::JoinSet::new();
    for h in hosts.iter().cloned() {
        set.spawn(async move {
            let url = format!("http://{h}:{port}/api/v1/info");
            let r = match http().get(url).timeout(Duration::from_secs(4)).send().await {
                Ok(resp) if resp.status().is_success() => match resp.json::<Info>().await {
                    Ok(info) => Ok(info.name),
                    Err(_) => Err("answered, but it isn't ScanSend"),
                },
                Ok(_) => Err("answered, but it isn't ScanSend"),
                Err(e) => Err(why(&e)),
            };
            (h, r)
        });
    }
    let mut failures = Vec::new();
    while let Some(Ok((h, r))) = set.join_next().await {
        match r {
            Ok(name) => {
                set.abort_all();
                return Ok((h, name));
            }
            Err(reason) => failures.push(format!("{h}:{port} {reason}")),
        }
    }
    Err(format!(
        "Can't reach the computer.\n{}\nCheck the phone is on the same Wi-Fi (not mobile data or a VPN), and that the computer's firewall allows ScanSend.{IOS_HINT}",
        failures.join("\n")
    ))
}

/// `reach`, but on iPhone keep trying for a bit: the very first connection
/// fails while the Local Network question is still on screen.
async fn reach_patiently(hosts: &[String], port: u16) -> Result<(String, String), String> {
    let tries = if cfg!(target_os = "ios") { 4 } else { 1 };
    let mut last = String::new();
    for i in 0..tries {
        if i > 0 {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        match reach(hosts, port).await {
            Ok(ok) => return Ok(ok),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Every computer on the Wi-Fi: UDP announcements plus, like LocalSend, asking
/// each address of the phone's /24 directly. The direct check is what works on
/// iPhone, where UDP broadcast needs an entitlement from Apple.
async fn find_computers(wait: Duration) -> Vec<Found> {
    let (udp, scanned) = tokio::join!(discover_inner(wait), scan_subnet());
    let mut found = udp.unwrap_or_default();
    for f in scanned {
        if !found.iter().any(|g| g.id == f.id) {
            found.push(f);
        }
    }
    found.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    found
}

async fn scan_subnet() -> Vec<Found> {
    let Some(Ok(IpAddr::V4(me))) = local_ip(None).map(|ip| ip.parse::<IpAddr>()) else { return Vec::new() };
    let [a, b, c, mine] = me.octets();
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_millis(800))
        .timeout(Duration::from_millis(1500))
        .build()
        .expect("http client");
    let mut set = tokio::task::JoinSet::new();
    for d in (1..=254u8).filter(|d| *d != mine) {
        let client = client.clone();
        set.spawn(async move {
            let host = Ipv4Addr::new(a, b, c, d).to_string();
            let info: Info = client
                .get(format!("http://{host}:{DEFAULT_HTTP_PORT}/api/v1/info"))
                .send()
                .await
                .ok()?
                .json()
                .await
                .ok()?;
            Some(Found {
                id: info.id,
                name: info.name,
                host,
                port: DEFAULT_HTTP_PORT,
                paired: false,
                device: info.device,
                os: info.os,
            })
        });
    }
    let mut found = Vec::new();
    while let Some(r) = set.join_next().await {
        if let Ok(Some(f)) = r {
            found.push(f);
        }
    }
    found
}

async fn discover_inner(wait: Duration) -> Result<Vec<Found>, String> {
    let sock = tokio::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .await
        .map_err(|e| e.to_string())?;
    let _ = sock.set_broadcast(true);
    let q = Discovery::query().to_bytes();

    // Ask a few times; UDP on Wi-Fi drops packets.
    let deadline = tokio::time::Instant::now() + wait;
    let mut found: Vec<Found> = Vec::new();
    let mut buf = [0u8; 1024];
    let mut next_send = tokio::time::Instant::now();
    while tokio::time::Instant::now() < deadline {
        if tokio::time::Instant::now() >= next_send {
            let _ = sock.send_to(&q, (MULTICAST_ADDR, DISCOVERY_PORT)).await;
            let _ = sock.send_to(&q, (Ipv4Addr::BROADCAST, DISCOVERY_PORT)).await;
            next_send += Duration::from_millis(400);
        }
        let until = next_send.min(deadline);
        if let Ok(Ok((n, from))) = tokio::time::timeout_at(until, sock.recv_from(&mut buf)).await {
            if let Some(Discovery::Announce { id, name, port, device, os, .. }) = Discovery::from_bytes(&buf[..n]) {
                if let IpAddr::V4(ip) = from.ip() {
                    if !found.iter().any(|f| f.id == id) {
                        found.push(Found { id, name, host: ip.to_string(), port, paired: false, device, os });
                    }
                }
            }
        }
    }
    Ok(found)
}

#[tauri::command]
fn get_state(state: State<'_, AppState>) -> PublicState {
    state.public()
}

#[tauri::command]
fn set_device_name(name: String, state: State<'_, AppState>) -> Result<PublicState, String> {
    {
        let mut s = state.saved.lock().unwrap();
        let name = name.trim();
        if computer_names(&s).iter().any(|c| c.eq_ignore_ascii_case(name)) {
            return Err("That's the name of a paired computer. Pick a different one so you can tell them apart.".into());
        }
        let new = if name.is_empty() { random_name(&computer_names(&s)) } else { name.chars().take(64).collect() };
        s.device_name = Some(new);
        state.save(&s)?;
    }
    Ok(state.public())
}

#[tauri::command]
fn randomize_device_name(state: State<'_, AppState>) -> Result<PublicState, String> {
    {
        let mut s = state.saved.lock().unwrap();
        s.device_name = Some(random_name(&computer_names(&s)));
        state.save(&s)?;
    }
    Ok(state.public())
}

#[tauri::command]
fn set_active(id: String, state: State<'_, AppState>) -> Result<PublicState, String> {
    {
        let mut s = state.saved.lock().unwrap();
        if !s.receivers.iter().any(|r| r.id == id) {
            return Err("Unknown computer".into());
        }
        s.active = Some(id);
        state.save(&s)?;
    }
    Ok(state.public())
}

#[tauri::command]
fn forget(id: String, state: State<'_, AppState>) -> Result<PublicState, String> {
    let target = {
        let mut s = state.saved.lock().unwrap();
        let target = s.receivers.iter().find(|r| r.id == id).cloned();
        s.receivers.retain(|r| r.id != id);
        if s.active.as_deref() == Some(id.as_str()) {
            s.active = s.receivers.first().map(|r| r.id.clone());
        }
        state.save(&s)?;
        target
    };
    // Tell the computer in the background, so it drops this phone from its list too.
    // Best effort: it may be off or on another network; the phone doesn't wait for it.
    if let Some(r) = target {
        tauri::async_runtime::spawn(async move {
            let _ = http()
                .post(format!("http://{}:{}/api/v1/disconnect", r.host, r.port))
                .bearer_auth(&r.token)
                .send()
                .await;
        });
    }
    Ok(state.public())
}

#[tauri::command]
async fn discover(state: State<'_, AppState>) -> Result<Vec<Found>, String> {
    let mut found = find_computers(Duration::from_millis(1500)).await;
    let mut s = state.saved.lock().unwrap();
    let mut changed = false;
    for f in &mut found {
        if let Some(r) = s.receivers.iter_mut().find(|r| r.id == f.id) {
            f.paired = true;
            // Remember what kind of computer it is, for the icon under "Your computers".
            if f.os.is_some() && (r.os != f.os || r.device != f.device) {
                r.os = f.os.clone();
                r.device = f.device.clone();
                changed = true;
            }
        }
    }
    if changed {
        let _ = state.save(&s);
    }
    Ok(found)
}

/// Pair with a computer reachable at any of `hosts` (best first).
async fn pair_inner(hosts: Vec<String>, port: u16, pin: String, state: &AppState) -> Result<PublicState, String> {
    for h in &hosts {
        h.parse::<IpAddr>().map_err(|_| format!("Bad address in pairing code: {h}"))?;
    }
    let (host, computer_name) = reach_patiently(&hosts, port).await?;
    let device_name = name_for_pairing(&computer_name, state)?;
    let resp = http()
        .post(format!("http://{host}:{port}/api/v1/pair"))
        .json(&PairRequest { pin, device_name, phone_id: phone_id(state) })
        .send()
        .await
        .map_err(|e| format!("Lost the connection to {host} while pairing ({}). Try again.", why(&e)))?;
    match resp.status() {
        s if s.is_success() => {}
        reqwest::StatusCode::FORBIDDEN => {
            return Err("Wrong or expired PIN. Use the PIN or QR code shown on the computer right now (it changes after each pairing).".into())
        }
        s => return Err(format!("Computer replied {s} while pairing")),
    }
    let p: PairResponse = resp.json().await.map_err(|e| e.to_string())?;
    remember_receiver(p, host, port, hosts, state)
}

fn phone_id(state: &AppState) -> Option<String> {
    state.saved.lock().unwrap().phone_id.clone()
}

/// This phone's name for pairing with `computer_name`: never the same as the
/// computer (or another paired one), so it's clear which side is which.
fn name_for_pairing(computer_name: &str, state: &AppState) -> Result<String, String> {
    let mut s = state.saved.lock().unwrap();
    let mut taken = computer_names(&s);
    taken.push(computer_name);
    let current = s.device_name.clone().unwrap_or_default();
    if current.is_empty() || taken.iter().any(|t| t.eq_ignore_ascii_case(&current)) {
        let new = random_name(&taken);
        s.device_name = Some(new.clone());
        state.save(&s)?;
        Ok(new)
    } else {
        Ok(current)
    }
}

fn remember_receiver(
    p: PairResponse,
    host: String,
    port: u16,
    alt_hosts: Vec<String>,
    state: &AppState,
) -> Result<PublicState, String> {
    {
        let mut s = state.saved.lock().unwrap();
        s.receivers.retain(|r| r.id != p.receiver_id);
        s.receivers.push(Receiver {
            id: p.receiver_id.clone(),
            name: p.receiver_name,
            host,
            port,
            token: p.token,
            alt_hosts,
            os: None,
            device: None,
        });
        s.active = Some(p.receiver_id);
        state.save(&s)?;
    }
    Ok(state.public())
}

/// LocalSend-style: ask the computer to let this phone connect. Returns once
/// someone taps Accept or Decline there (or nobody does within a minute).
#[tauri::command]
async fn request_connect(
    host: String,
    port: u16,
    name: String,
    request_id: String,
    state: State<'_, AppState>,
) -> Result<PublicState, String> {
    host.parse::<IpAddr>().map_err(|_| format!("Bad address: {host}"))?;
    let device_name = name_for_pairing(&name, &state)?;
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(CONNECT_WAIT_SECS + 10))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .post(format!("http://{host}:{port}/api/v1/connect"))
        .json(&ConnectRequest { request_id, device_name, phone_id: phone_id(&state) })
        .send()
        .await
        .map_err(|e| format!("Can't reach \"{name}\" ({}).{IOS_HINT}", why(&e)))?;
    match resp.status() {
        s if s.is_success() => {}
        reqwest::StatusCode::FORBIDDEN => return Err(format!("\"{name}\" declined.")),
        reqwest::StatusCode::REQUEST_TIMEOUT => return Err("cancelled".into()),
        reqwest::StatusCode::TOO_MANY_REQUESTS => {
            return Err(format!("\"{name}\" has other phones waiting. Try again in a minute."))
        }
        reqwest::StatusCode::NOT_FOUND => {
            return Err(format!("\"{name}\" runs an older ScanSend. Update it on the computer."))
        }
        s => return Err(format!("Computer replied {s}")),
    }
    let p: PairResponse = resp.json().await.map_err(|e| e.to_string())?;
    remember_receiver(p, host.clone(), port, vec![host], &state)
}

/// The phone tapped Cancel while waiting: take the question off the computer.
#[tauri::command]
async fn cancel_connect(host: String, port: u16, request_id: String) {
    let _ = http()
        .post(format!("http://{host}:{port}/api/v1/connect/cancel"))
        .json(&CancelRequest { request_id })
        .send()
        .await;
}

#[tauri::command]
async fn pair(host: String, port: u16, pin: String, state: State<'_, AppState>) -> Result<PublicState, String> {
    pair_inner(vec![host], port, pin, &state).await
}

/// Pair by scanning the QR code the computer shows.
#[tauri::command]
async fn pair_uri(uri: String, state: State<'_, AppState>) -> Result<PublicState, String> {
    let p = PairUri::parse(&uri).ok_or("That isn't a ScanSend pairing code.")?;
    pair_inner(p.hosts, p.port, p.pin, &state).await
}

#[derive(Serialize)]
struct Sent {
    to: String,
}

#[tauri::command]
async fn send_scan(text: String, format: Option<String>, state: State<'_, AppState>) -> Result<Sent, String> {
    let target = {
        let s = state.saved.lock().unwrap();
        let id = s.active.clone().ok_or("Pair with a computer first.")?;
        s.receivers.iter().find(|r| r.id == id).cloned().ok_or("Pair with a computer first.")?
    };
    let mut idb = [0u8; 8];
    rand::rng().fill_bytes(&mut idb);
    let scan = Scan { text, format, id: Some(hex::encode(idb)) };

    let post = |host: String, port: u16| {
        let scan = scan.clone();
        let token = target.token.clone();
        async move {
            http()
                .post(format!("http://{host}:{port}/api/v1/scan"))
                .bearer_auth(token)
                .json(&scan)
                .send()
                .await
        }
    };

    let remember = |host: &str, port: u16| {
        let mut s = state.saved.lock().unwrap();
        if let Some(r) = s.receivers.iter_mut().find(|r| r.id == target.id) {
            r.host = host.to_string();
            r.port = port;
        }
        let _ = state.save(&s);
    };

    let mut result = post(target.host.clone(), target.port).await;

    if let Err(first_err) = &result {
        if first_err.is_connect() || first_err.is_timeout() {
            let first_why = why(first_err);
            // 1. The computer's other addresses (Wi-Fi vs Ethernet, Tailscale...).
            let others: Vec<String> =
                target.alt_hosts.iter().filter(|h| **h != target.host).cloned().collect();
            let mut new_addr = None;
            if !others.is_empty() {
                if let Ok((h, _)) = reach(&others, target.port).await {
                    new_addr = Some((h, target.port));
                }
            }
            // 2. Its IP changed (DHCP): find it again by its id.
            if new_addr.is_none() {
                if let Some(f) = find_computers(Duration::from_millis(1200)).await.into_iter().find(|f| f.id == target.id) {
                    new_addr = Some((f.host, f.port));
                }
            }
            match new_addr {
                Some((h, p)) => {
                    remember(&h, p);
                    result = post(h, p).await;
                }
                None => {
                    return Err(format!(
                        "Can't reach \"{}\" at {}:{} ({first_why}).\nCheck ScanSend is open on the computer and the phone is on the same Wi-Fi (not mobile data or a VPN).{IOS_HINT}",
                        target.name, target.host, target.port
                    ))
                }
            }
        }
    }

    let resp = result.map_err(friendly)?;
    match resp.status() {
        s if s.is_success() => Ok(Sent { to: target.name }),
        reqwest::StatusCode::UNAUTHORIZED => {
            // Unpaired on the computer: drop it here too, so it shows up under Nearby again.
            {
                let mut s = state.saved.lock().unwrap();
                s.receivers.retain(|r| r.id != target.id);
                if s.active.as_deref() == Some(target.id.as_str()) {
                    s.active = s.receivers.first().map(|r| r.id.clone());
                }
                let _ = state.save(&s);
            }
            Err(format!("\"{}\" removed this phone. Tap it under Nearby computers to connect again.", target.name))
        }
        s => Err(format!("Computer replied {s}")),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();
    #[cfg(mobile)]
    let builder = builder.plugin(tauri_plugin_barcode_scanner::init());

    builder
        .setup(|app| {
            let path = app.path().app_data_dir()?.join("receivers.json");
            let mut saved: Saved = std::fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            // First launch (or the old default): give the phone a fun name like "Sleepy Dongle".
            let mut changed = saved.phone_id.is_none();
            if changed {
                let mut b = [0u8; 12];
                rand::rng().fill_bytes(&mut b);
                saved.phone_id = Some(hex::encode(b));
            }
            let fresh = saved.device_name.as_deref().is_none_or(|n| n == "Phone");
            if fresh {
                saved.device_name = Some(random_name(&computer_names(&saved)));
                changed = true;
            }
            let state = AppState { path, saved: Mutex::new(saved) };
            if changed {
                let _ = state.save(&state.saved.lock().unwrap());
            }
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            set_device_name,
            randomize_device_name,
            set_active,
            forget,
            discover,
            pair,
            pair_uri,
            send_scan,
            request_local_network,
            request_connect,
            cancel_connect
        ])
        .run(tauri::generate_context!())
        .expect("error while running ScanSend");
}

#[cfg(test)]
mod tests {
    /// Needs a receiver running on this machine: `cargo test -- --ignored`
    #[tokio::test]
    #[ignore]
    async fn finds_local_receiver() {
        let found = super::discover_inner(std::time::Duration::from_millis(1500)).await.unwrap();
        println!("{found:?}");
        assert!(!found.is_empty(), "no receiver answered");
    }
}
