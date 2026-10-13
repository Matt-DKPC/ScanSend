//! Network side: HTTP server + LAN discovery, on its own tokio runtime so the
//! GUI thread never blocks.

use crate::{discovery, server::{self, AppState}};
use scansend_proto::DISCOVERY_PORT;
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Starting,
    Listening { discovery_ok: bool },
    Failed(String),
}

fn set(state: &AppState, st: Status) {
    *state.status.lock().unwrap() = st;
    (state.on_change)();
}

/// Runs until the process exits. Errors are reported through `state.status`.
pub async fn run(state: Arc<AppState>, port: u16) {
    let listener = match tokio::net::TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], port))).await {
        Ok(l) => l,
        Err(e) => {
            let msg = if e.kind() == std::io::ErrorKind::AddrInUse {
                format!("Port {port} is already in use. ScanSend is probably already running (check the system tray or close it and try again).")
            } else {
                format!("Couldn't open port {port}: {e}")
            };
            set(&state, Status::Failed(msg));
            return;
        }
    };

    let (id, name) = (state.cfg.lock().unwrap().id.clone(), state.name.clone());
    let disco_state = state.clone();
    tokio::spawn(async move {
        if let Err(e) = discovery::run(id, name, port).await {
            eprintln!("  ! auto-discovery is off (UDP {DISCOVERY_PORT}: {e:#}). Pair with the QR code instead.");
            set(&disco_state, Status::Listening { discovery_ok: false });
        }
    });
    set(&state, Status::Listening { discovery_ok: true });

    if let Err(e) = axum::serve(
        listener,
        server::router(state.clone()).into_make_service_with_connect_info::<SocketAddr>(),
    ).await {
        set(&state, Status::Failed(format!("Server stopped: {e}")));
    }
}

/// Start the backend on a background thread (GUI mode).
pub fn spawn(state: Arc<AppState>, port: u16) {
    std::thread::Builder::new()
        .name("network".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(run(state, port));
        })
        .expect("spawn network thread");
}
