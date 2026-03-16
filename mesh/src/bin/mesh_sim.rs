/// Two-node Meshtastic simulation with egui GUI.
///
/// Node A and Node B share an in-memory AWGN channel.  Node A sends text
/// messages and NodeInfo beacons; Node B relays back its own NodeInfo.
/// Both nodes update their neighbour tables from received beacons.
///
/// Run with:
///   cargo run --bin mesh_sim

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
    thread,
};

use eframe::egui::{self, Color32, RichText, ScrollArea};
use lora::channel::Channel;
use lora::modem::{Tx, Rx};

use mesh::{
    app::{ChannelConfig, MeshNode},
    mac::packet::BROADCAST,
    presets::{PRESETS, ModemPreset},
};

// ── Shared state ──────────────────────────────────────────────────────────────

#[derive(Clone)]
struct LogEntry {
    ok:   bool,
    text: String,
}

struct SimShared {
    running:     AtomicBool,
    /// Spreading factor (7–12).
    sf:          Mutex<u8>,
    /// Signal amplitude in dB (TX level through channel).
    signal_db:   Mutex<f32>,
    /// Noise sigma in dB.
    noise_db:    Mutex<f32>,
    /// TX interval in milliseconds.
    interval_ms: Mutex<u64>,
    /// Message log (newest last).
    log:         Mutex<VecDeque<LogEntry>>,
    /// Snapshot of Node A's neighbour table.
    a_neighbours: Mutex<Vec<String>>,
    /// Snapshot of Node B's neighbour table.
    b_neighbours: Mutex<Vec<String>>,
    /// Human-readable IDs for the two nodes.
    a_id: Mutex<String>,
    b_id: Mutex<String>,
    /// Packets sent / received counters.
    tx_count: Mutex<usize>,
    rx_count: Mutex<usize>,
}

impl SimShared {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            running:     AtomicBool::new(true),
            sf:          Mutex::new(7),
            signal_db:   Mutex::new(-20.0),
            noise_db:    Mutex::new(-60.0),
            interval_ms: Mutex::new(1500),
            log:         Mutex::new(VecDeque::new()),
            a_neighbours: Mutex::new(vec![]),
            b_neighbours: Mutex::new(vec![]),
            a_id: Mutex::new(String::new()),
            b_id: Mutex::new(String::new()),
            tx_count: Mutex::new(0),
            rx_count: Mutex::new(0),
        })
    }
}

const MAX_LOG: usize = 200;
const OS_FACTOR: u32 = 4;   // SR = 4 × BW (250 kHz × 4 = 1 MHz sample rate)
const CR: u8 = 4;            // CR 4/8 — matches gui_sim default
const SYNC_WORD: u8 = 0x2B; // Meshtastic
const PREAMBLE: u16 = 16;   // Meshtastic default

fn db_to_amp(db: f32) -> f32 { 10_f32.powf(db / 20.0) }

// ── Simulation thread ─────────────────────────────────────────────────────────

