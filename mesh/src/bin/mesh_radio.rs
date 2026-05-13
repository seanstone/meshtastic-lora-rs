/// Single-node Meshtastic simulator with egui GUI, spectrum & waterfall.
///
/// Boots the shared [`ViewModel`], spawns [`mesh::radio::sim_loop`] in the
/// background, and runs the [`MeshSimApp`] view on the main thread. Compiles
/// to native (tokio) and WASM (gloo-timers / spawn_local).

use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::Ordering;

#[cfg(not(target_arch = "wasm32"))]
use eframe::egui;
use tokio::sync::mpsc;

use mesh::{model::ViewModel, radio::sim_loop, view::MeshSimApp};

// ── WASM entry point ─────────────────────────────────────────────────────────

#[cfg(feature = "wasm")]
mod wasm_entry {
    use super::*;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;
    use web_sys::HtmlCanvasElement;

    #[wasm_bindgen(start)]
    pub async fn start() {
        console_error_panic_hook::set_once();

        let shared = ViewModel::new();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let shared_sim = Arc::clone(&shared);
        wasm_bindgen_futures::spawn_local(sim_loop(shared_sim, cmd_rx));

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
}

// ── Native entry point ───────────────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result<()> {
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
    let shared_sim = Arc::clone(&shared);

    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(sim_loop(shared_sim, cmd_rx));
    });

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Mesh Radio")
            .with_inner_size([960.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Mesh Radio",
        options,
        Box::new(|_cc| Ok(Box::new(MeshSimApp::new(shared, cmd_tx)))),
    )
}
