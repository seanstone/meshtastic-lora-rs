/// Two-node Meshtastic simulation with egui GUI, spectrum & waterfall.
///
/// Node A and Node B share an in-memory AWGN channel.  The sim runs a
/// continuous tick-based loop so the spectrum/waterfall show the noise floor
/// between packets and the LoRa chirps during transmission.
///
/// Run with:
///   cargo run --bin mesh_sim

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant},
    thread,
};

use eframe::egui::{self, Color32, RichText, ScrollArea};
use rustfft::num_complex::Complex;
use lora::channel::Channel;
use lora::modem::{Tx, Rx};
use lora::ui::{Chart, SpectrumPlot, WaterfallPlot, SpectrumAnalyzer};

use mesh::{
    app::{ChannelConfig, MeshNode},
    mac::packet::BROADCAST,
    presets::{PRESETS, ModemPreset},
};

// ── Constants ────────────────────────────────────────────────────────────────

const MAX_LOG: usize = 200;
const OS_FACTOR: u32 = 4;
const CR: u8 = 4;
const SYNC_WORD: u8 = 0x2B;
const PREAMBLE: u16 = 16;
const FFT_SIZE: usize = 2048;
/// Sim sample rate = BW × OS = 250 kHz × 4 = 1 MHz.
const SR_HZ: u64 = 1_000_000;
/// Sim tick period (~60 fps).
const TICK: Duration = Duration::from_millis(16);
/// NodeInfo beacon interval in samples.
const BEACON_INTERVAL: u64 = 5 * SR_HZ;

// ── Shared state ─────────────────────────────────────────────────────────────

#[derive(Clone)]
struct LogEntry {
    ok:   bool,
    text: String,
}

struct SimShared {
    running:     AtomicBool,
    sf:          Mutex<u8>,
    signal_db:   Mutex<f32>,
    noise_db:    Mutex<f32>,
    interval_ms: Mutex<u64>,
    log:         Mutex<VecDeque<LogEntry>>,
    a_neighbours: Mutex<Vec<String>>,
    b_neighbours: Mutex<Vec<String>>,
    a_id:        Mutex<String>,
    b_id:        Mutex<String>,
    tx_count:    AtomicU64,
    rx_count:    AtomicU64,

    // Visualization.
    spectrum_plot:  Arc<SpectrumPlot>,
    waterfall_plot: Arc<WaterfallPlot>,
}

impl SimShared {
    fn new() -> Arc<Self> {
        let init_spec: Vec<[f64; 2]> = (0..FFT_SIZE).map(|i| [i as f64, -80.0]).collect();
        let spectrum_plot  = SpectrumPlot::new("Spectrum",   init_spec.clone(), -80.0, 80.0);
        let waterfall_plot = WaterfallPlot::new("Waterfall", init_spec,         -80.0);
        waterfall_plot.set_freq(FFT_SIZE as f64 / 2.0);
        waterfall_plot.set_bw(FFT_SIZE as f64);

        Arc::new(Self {
            running:      AtomicBool::new(true),
            sf:           Mutex::new(11),
            signal_db:    Mutex::new(-20.0),
            noise_db:     Mutex::new(-60.0),
            interval_ms:  Mutex::new(2000),
            log:          Mutex::new(VecDeque::new()),
            a_neighbours: Mutex::new(vec![]),
            b_neighbours: Mutex::new(vec![]),
            a_id:         Mutex::new(String::new()),
            b_id:         Mutex::new(String::new()),
            tx_count:     AtomicU64::new(0),
            rx_count:     AtomicU64::new(0),
            spectrum_plot,
            waterfall_plot,
        })
    }
}

fn db_to_amp(db: f32) -> f32 { 10_f32.powf(db / 20.0) }

fn push_log(shared: &SimShared, ok: bool, text: String) {
    let mut log = shared.log.lock().unwrap();
    log.push_back(LogEntry { ok, text });
    if log.len() > MAX_LOG { log.pop_front(); }
}

// ── Simulation thread ────────────────────────────────────────────────────────

