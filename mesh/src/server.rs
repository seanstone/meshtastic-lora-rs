//! axum-based HTTP + WebSocket server hosted by the combined `mesh` binary.
//!
//! Exposes one port that does both jobs:
//!
//! - `GET /`     — placeholder index page (the wasm GUI bundle will land here)
//! - `GET /ws`   — WebSocket upgrade for live state + commands
//!
//! Wire format:
//!
//! - Client → server: a [`Command`] (serde adjacently-tagged JSON, `{"t":..,"c":..}`).
//! - Server → client: a [`ServerMsg`] in the same form. A `Snapshot` is sent on
//!   connect and again every 100 ms; future stages may add deltas.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::{
    Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::{Html, IntoResponse},
    routing::get,
};
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::sync::mpsc::UnboundedSender;

use crate::model::{Command, LogEntry, SimMode, ViewModel};

// ── Shared state held by axum ──────────────────────────────────────────────

#[derive(Clone)]
struct AppState {
    shared: Arc<ViewModel>,
    cmd_tx: UnboundedSender<Command>,
}

// ── Wire types: server → client ─────────────────────────────────────────────

#[derive(Serialize)]
#[serde(tag = "t", content = "c")]
pub enum ServerMsg {
    /// Periodic full snapshot of the radio state.
    Snapshot(Snapshot),
    /// Single log line appended (reserved — not emitted yet; the snapshot's
    /// stats fields and a client-side log mirror are enough for the first cut).
    LogAppend(LogEntry),
    /// A deserialization or protocol error sent back to the offending client.
    Error(String),
}

#[derive(Serialize)]
pub struct Snapshot {
    pub running:        bool,
    pub sf:             u8,
    pub signal_db:      f32,
    pub noise_db:       f32,
    pub interval_ms:    u64,
    pub mode:           SimMode,
    pub node_id_str:    String,
    pub node_short:     String,
    pub node_long:      String,
    pub neighbours:     Vec<String>,
    pub tx_count:       u64,
    pub rx_count:       u64,
    pub use_uhd:        bool,
    pub uhd_args:       String,
    pub uhd_freq_hz:    f64,
    pub uhd_rx_gain_db: f64,
    pub uhd_tx_gain_db: f64,
    pub uhd_loading:    bool,
    pub uhd_warning:    Option<String>,
    pub auto_tx:        bool,
    pub tx_dest:        u32,
}

fn snapshot_of(vm: &ViewModel) -> Snapshot {
    Snapshot {
        running:        vm.running.load(Ordering::Relaxed),
        sf:             *vm.sf.lock().unwrap(),
        signal_db:      *vm.signal_db.lock().unwrap(),
        noise_db:       *vm.noise_db.lock().unwrap(),
        interval_ms:    *vm.interval_ms.lock().unwrap(),
        mode:           *vm.mode.lock().unwrap(),
        node_id_str:    vm.node_id_str.lock().unwrap().clone(),
        node_short:     vm.node_short.lock().unwrap().clone(),
        node_long:      vm.node_long.lock().unwrap().clone(),
        neighbours:     vm.neighbours.lock().unwrap().clone(),
        tx_count:       vm.tx_count.load(Ordering::Relaxed),
        rx_count:       vm.rx_count.load(Ordering::Relaxed),
        use_uhd:        vm.use_uhd.load(Ordering::Relaxed),
        uhd_args:       vm.uhd_args.lock().unwrap().clone(),
        uhd_freq_hz:    *vm.uhd_freq_hz.lock().unwrap(),
        uhd_rx_gain_db: *vm.uhd_rx_gain_db.lock().unwrap(),
        uhd_tx_gain_db: *vm.uhd_tx_gain_db.lock().unwrap(),
        uhd_loading:    vm.uhd_loading.load(Ordering::Relaxed),
        uhd_warning:    vm.uhd_warning.lock().unwrap().clone(),
        auto_tx:        vm.auto_tx.load(Ordering::Relaxed),
        tx_dest:        *vm.tx_dest.lock().unwrap(),
    }
}

// ── Entry point ────────────────────────────────────────────────────────────

/// Bind to `addr` and serve HTTP + WS until the listener errors. Long-running.
pub async fn serve(
    addr: SocketAddr,
    shared: Arc<ViewModel>,
    cmd_tx: UnboundedSender<Command>,
) -> std::io::Result<()> {
    let state = AppState { shared, cmd_tx };
    let app = Router::new()
        .route("/", get(index))
        .route("/ws", get(ws_handler))
        .with_state(state);
    let listener = TcpListener::bind(addr).await?;
    eprintln!("[server] listening on http://{addr}");
    axum::serve(listener, app).await
}

// ── Handlers ───────────────────────────────────────────────────────────────

async fn index() -> Html<&'static str> {
    Html(INDEX_PLACEHOLDER)
}

const INDEX_PLACEHOLDER: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>mesh</title>
<style>body{font-family:monospace;max-width:38em;margin:3em auto;line-height:1.5}</style>
</head><body>
<h1>mesh server</h1>
<p>WebSocket endpoint: <code>/ws</code></p>
<p>Wire format: adjacently-tagged JSON, e.g.
   <code>{"t":"SetSf","c":11}</code> client→server,
   <code>{"t":"Snapshot","c":{...}}</code> server→client.</p>
<p>The web GUI bundle will land here in a future stage.</p>
</body></html>"#;

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    eprintln!("[server] ws client connected");

    // Send an initial snapshot so the client can render immediately.
    if !send_msg(&mut socket, &ServerMsg::Snapshot(snapshot_of(&state.shared))).await {
        return;
    }

    let mut snapshot_interval = tokio::time::interval(Duration::from_millis(100));
    snapshot_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = snapshot_interval.tick() => {
                let msg = ServerMsg::Snapshot(snapshot_of(&state.shared));
                if !send_msg(&mut socket, &msg).await { break; }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(t))) => {
                        match serde_json::from_str::<Command>(t.as_str()) {
                            Ok(cmd) => { let _ = state.cmd_tx.send(cmd); }
                            Err(e) => {
                                if !send_msg(&mut socket, &ServerMsg::Error(e.to_string())).await {
                                    break;
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {} // Ignore Binary / Ping / Pong (axum handles ping/pong itself).
                }
            }
        }
    }
    eprintln!("[server] ws client disconnected");
}

async fn send_msg(socket: &mut WebSocket, msg: &ServerMsg) -> bool {
    let Ok(json) = serde_json::to_string(msg) else { return true };
    socket.send(Message::Text(json.into())).await.is_ok()
}
