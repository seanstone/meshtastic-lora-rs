//! `mesh_web` — wasm GUI that talks to a `mesh` server over WebSocket.
//!
//! The page imports `mesh_web.js`; this module's `start()` opens
//! `ws[s]://<host>/ws`, mirrors incoming [`Snapshot`]s into a local
//! [`ViewModel`], and forwards each [`Command`] from the egui view back over
//! the wire. Rendering is the same [`MeshSimApp`] the desktop binary uses —
//! the only difference is where the state comes from.

#[cfg(target_arch = "wasm32")]
mod entry {
    use std::sync::Arc;

    use futures_util::{SinkExt, StreamExt};
    use gloo_net::websocket::{Message as WsMessage, futures::WebSocket};
    use tokio::sync::mpsc;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::spawn_local;
    use web_sys::HtmlCanvasElement;

    use mesh::{
        model::{Command, ViewModel},
        proto_ws::ServerMsg,
        view::MeshSimApp,
    };

    #[wasm_bindgen(start)]
    pub async fn start() {
        console_error_panic_hook::set_once();

        let shared = ViewModel::new();
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<Command>();

        let ws_url = build_ws_url();
        web_sys::console::log_1(&format!("[mesh_web] connecting to {ws_url}").into());

        match WebSocket::open(&ws_url) {
            Ok(ws) => {
                let (mut writer, mut reader) = ws.split();

                // Reader: deserialize ServerMsg::Snapshot, mirror into local
                // ViewModel. Other variants are ignored for now.
                let shared_for_reader = Arc::clone(&shared);
                spawn_local(async move {
                    while let Some(msg) = reader.next().await {
                        let Ok(WsMessage::Text(t)) = msg else { continue };
                        match serde_json::from_str::<ServerMsg>(&t) {
                            Ok(ServerMsg::Snapshot(snap)) => snap.apply(&shared_for_reader),
                            Ok(ServerMsg::LogAppend(_)) => {} // reserved
                            Ok(ServerMsg::Error(e)) => {
                                web_sys::console::warn_1(&format!("[mesh_web] server error: {e}").into());
                            }
                            Err(e) => {
                                web_sys::console::warn_1(&format!("[mesh_web] bad ServerMsg: {e}").into());
                            }
                        }
                    }
                    web_sys::console::warn_1(&"[mesh_web] WS reader closed".into());
                });

                // Writer: each Command from the view becomes a JSON text frame.
                spawn_local(async move {
                    while let Some(cmd) = cmd_rx.recv().await {
                        let Ok(json) = serde_json::to_string(&cmd) else { continue };
                        if writer.send(WsMessage::Text(json)).await.is_err() {
                            web_sys::console::warn_1(&"[mesh_web] WS writer closed".into());
                            break;
                        }
                    }
                });
            }
            Err(e) => {
                web_sys::console::error_1(&format!("[mesh_web] WS open failed: {e:?}").into());
            }
        }

        // Boot the egui canvas. The view holds `cmd_tx` and reads from
        // `shared`; the spawn_local tasks above keep both in sync with the
        // server.
        let canvas = web_sys::window().unwrap()
            .document().unwrap()
            .get_element_by_id("canvas").unwrap()
            .unchecked_into::<HtmlCanvasElement>();

        let _ = eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(move |_cc| Ok(Box::new(MeshSimApp::new(shared, cmd_tx)))),
            )
            .await;
    }

    fn build_ws_url() -> String {
        let location = web_sys::window().unwrap().location();
        let host = location.host().unwrap_or_else(|_| "localhost:3000".into());
        let proto = if location.protocol().unwrap_or_default() == "https:" {
            "wss"
        } else {
            "ws"
        };
        format!("{proto}://{host}/ws")
    }
}

fn main() {}
