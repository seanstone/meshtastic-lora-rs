//! `mesh` — combined headless server.
//!
//! Boots the shared [`ViewModel`], runs the radio loop in the background, and
//! exposes HTTP + WebSocket on a single port for the web GUI (or any other
//! tool) to drive. The desktop egui window is added in a later stage behind
//! a compile-time feature.
//!
//! CLI:
//!
//! ```text
//! mesh [--bind ADDR]      # default 0.0.0.0:3000
//! ```

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::sync::mpsc;

use mesh::{model::ViewModel, radio::sim_loop, server};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let bind = parse_args();

    let shared = ViewModel::new();

    // Auto-detect USRP at startup, same as mesh_radio does.
    #[cfg(feature = "uhd")]
    {
        eprint!("[uhd] probing for USRP… ");
        if lora::uhd::probe() {
            eprintln!("found — switching to UHD mode");
            shared.use_uhd.store(true, Ordering::Relaxed);
            shared.uhd_loading.store(true, Ordering::Relaxed);
            shared.rebuild_driver.store(true, Ordering::Relaxed);
        } else {
            eprintln!("none found — using simulator");
        }
    }

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let shared_for_radio = Arc::clone(&shared);
    tokio::spawn(sim_loop(shared_for_radio, cmd_rx));

    tokio::select! {
        result = server::serve(bind, shared, cmd_tx) => result,
        _ = tokio::signal::ctrl_c() => {
            eprintln!("[mesh] shutting down");
            Ok(())
        }
    }
}

fn parse_args() -> SocketAddr {
    let args: Vec<String> = std::env::args().collect();
    let mut bind: SocketAddr = "0.0.0.0:3000".parse().expect("default addr");
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--bind" => {
                i += 1;
                bind = args.get(i).expect("--bind needs an address")
                    .parse().expect("invalid --bind address");
            }
            "--help" | "-h" => {
                eprintln!("Usage: mesh [--bind ADDR]\n  default: --bind 0.0.0.0:3000");
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    bind
}