fn sim_thread(shared: Arc<SimShared>) {
    let channel_cfg = ChannelConfig::default();

    let node_a = MeshNode::with_identity(channel_cfg.clone(), "MSIM", "Mesh-Sim Node A");
    let mut node_b = MeshNode::with_identity(channel_cfg.clone(), "MRCV", "Mesh-Sim Node B");

    *shared.a_id.lock().unwrap() = format!("!{:08x}", node_a.node_id());
    *shared.b_id.lock().unwrap() = format!("!{:08x}", node_b.node_id());

    let mut channel = Channel::new(0.001, 0.1);
    let mut analyzer = SpectrumAnalyzer::new(FFT_SIZE);

    let mut tx_modem = Tx::new(11, CR, OS_FACTOR, SYNC_WORD, PREAMBLE);
    let mut rx_modem = Rx::new(11, CR, OS_FACTOR, SYNC_WORD, PREAMBLE);
    let mut cur_sf: u8 = 11;

    let mut rx_buffer: Vec<Complex<f32>> = Vec::new();

    let mut produced: u64 = 0;
    let mut seq: u32 = 0;
    let mut next_tx_at: u64 = SR_HZ; // first packet after 1 s warm-up
    let mut next_beacon_at: u64 = SR_HZ / 2; // first beacon after 0.5 s

    let samples_per_tick = (SR_HZ as f64 * TICK.as_secs_f64()).round() as usize;

    // Maximum rx_buffer before we force-drain.  Must hold at least one full
    // maximum-size packet.  At SF=12/OS=4 the worst-case frame (253 B payload)
    // is ~3 M samples; 4 M gives comfortable margin.
    const MAX_RX_BUF: usize = 4_000_000;

    loop {
        let tick_start = Instant::now();

        if !shared.running.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(50));
            continue;
        }

        // ── Read params (single lock per field, fast) ────────────────────
        let sf         = *shared.sf.lock().unwrap();
        let signal_amp = db_to_amp(*shared.signal_db.lock().unwrap());
        let noise_sigma = db_to_amp(*shared.noise_db.lock().unwrap()) / std::f32::consts::SQRT_2;
        let interval_ms = *shared.interval_ms.lock().unwrap();
        let interval_samples = interval_ms * SR_HZ / 1000;

        channel.set_signal_amp(signal_amp);
        channel.set_noise_sigma(noise_sigma);

        // Rebuild modems if SF changed.
        if sf != cur_sf {
            cur_sf = sf;
            tx_modem = Tx::new(sf, CR, OS_FACTOR, SYNC_WORD, PREAMBLE);
            rx_modem = Rx::new(sf, CR, OS_FACTOR, SYNC_WORD, PREAMBLE);
            rx_buffer.clear();
            channel.clear();
        }

        // ── NodeInfo beacon ──────────────────────────────────────────────
        if produced >= next_beacon_at {
            next_beacon_at = produced + BEACON_INTERVAL;

            if let Some(frame) = node_a.build_nodeinfo_frame() {
                let iq = tx_modem.modulate(&frame.to_bytes());
                channel.push_samples(iq);
            }
            if let Some(frame) = node_b.build_nodeinfo_frame() {
                let iq = tx_modem.modulate(&frame.to_bytes());
                channel.push_samples(iq);
            }

            *shared.a_neighbours.lock().unwrap() = node_a.neighbours()
                .iter().map(|n| format!("{} (!{:08x})", n.short_name, n.node_id)).collect();
            *shared.b_neighbours.lock().unwrap() = node_b.neighbours()
                .iter().map(|n| format!("{} (!{:08x})", n.short_name, n.node_id)).collect();
        }

        // ── Text TX ──────────────────────────────────────────────────────
        if produced >= next_tx_at {
            next_tx_at = produced + interval_samples;
            seq += 1;
            let text = format!("Hello from A #{seq}");

            if let Some(frame) = node_a.build_text_frame(BROADCAST, &text) {
                shared.tx_count.fetch_add(1, Ordering::Relaxed);
                push_log(&shared, true, format!("TX A→* \"{text}\""));
                let iq = tx_modem.modulate(&frame.to_bytes());
                channel.push_samples(iq);
            }
        }

        // ── Tick channel → mixed IQ ──────────────────────────────────────
        let mixed = channel.tick(samples_per_tick);
        produced += samples_per_tick as u64;

        // ── FFT → spectrum & waterfall ───────────────────────────────────
        let mut peak: Vec<[f64; 2]> = Vec::new();
        for chunk in mixed.chunks(FFT_SIZE) {
            if chunk.len() == FFT_SIZE {
                let spec = analyzer.compute(chunk);
                shared.waterfall_plot.update(spec.clone());
                if peak.is_empty() {
                    peak = spec;
                } else {
                    for (p, s) in peak.iter_mut().zip(spec.iter()) {
                        if s[1] > p[1] { p[1] = s[1]; }
                    }
                }
            }
        }
        if !peak.is_empty() {
            shared.spectrum_plot.update(peak);
        }

        // ── Streaming RX decode ──────────────────────────────────────────
        rx_buffer.extend_from_slice(&mixed);

        // Safety-valve: prevent truly unbounded growth.
        if rx_buffer.len() > MAX_RX_BUF {
            let drain = rx_buffer.len() - MAX_RX_BUF / 2;
            rx_buffer.drain(..drain);
        }

        // Try to decode frames from the buffer.
        loop {
            match rx_modem.decode_streaming(&rx_buffer) {
                Some((payload, consumed)) => {
                    rx_buffer.drain(..consumed);
                    match node_b.process_rx_frame(&payload) {
                        Ok((Some(msg), _fwd)) => {
                            shared.rx_count.fetch_add(1, Ordering::Relaxed);
                            let label = if let Some(t) = msg.data.text() {
                                format!("RX B←* \"{}\" (hops: {})", t, msg.hop_limit)
                            } else {
                                format!("RX portnum={} to B", msg.data.portnum)
                            };
                            push_log(&shared, true, label);
                        }
                        Ok((None, _)) => {}
                        Err(e) => push_log(&shared, false, format!("ERR: {e}")),
                    }
                }
                None => break,
            }
        }

        // Sleep only the remainder of the tick to maintain real-time pacing.
        let elapsed = tick_start.elapsed();
        if let Some(remaining) = TICK.checked_sub(elapsed) {
            thread::sleep(remaining);
        }
    }
}

