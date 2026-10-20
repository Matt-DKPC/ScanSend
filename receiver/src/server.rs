use crate::config::{self, Config, PairedDevice};
use axum::{
    extract::{ConnectInfo, DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use rand::Rng;
use scansend_proto::{
    ApiError, CancelRequest, ConnectRequest, Info, PairRequest, PairResponse, RenameRequest, Scan, CONNECT_WAIT_SECS, MAX_SCAN_LEN,
    PROTOCOL_VERSION,
};
use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;

/// Wrong PIN guesses allowed before the PIN is replaced.
const MAX_PIN_FAILURES: u32 = 5;
/// Connect requests waiting for an answer at once (one per phone address).
const MAX_WAITING: usize = 4;

/// A phone asking to connect, waiting for Accept / Decline on this computer.
pub struct ConnectAsk {
    pub request_id: String,
    pub device_name: String,
    pub from: IpAddr,
    answer: oneshot::Sender<bool>,
}

/// What the window / terminal shows for a waiting request.
#[derive(Debug, Clone)]
pub struct Asking {
    pub request_id: String,
    pub device_name: String,
    pub from: IpAddr,
}

pub struct AppState {
    pub name: String,
    pub cfg: Mutex<Config>,
    pub pin: Mutex<PinState>,
    pub recent_ids: Mutex<VecDeque<String>>,
    pub actions: Mutex<mpsc::Sender<ScanEvent>>,
    pub status: Mutex<crate::backend::Status>,
    /// Newest first, capped at LOG_LEN.
    pub log: Mutex<VecDeque<ScanEvent>>,
    /// Set once any phone has reached us over the network (so the firewall
    /// hint can be hidden).
    pub phone_seen: std::sync::atomic::AtomicBool,
    /// Phones waiting for Accept / Decline, oldest first.
    pub asking: Mutex<Vec<ConnectAsk>>,
    /// Called when a phone asks to connect (terminal mode prints the question,
    /// the window asks for attention).
    pub on_ask: Box<dyn Fn(&Asking) + Send + Sync>,
    /// Called whenever the PIN changes (terminal mode reprints the QR).
    pub on_new_pin: Box<dyn Fn(&str) + Send + Sync>,
    /// Called after any change the GUI should redraw for.
    pub on_change: Box<dyn Fn() + Send + Sync>,
}

pub const LOG_LEN: usize = 200;

#[derive(Debug, Clone)]
pub struct ScanEvent {
    pub time: String,
    pub device: String,
    pub format: String,
    pub text: String,
}

impl AppState {
    pub fn set_new_pin(&self) {
        let pin = new_pin();
        {
            let mut p = self.pin.lock().unwrap();
            p.pin = pin.clone();
            p.failures = 0;
        }
        (self.on_new_pin)(&pin);
        (self.on_change)();
    }

    pub fn current_pin(&self) -> String {
        self.pin.lock().unwrap().pin.clone()
    }

    pub fn asking(&self) -> Vec<Asking> {
        self.asking
            .lock()
            .unwrap()
            .iter()
            .map(|a| Asking { request_id: a.request_id.clone(), device_name: a.device_name.clone(), from: a.from })
            .collect()
    }

    /// Accept or decline a waiting phone. The phone's request then returns.
    pub fn answer(&self, request_id: &str, accept: bool) {
        let ask = {
            let mut asking = self.asking.lock().unwrap();
            asking.iter().position(|a| a.request_id == request_id).map(|i| asking.remove(i))
        };
        if let Some(a) = ask {
            let _ = a.answer.send(accept);
        }
        (self.on_change)();
    }

    fn drop_ask(&self, request_id: &str) {
        let removed = {
            let mut asking = self.asking.lock().unwrap();
            let before = asking.len();
            asking.retain(|a| a.request_id != request_id);
            before != asking.len()
        };
        if removed {
            (self.on_change)();
        }
    }

    /// Remember a new phone; returns the token it must send with each scan.
    /// The same phone reconnecting (same `phone_id`) replaces its old entry.
    fn add_device(&self, device_name: &str, phone_id: Option<String>) -> anyhow::Result<PairResponse> {
        let token = config::random_hex(32);
        let phone_id: Option<String> =
            phone_id.map(|p| p.chars().filter(|c| c.is_ascii_alphanumeric()).take(64).collect()).filter(|p: &String| !p.is_empty());
        let mut cfg = self.cfg.lock().unwrap();
        if phone_id.is_some() {
            cfg.devices.retain(|d| d.phone_id != phone_id);
        }
        cfg.devices.push(PairedDevice {
            name: if device_name.is_empty() { "Phone".into() } else { device_name.into() },
            token_sha256: config::hash_token(&token),
            paired_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            phone_id,
        });
        cfg.save()?;
        Ok(PairResponse { token, receiver_id: cfg.id.clone(), receiver_name: self.name.clone() })
    }

    pub fn unpair(&self, token_sha256: &str) -> anyhow::Result<()> {
        let mut cfg = self.cfg.lock().unwrap();
        cfg.devices.retain(|d| d.token_sha256 != token_sha256);
        cfg.save()?;
        drop(cfg);
        (self.on_change)();
        Ok(())
    }
}

pub struct PinState {
    pub pin: String,
    pub failures: u32,
}

pub fn new_pin() -> String {
    format!("{:06}", rand::rng().random_range(0..1_000_000))
}

fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(ApiError { error: msg.into() })).into_response()
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/v1/info", get(info))
        .route("/api/v1/pair", post(pair))
        .route("/api/v1/connect", post(connect))
        .route("/api/v1/connect/cancel", post(cancel_connect))
        .route("/api/v1/scan", post(scan))
        .route("/api/v1/disconnect", post(disconnect))
        .route("/api/v1/rename", post(rename))
        .route("/api/v1/paired", get(paired))
        .layer(DefaultBodyLimit::max(MAX_SCAN_LEN * 2))
        .with_state(state)
}