fn sim_thread(shared: Arc<SimShared>) {
    let channel_cfg = ChannelConfig::default();

    let mut node_a = MeshNode::with_identity(channel_cfg.clone(), "MSIM", "Mesh-Sim Node A");
    let mut node_b = MeshNode::with_identity(channel_cfg.clone(), "MRCV", "Mesh-Sim Node B");

    // Publish node IDs to the GUI.
    *shared.a_id.lock().unwrap() = format!("!{:08x}", node_a.node_id());
    *shared.b_id.lock().unwrap() = format!("!{:08x}", node_b.node_id());

    let mut seq: u32 = 0;
    let mut last_tx = Instant::now();
    let mut last_beacon = Instant::now();

    loop {
        if !shared.running.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(50));
            continue;
        }

        let sf          = *shared.sf.lock().unwrap();
        let signal_amp  = db_to_amp(*shared.signal_db.lock().unwrap());
        let noise_sigma = db_to_amp(*shared.noise_db.lock().unwrap()) / std::f32::consts::SQRT_2;
        let interval_ms = *shared.interval_ms.lock().unwrap();

        let tx = Tx::new(sf, CR, OS_FACTOR, SYNC_WORD, PREAMBLE);
        let rx = Rx::new(sf, CR, OS_FACTOR, SYNC_WORD, PREAMBLE);

        // ── NodeInfo beacon every 5 s ────────────────────────────────────────
        if last_beacon.elapsed() >= Duration::from_secs(5) {
            last_beacon = Instant::now();

            // Node A → Node B beacon.
            if let Some(frame) = node_a.build_nodeinfo_frame() {
                phy_transfer(&tx, &rx, frame.to_bytes(), signal_amp, noise_sigma,
                             &mut node_b, "NodeInfo A→B", &shared);
            }
            // Node B → Node A beacon.
            if let Some(frame) = node_b.build_nodeinfo_frame() {
                phy_transfer(&tx, &rx, frame.to_bytes(), signal_amp, noise_sigma,
                             &mut node_a, "NodeInfo B→A", &shared);
            }

            // Refresh neighbour snapshots.
            *shared.a_neighbours.lock().unwrap() = node_a.neighbours()
                .iter().map(|n| format!("{} ({})", n.short_name, format!("!{:08x}", n.node_id))).collect();
            *shared.b_neighbours.lock().unwrap() = node_b.neighbours()
                .iter().map(|n| format!("{} ({})", n.short_name, format!("!{:08x}", n.node_id))).collect();
        }

        // ── Text message every interval_ms ───────────────────────────────────
        if last_tx.elapsed() >= Duration::from_millis(interval_ms) {
            last_tx = Instant::now();
            seq += 1;
            let text = format!("Hello from A #{seq}");

            if let Some(frame) = node_a.build_text_frame(BROADCAST, &text) {
                *shared.tx_count.lock().unwrap() += 1;
                push_log(&shared, true, format!("TX A→* \"{text}\""));

                phy_transfer(&tx, &rx, frame.to_bytes(), signal_amp, noise_sigma,
                             &mut node_b, &text, &shared);
            }
        }

        thread::sleep(Duration::from_millis(10));
    }
}

/// Modulate `raw` through the AWGN channel, attempt decode on `dest` node.
fn phy_transfer(
    tx:         &Tx,
    rx:         &Rx,
    raw:        Vec<u8>,
    signal_amp: f32,
    noise_sigma: f32,
    dest:       &mut MeshNode,
    _label:     &str,
    shared:     &Arc<SimShared>,
) {
    let iq_clean = tx.modulate(&raw);
    let n = iq_clean.len();

    let mut channel = Channel::new(noise_sigma, signal_amp);
    channel.push_samples(iq_clean);
    let iq_mixed = channel.tick(n);

    if let Some(decoded) = rx.decode(&iq_mixed) {
        match dest.process_rx_frame(&decoded) {
            Ok((Some(msg), _)) => {
                *shared.rx_count.lock().unwrap() += 1;
                let label = if let Some(t) = msg.data.text() {
                    format!("RX *→{:08x} \"{}\" (hops left: {})",
                        dest.node_id(), t, msg.hop_limit)
                } else {
                    format!("RX portnum={} to={:08x}", msg.data.portnum, dest.node_id())
                };
                push_log(shared, true, label);
            }
            Ok((None, _)) => {}
            Err(e) => push_log(shared, false, format!("ERR: {e}")),
        }
    } else {
        push_log(shared, false, "PHY decode failed".into());
    }
}

fn push_log(shared: &Arc<SimShared>, ok: bool, text: String) {
    let mut log = shared.log.lock().unwrap();
    log.push_back(LogEntry { ok, text });
    if log.len() > MAX_LOG { log.pop_front(); }
}

// ── GUI ───────────────────────────────────────────────────────────────────────

struct MeshSimApp {
    shared:   Arc<SimShared>,
    sf:       u8,
    signal_db: f32,
    noise_db:  f32,
    interval_ms: u64,
    preset_idx:  usize,
}

impl MeshSimApp {
    fn new(shared: Arc<SimShared>) -> Self {
        Self {
            shared,
            sf: 7,
            signal_db: -20.0,
            noise_db: -60.0,
            interval_ms: 1500,
            preset_idx: 0,
        }
    }

    fn apply_preset(&mut self, p: &ModemPreset) {
        self.sf = p.sf;
        *self.shared.sf.lock().unwrap() = p.sf;
    }

    fn snr_db(&self) -> f32 { self.signal_db - self.noise_db }
}

