/// WebSocket server for external tool integration.
///
/// Runs a WebSocket server on a configurable port.  External tools (Node.js
/// scripts, Home Assistant, dashboards) connect and exchange JSON messages.
///
/// ## Inbound (tool → node)
///
/// ```json
/// { "type": "send_text", "to": 4294967295, "text": "hello" }
/// ```
///
/// `to` is optional (defaults to broadcast `0xFFFFFFFF`).
///
/// ## Outbound (node → tool)
///
/// ```json
/// { "type": "rx", "from": 2864434397, "to": 4294967295,
///   "portnum": 1, "text": "hello", "hops": 2 }
/// { "type": "tx", "text": "hello" }
/// { "type": "node_info", "id": 2864434397,
///   "short_name": "MRST", "long_name": "meshtastic-rs" }
/// ```

#[cfg(feature = "ws")]
pub use server::*;

#[cfg(feature = "ws")]
mod server {
    use futures_util::{SinkExt, StreamExt};
    use serde::{Deserialize, Serialize};
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio::sync::{broadcast, mpsc};
    use tokio_tungstenite::tungstenite::Message;

    // Re-export futures_util from tokio-tungstenite so the binary doesn't
    // need a direct dep.
    use tokio_tungstenite::accept_async;

    /// A command received from a WebSocket client.
    #[derive(Debug, Clone, Deserialize)]
    #[serde(tag = "type")]
    pub enum WsCommand {
        /// Send a text message.
        #[serde(rename = "send_text")]
        SendText {
            /// Destination node ID (default: broadcast).
            #[serde(default = "default_broadcast")]
            to: u32,
            text: String,
        },
    }

    fn default_broadcast() -> u32 { 0xFFFF_FFFF }

    /// An event sent to WebSocket clients.
    #[derive(Debug, Clone, Serialize)]
    #[serde(tag = "type")]
    pub enum WsEvent {
        /// A mesh message was received.
        #[serde(rename = "rx")]
        Rx {
            from: u32,
            to:   u32,
            portnum: i32,
            #[serde(skip_serializing_if = "Option::is_none")]
            text: Option<String>,
            payload_len: usize,
            hops: u8,
        },
        /// A text message was transmitted.
        #[serde(rename = "tx")]
        Tx { text: String },
        /// Node info update.
        #[serde(rename = "node_info")]
        NodeInfo {
            id:         u32,
            short_name: String,
            long_name:  String,
        },
    }

    /// Handle to a running WebSocket server.
    pub struct WsServer {
        /// Incoming commands from connected clients.
        pub commands: mpsc::Receiver<WsCommand>,
        /// Broadcast channel for sending events to all connected clients.
        event_tx: broadcast::Sender<String>,
    }

    impl WsServer {
        /// Broadcast a JSON event to all connected WebSocket clients.
        pub fn broadcast(&self, event: &WsEvent) {
            if let Ok(json) = serde_json::to_string(event) {
                // Ignore send errors (no receivers connected).
                let _ = self.event_tx.send(json);
            }
        }
    }

    /// Spawn a WebSocket server on the given port.
    ///
    /// Returns a [`WsServer`] handle.  Commands from clients arrive on
    /// `commands`; call `broadcast()` to push events to all clients.
    pub async fn spawn_ws_server(port: u16) -> Result<WsServer, String> {
        let addr = format!("0.0.0.0:{port}");
        let listener = TcpListener::bind(&addr).await
            .map_err(|e| format!("ws bind {addr}: {e}"))?;

        let (cmd_tx, cmd_rx) = mpsc::channel::<WsCommand>(64);
        let (event_tx, _) = broadcast::channel::<String>(256);

        let evt_tx = event_tx.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, peer)) = listener.accept().await else { continue };
                eprintln!("[ws] client connected: {peer}");

                let cmd_tx = cmd_tx.clone();
                let mut evt_rx = evt_tx.subscribe();

                tokio::spawn(async move {
                    let Ok(ws) = accept_async(stream).await else {
                        eprintln!("[ws] handshake failed for {peer}");
                        return;
                    };
                    let (mut sink, mut stream) = ws.split();

                    loop {
                        tokio::select! {
                            // Client → node
                            msg = stream.next() => {
                                match msg {
                                    Some(Ok(Message::Text(txt))) => {
                                        match serde_json::from_str::<WsCommand>(&txt) {
                                            Ok(cmd) => { let _ = cmd_tx.send(cmd).await; }
                                            Err(e) => {
                                                let err = format!(
                                                    r#"{{"type":"error","message":"{}"}}"#,
                                                    e.to_string().replace('"', "'")
                                                );
                                                let _ = sink.send(Message::Text(err.into())).await;
                                            }
                                        }
                                    }
                                    Some(Ok(Message::Close(_))) | None => break,
                                    _ => {}
                                }
                            }
                            // Node → client
                            Ok(json) = evt_rx.recv() => {
                                if sink.send(Message::Text(json.into())).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    eprintln!("[ws] client disconnected: {peer}");
                });
            }
        });

        eprintln!("[ws] listening on ws://{addr}");
        Ok(WsServer { commands: cmd_rx, event_tx })
    }
}