fn mark_seen(s: &AppState) {
    if !s.phone_seen.swap(true, std::sync::atomic::Ordering::Relaxed) {
        (s.on_change)();
    }
}

async fn info(State(s): State<Arc<AppState>>) -> Json<Info> {
    mark_seen(&s);
    let id = s.cfg.lock().unwrap().id.clone();
    Json(Info {
        v: PROTOCOL_VERSION,
        id,
        name: s.name.clone(),
        device: Some(crate::net::device_name()),
        os: Some(scansend_proto::os_label().into()),
    })
}

async fn pair(State(s): State<Arc<AppState>>, Json(req): Json<PairRequest>) -> Response {
    mark_seen(&s);
    let device_name: String = req.device_name.chars().filter(|c| !c.is_control()).take(64).collect();
    {
        let mut p = s.pin.lock().unwrap();
        if req.pin.trim() != p.pin {
            p.failures += 1;
            if p.failures >= MAX_PIN_FAILURES {
                p.pin = new_pin();
                p.failures = 0;
                println!("\n  Too many wrong PINs. Pairing code was reset.");
                (s.on_new_pin)(&p.pin);
                (s.on_change)();
            }
            return err(StatusCode::FORBIDDEN, "wrong PIN");
        }
        // One-time use: a PIN (or a photo of the QR) can't pair a second phone.
        p.pin = new_pin();
        p.failures = 0;
    }

    let resp = match s.add_device(&device_name, req.phone_id.clone()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  ! could not save pairing: {e:#}");
            return err(StatusCode::INTERNAL_SERVER_ERROR, "could not save pairing");
        }
    };
    println!("\n  ✓ Paired with \"{device_name}\". Scans from it will be accepted.");
    let pin = s.current_pin();
    (s.on_new_pin)(&pin);
    (s.on_change)();
    Json(resp).into_response()
}

/// Removes a request from the waiting list however the handler ends,
/// including when the phone gives up and the connection drops.
struct AskGuard {
    state: Arc<AppState>,
    request_id: String,
}

impl Drop for AskGuard {
    fn drop(&mut self) {
        self.state.drop_ask(&self.request_id);
    }
}

/// LocalSend-style pairing: the phone taps this computer's name, this computer
/// asks "Let 📱 X connect?", and the request stays open until someone answers.
async fn connect(
    State(s): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<ConnectRequest>,
) -> Response {
    mark_seen(&s);
    let device_name: String = req.device_name.chars().filter(|c| !c.is_control()).take(64).collect();
    let request_id: String = req.request_id.chars().filter(|c| c.is_ascii_alphanumeric()).take(64).collect();
    if request_id.is_empty() {
        return err(StatusCode::BAD_REQUEST, "missing request_id");
    }
    let (tx, rx) = oneshot::channel();
    let ask = Asking { request_id: request_id.clone(), device_name: device_name.clone(), from: addr.ip() };
    {
        let mut asking = s.asking.lock().unwrap();
        // One question per phone address at a time; a new tap replaces the old one.
        asking.retain(|a| a.from != addr.ip());
        if asking.len() >= MAX_WAITING {
            return err(StatusCode::TOO_MANY_REQUESTS, "too many phones are waiting; try again in a minute");
        }
        asking.push(ConnectAsk { request_id: request_id.clone(), device_name: device_name.clone(), from: addr.ip(), answer: tx });
    }
    let _guard = AskGuard { state: s.clone(), request_id };
    (s.on_ask)(&ask);
    (s.on_change)();

    match tokio::time::timeout(Duration::from_secs(CONNECT_WAIT_SECS), rx).await {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => {
            println!("  ✗ Declined \"{device_name}\".");
            return err(StatusCode::FORBIDDEN, "declined");
        }
        // Timed out, or the request was replaced / cancelled.
        _ => return err(StatusCode::REQUEST_TIMEOUT, "nobody answered"),
    }
    match s.add_device(&device_name, req.phone_id.clone()) {
        Ok(resp) => {
            println!("\n  ✓ Connected to \"{device_name}\". Scans from it will be accepted.");
            (s.on_change)();
            Json(resp).into_response()
        }
        Err(e) => {
            eprintln!("  ! could not save pairing: {e:#}");
            err(StatusCode::INTERNAL_SERVER_ERROR, "could not save pairing")
        }
    }
}

