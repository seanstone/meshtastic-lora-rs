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
use tokio::net::TcpListener;
use tokio::sync::mpsc::UnboundedSender;
use tower_http::services::ServeDir;

use crate::model::{Command, ViewModel};
use crate::proto_ws::{ServerMsg, Snapshot};

// ── Shared state held by axum ──────────────────────────────────────────────

#[derive(Clone)]
struct AppState {
    shared: Arc<ViewModel>,
    cmd_tx: UnboundedSender<Command>,
}

// ── Entry point ────────────────────────────────────────────────────────────

/// Bind to `addr` and serve HTTP + WS until the listener errors. Long-running.
///
/// Static files: if `./dist` exists at startup (i.e. you ran `make wasm-web`
/// first), it's served as the fallback — `GET /` hits `dist/index.html`,
/// `GET /mesh_web_bg.wasm` hits the wasm payload, etc. Otherwise `/` returns
/// a placeholder page explaining the wire protocol.
pub async fn serve(
    addr: SocketAddr,
    shared: Arc<ViewModel>,
    cmd_tx: UnboundedSender<Command>,
) -> std::io::Result<()> {
    let state = AppState { shared, cmd_tx };
    let dist = std::path::Path::new("./dist");
    let mut app = Router::new().route("/ws", get(ws_handler));
    if dist.is_dir() {
        eprintln!("[server] serving static assets from ./dist");
        app = app.fallback_service(ServeDir::new(dist));
    } else {
        app = app.route("/", get(index_placeholder));
    }
    let app = app.with_state(state);
    let listener = TcpListener::bind(addr).await?;
    eprintln!("[server] listening on http://{addr}");
    axum::serve(listener, app).await
}

// ── Handlers ───────────────────────────────────────────────────────────────

async fn index_placeholder() -> Html<&'static str> {
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
    if !send_msg(&mut socket, &ServerMsg::Snapshot(Snapshot::from_view(&state.shared))).await {
        return;
    }

    let mut snapshot_interval = tokio::time::interval(Duration::from_millis(100));
    snapshot_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = snapshot_interval.tick() => {
                let msg = ServerMsg::Snapshot(Snapshot::from_view(&state.shared));
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
