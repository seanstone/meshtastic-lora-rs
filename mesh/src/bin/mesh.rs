//! `mesh` — the combined binary.
//!
//! Always runs: the radio loop + HTTP/WS server. The desktop egui window is
//! optional — compiled in via the `desktop` cargo feature and skipped at
//! runtime if `--headless` is set or no X11/Wayland display is reachable.
//!
//! ```text
//! mesh [--bind ADDR]   # default 0.0.0.0:9069
//!      [--headless]    # skip the egui window even if desktop feature is on
//! ```

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::sync::mpsc;

use mesh::{model::ViewModel, radio::sim_loop, server};

struct Args {
    bind:     SocketAddr,
    /// Read only when the `desktop` feature is on; pure-server builds parse
    /// the flag for CLI consistency but ignore it.
    #[allow(dead_code)]
    headless: bool,
}

fn main() -> std::io::Result<()> {
    let args = parse_args();

    let shared = ViewModel::new();

    // Auto-detect USRP at startup.
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

    // Radio runs on its own dedicated thread with a single-threaded current-
    // thread tokio runtime. This isolates the heavy synchronous work in each
    // radio tick (FFT, decode, UHD send) from the server's tokio workers, so
    // a long decode or a blocked UHD push can't starve the snapshot loop and
    // freeze the web GUI.
    let _radio = std::thread::Builder::new()
        .name("mesh-radio".into())
        .spawn({
            let shared = Arc::clone(&shared);
            move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                    .expect("radio runtime");
                rt.block_on(sim_loop(shared, cmd_rx));
            }
        })
        .expect("spawn radio thread");

    // Server runs on a separate multi-thread tokio runtime — the snapshot
    // loop and any future per-client tasks share workers among themselves
    // but never with the radio.
    let bg = std::thread::Builder::new()
        .name("mesh-server".into())
        .spawn({
            let shared = Arc::clone(&shared);
            let cmd_tx = cmd_tx.clone();
            move || {
                let rt = tokio::runtime::Runtime::new().expect("server runtime");
                rt.block_on(async move {
                    let serve_fut = server::serve(args.bind, shared, cmd_tx);
                    tokio::select! {
                        result = serve_fut       => result,
                        _      = tokio::signal::ctrl_c() => Ok(()),
                    }
                })
            }
        })
        .expect("spawn server thread");

    #[cfg(feature = "desktop")]
    if !args.headless && has_display() {
        return run_desktop_gui(shared, cmd_tx);
    }
    #[cfg(feature = "desktop")]
    if !args.headless {
        eprintln!("[mesh] no display detected (DISPLAY / WAYLAND_DISPLAY unset) — running headless");
    }
    #[cfg(not(feature = "desktop"))]
    let _ = (&shared, &cmd_tx); // suppress unused-variable warnings

    // Headless: wait for the runtime to exit (Ctrl+C reaches it via signal::ctrl_c).
    bg.join().expect("runtime thread panicked")
}

fn parse_args() -> Args {
    let mut bind: SocketAddr = "0.0.0.0:9069".parse().expect("default addr");
    let mut headless = false;
    let argv: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "--bind" => {
                i += 1;
                bind = argv.get(i).expect("--bind needs an address")
                    .parse().expect("invalid --bind address");
            }
            "--headless" => headless = true,
            "--help" | "-h" => {
                eprintln!(
                    "Usage: mesh [--bind ADDR] [--headless]\n  \
                     default: --bind 0.0.0.0:9069"
                );
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    Args { bind, headless }
}

#[cfg(all(feature = "desktop", target_os = "linux"))]
fn has_display() -> bool {
    std::env::var("DISPLAY").is_ok()
        || std::env::var("WAYLAND_DISPLAY").is_ok()
        || std::env::var("WAYLAND_SOCKET").is_ok()
}
#[cfg(all(feature = "desktop", not(target_os = "linux")))]
fn has_display() -> bool { true }

#[cfg(feature = "desktop")]
fn run_desktop_gui(
    shared: Arc<ViewModel>,
    cmd_tx: tokio::sync::mpsc::UnboundedSender<mesh::model::Command>,
) -> std::io::Result<()> {
    use eframe::egui;
    use mesh::view::MeshSimApp;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("mesh")
            .with_inner_size([960.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native(
        "mesh",
        options,
        Box::new(move |_cc| Ok(Box::new(MeshSimApp::new(shared, cmd_tx)))),
    )
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
}