/// The phone tapped Cancel while waiting.
async fn cancel_connect(
    State(s): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<CancelRequest>,
) -> StatusCode {
    let mine = s.asking.lock().unwrap().iter().any(|a| a.request_id == req.request_id && a.from == addr.ip());
    if mine {
        s.drop_ask(&req.request_id); // drops the sender, so the waiting request returns 408
    }
    StatusCode::NO_CONTENT
}

fn bearer(headers: &HeaderMap) -> &str {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("")
}

/// The phone tapped Forget: drop it here too, so it doesn't linger in "Paired phones".
async fn disconnect(State(s): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let hash = config::hash_token(bearer(&headers));
    let Some(name) = s.cfg.lock().unwrap().devices.iter().find(|d| d.token_sha256 == hash).map(|d| d.name.clone()) else {
        return err(StatusCode::UNAUTHORIZED, "not paired");
    };
    if let Err(e) = s.unpair(&hash) {
        eprintln!("  ! could not save: {e:#}");
        return err(StatusCode::INTERNAL_SERVER_ERROR, "could not save");
    }
    println!("  \"{name}\" disconnected.");
    StatusCode::NO_CONTENT.into_response()
}

/// Give the phone with this token a new name. Returns false if the token is unknown.
fn set_device_name(s: &AppState, token: &str, name: &str) -> bool {
    let name: String = name.chars().filter(|c| !c.is_control()).take(64).collect();
    let name = name.trim();
    let hash = config::hash_token(token);
    let mut cfg = s.cfg.lock().unwrap();
    let Some(d) = cfg.devices.iter_mut().find(|d| d.token_sha256 == hash) else { return false };
    if name.is_empty() || d.name == name {
        return true;
    }
    println!("  \"{}\" is now called \"{name}\".", d.name);
    d.name = name.to_string();
    if let Err(e) = cfg.save() {
        eprintln!("  ! could not save: {e:#}");
    }
    drop(cfg);
    (s.on_change)();
    true
}

/// Phones ask this while on their main screen, so an Unpair here shows up there within seconds.
async fn paired(State(s): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if s.cfg.lock().unwrap().device_for_token(bearer(&headers)).is_some() {
        StatusCode::NO_CONTENT.into_response()
    } else {
        err(StatusCode::UNAUTHORIZED, "not paired")
    }
}

/// The phone changed its name in its settings.
async fn rename(State(s): State<Arc<AppState>>, headers: HeaderMap, Json(req): Json<RenameRequest>) -> Response {
    if set_device_name(&s, bearer(&headers), &req.device_name) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        err(StatusCode::UNAUTHORIZED, "not paired")
    }
}

async fn scan(State(s): State<Arc<AppState>>, headers: HeaderMap, Json(scan): Json<Scan>) -> Response {
    mark_seen(&s);
    let token = bearer(&headers);
    // Catch up on a rename the computer missed (it was off, or on another network).
    if let Some(name) = scan.device_name.as_deref() {
        set_device_name(&s, token, name);
    }
    let Some(device) = s.cfg.lock().unwrap().device_for_token(token).map(|d| d.name.clone()) else {
        return err(StatusCode::UNAUTHORIZED, "not paired");
    };
    if scan.text.is_empty() || scan.text.len() > MAX_SCAN_LEN {
        return err(StatusCode::PAYLOAD_TOO_LARGE, "scan empty or too long");
    }

    if let Some(id) = scan.id.as_ref() {
        let mut recent = s.recent_ids.lock().unwrap();
        if recent.contains(id) {
            return StatusCode::NO_CONTENT.into_response(); // retry of one we already handled
        }
        recent.push_back(id.clone());
        if recent.len() > 64 {
            recent.pop_front();
        }
    }

    let fmt = scan.format.clone().unwrap_or_else(|| "?".into());
    println!("  [{device}] {fmt}: {}", scan.text.escape_debug());
    let ev = ScanEvent {
        time: chrono::Local::now().format("%H:%M:%S").to_string(),
        device,
        format: fmt,
        text: scan.text,
    };
    {
        let mut log = s.log.lock().unwrap();
        log.push_front(ev.clone());
        log.truncate(LOG_LEN);
    }
    if s.actions.lock().unwrap().send(ev).is_err() {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "action thread stopped");
    }
    (s.on_change)();
    StatusCode::NO_CONTENT.into_response()
}