impl eframe::App for MeshSimApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(200));

        // ── Left settings panel ──────────────────────────────────────────────
        egui::SidePanel::left("settings").min_width(200.0).show(ctx, |ui| {
            ui.heading("Settings");
            ui.separator();

            // Preset selector.
            ui.label("Preset");
            egui::ComboBox::from_id_salt("preset")
                .selected_text(PRESETS[self.preset_idx].name)
                .show_ui(ui, |ui| {
                    for (i, p) in PRESETS.iter().enumerate() {
                        if ui.selectable_label(self.preset_idx == i, p.name).clicked() {
                            self.preset_idx = i;
                            self.apply_preset(p);
                        }
                    }
                });

            ui.add_space(6.0);

            // SF (independent of preset for quick tuning).
            ui.label(format!("SF  {}", self.sf));
            if ui.add(egui::Slider::new(&mut self.sf, 7_u8..=12).show_value(false)).changed() {
                *self.shared.sf.lock().unwrap() = self.sf;
            }

            ui.add_space(6.0);
            ui.label(format!("Signal  {:.0} dB", self.signal_db));
            if ui.add(egui::Slider::new(&mut self.signal_db, -60.0_f32..=0.0).show_value(false)).changed() {
                *self.shared.signal_db.lock().unwrap() = self.signal_db;
            }

            ui.label(format!("Noise   {:.0} dB", self.noise_db));
            if ui.add(egui::Slider::new(&mut self.noise_db, -80.0_f32..=-20.0).show_value(false)).changed() {
                *self.shared.noise_db.lock().unwrap() = self.noise_db;
            }

            ui.label(RichText::new(format!("SNR  {:.0} dB", self.snr_db()))
                .color(if self.snr_db() > 0.0 { Color32::GREEN } else { Color32::YELLOW }));

            ui.add_space(6.0);
            ui.label(format!("Interval  {} ms", self.interval_ms));
            if ui.add(egui::Slider::new(&mut self.interval_ms, 200_u64..=10000).show_value(false)).changed() {
                *self.shared.interval_ms.lock().unwrap() = self.interval_ms;
            }

            ui.add_space(8.0);
            let running = self.shared.running.load(Ordering::Relaxed);
            if ui.button(if running { "⏸ Pause" } else { "▶ Resume" }).clicked() {
                self.shared.running.store(!running, Ordering::Relaxed);
            }

            ui.separator();

            // Counters.
            let tx = *self.shared.tx_count.lock().unwrap();
            let rx = *self.shared.rx_count.lock().unwrap();
            ui.label(format!("TX  {tx}"));
            ui.label(format!("RX  {rx}"));
            if tx > 0 {
                ui.label(format!("PER  {:.1}%", (tx - rx.min(tx)) as f32 / tx as f32 * 100.0));
            }

            ui.separator();

            // Node info.
            ui.heading("Nodes");
            let a_id = self.shared.a_id.lock().unwrap().clone();
            let b_id = self.shared.b_id.lock().unwrap().clone();
            ui.label(format!("A  {a_id}"));
            ui.label(format!("B  {b_id}"));

            ui.add_space(4.0);
            ui.label("A neighbours:");
            for n in self.shared.a_neighbours.lock().unwrap().iter() {
                ui.label(format!("  {n}"));
            }
            ui.label("B neighbours:");
            for n in self.shared.b_neighbours.lock().unwrap().iter() {
                ui.label(format!("  {n}"));
            }
        });

        // ── Central message log ──────────────────────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Mesh Message Log");
            ui.separator();

            ScrollArea::vertical()
                .auto_shrink([false; 2])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    let log = self.shared.log.lock().unwrap();
                    for entry in log.iter() {
                        let color = if entry.ok { Color32::from_rgb(100, 220, 100) }
                                    else        { Color32::from_rgb(220, 100, 100) };
                        ui.label(RichText::new(&entry.text).color(color).monospace());
                    }
                });
        });
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() -> eframe::Result<()> {
    let shared = SimShared::new();
    let shared_sim = Arc::clone(&shared);
    thread::spawn(move || sim_thread(shared_sim));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Mesh Sim")
            .with_inner_size([800.0, 540.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Mesh Sim",
        options,
        Box::new(|_cc| Ok(Box::new(MeshSimApp::new(shared)))),
    )
}