// ── GUI ──────────────────────────────────────────────────────────────────────

struct MeshSimApp {
    shared:          Arc<SimShared>,
    sf:              u8,
    signal_db:       f32,
    noise_db:        f32,
    interval_ms:     u64,
    preset_idx:      usize,
    spectrum_chart:  Chart,
    waterfall_chart: Chart,
}

impl MeshSimApp {
    fn new(shared: Arc<SimShared>) -> Self {
        let mut spectrum_chart = Chart::new("spectrum");
        spectrum_chart.set_x_limits([0.0, FFT_SIZE as f64]);
        spectrum_chart.set_y_limits([-90.0, 50.0]);
        spectrum_chart.set_link_axis("mesh_link", true, false);
        spectrum_chart.set_link_cursor("mesh_link", true, false);
        spectrum_chart.add(shared.spectrum_plot.clone());

        let mut waterfall_chart = Chart::new("waterfall");
        waterfall_chart.set_x_limits([0.0, FFT_SIZE as f64]);
        waterfall_chart.set_y_limits([0.0, 1.0]);
        waterfall_chart.set_link_axis("mesh_link", true, false);
        waterfall_chart.set_link_cursor("mesh_link", true, false);
        waterfall_chart.add(shared.waterfall_plot.clone());
        let wf_secs = FFT_SIZE as f64 * 512.0 / SR_HZ as f64;
        waterfall_chart.set_y_time_display(wf_secs);

        Self {
            shared,
            sf: 11,
            signal_db: -20.0,
            noise_db: -60.0,
            interval_ms: 2000,
            preset_idx: 5, // LongFast
            spectrum_chart,
            waterfall_chart,
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
        ctx.request_repaint_after(Duration::from_millis(32));

        // ── Left settings panel ──────────────────────────────────────────
        egui::SidePanel::left("settings").min_width(190.0).show(ctx, |ui| {
            ui.heading("Settings");
            ui.separator();

            // Preset.
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

            ui.add_space(4.0);
            ui.label(format!("SF  {}", self.sf));
            if ui.add(egui::Slider::new(&mut self.sf, 7_u8..=12).show_value(false)).changed() {
                *self.shared.sf.lock().unwrap() = self.sf;
            }

            ui.add_space(4.0);
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

            ui.add_space(4.0);
            ui.label(format!("Interval  {} ms", self.interval_ms));
            if ui.add(egui::Slider::new(&mut self.interval_ms, 200_u64..=10000).show_value(false)).changed() {
                *self.shared.interval_ms.lock().unwrap() = self.interval_ms;
            }

            ui.add_space(6.0);
            let running = self.shared.running.load(Ordering::Relaxed);
            if ui.button(if running { "⏸ Pause" } else { "▶ Resume" }).clicked() {
                self.shared.running.store(!running, Ordering::Relaxed);
            }

            ui.separator();

            // Counters.
            let tx = self.shared.tx_count.load(Ordering::Relaxed);
            let rx = self.shared.rx_count.load(Ordering::Relaxed);
            ui.label(format!("TX  {tx}"));
            ui.label(format!("RX  {rx}"));
            if tx > 0 {
                ui.label(format!("PER  {:.1}%", (tx - rx.min(tx)) as f32 / tx as f32 * 100.0));
            }

            ui.separator();

            // Nodes.
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

        // ── Central: spectrum + waterfall + messages ─────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            let avail = ui.available_height();

            // Spectrum: top 25%.
            let spec_h = (avail * 0.25).max(80.0);
            ui.allocate_ui(egui::vec2(ui.available_width(), spec_h), |ui| {
                self.spectrum_chart.ui(ui);
            });

            // Waterfall: next 35%.
            let wf_h = (avail * 0.35).max(100.0);
            ui.allocate_ui(egui::vec2(ui.available_width(), wf_h), |ui| {
                self.waterfall_chart.ui(ui);
            });

            ui.separator();

            // Message log: remaining space.
            ui.heading("Mesh Messages");
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

// ── Entry point ──────────────────────────────────────────────────────────────

fn main() -> eframe::Result<()> {
    let shared = SimShared::new();
    let shared_sim = Arc::clone(&shared);
    thread::spawn(move || sim_thread(shared_sim));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Mesh Sim")
            .with_inner_size([960.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Mesh Sim",
        options,
        Box::new(|_cc| Ok(Box::new(MeshSimApp::new(shared)))),
    )
}
