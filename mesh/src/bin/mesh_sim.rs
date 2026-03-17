/// Two-node Meshtastic simulation with egui GUI, spectrum & waterfall.
///
/// Supports both a simulated AWGN channel and real RF via UHD (USRP).
/// Compiles to native (tokio) and WASM (gloo-timers).
///
/// Run native:  cargo run --bin mesh_sim
/// Build WASM:  trunk build --release

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};

use eframe::egui::{self, Color32, RichText, ScrollArea};
use rustfft::num_complex::Complex;
use lora::channel::{Channel, Driver};
use lora::modem::{Tx, Rx};
use lora::ui::{Chart, SpectrumPlot, WaterfallPlot, SpectrumAnalyzer};
#[cfg(feature = "uhd")]
use lora::uhd::UhdDevice;

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
const SR_HZ: u64 = 1_000_000;
const TICK: Duration = Duration::from_millis(16);
const BEACON_INTERVAL: u64 = 5 * SR_HZ;

const DEFAULT_SF: u8 = 11;
const DEFAULT_SIGNAL_DB: f32 = -20.0;
const DEFAULT_NOISE_DB: f32 = -60.0;
const DEFAULT_INTERVAL_MS: u64 = 2000;
const DEFAULT_PRESET_IDX: usize = 5;

// ── Operating mode ──────────────────────────────────────────────────────────

/// Simulation mode.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SimMode {
    /// Two simulated nodes (A → B).  Auto-TX sends from A, RX decodes on B.
    TwoNodeTest,
    /// Single node (terminal).  Manual + auto TX/RX on the same node.
    /// Use with UHD to talk to real Meshtastic radios.
    Terminal,
}

// ── Platform helpers ─────────────────────────────────────────────────────────

async fn tick_sleep(remaining: Duration) {
    #[cfg(not(target_arch = "wasm32"))]
    tokio::time::sleep(remaining).await;
    #[cfg(target_arch = "wasm32")]
    gloo_timers::future::TimeoutFuture::new(remaining.as_millis() as u32).await;
}

// ── Shared state ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum MsgDir { Tx, Rx, Fwd, System, Error }

#[derive(Clone)]
struct LogEntry {
    time:     String,
    dir:      MsgDir,
    text:     String,
    from_id:  Option<u32>,
    hops:     Option<u8>,
}

struct SimShared {
    running:       AtomicBool,
    sf:            Mutex<u8>,
    signal_db:     Mutex<f32>,
    noise_db:      Mutex<f32>,
    interval_ms:   Mutex<u64>,
    log:           Mutex<VecDeque<LogEntry>>,
    a_neighbours:  Mutex<Vec<String>>,
    b_neighbours:  Mutex<Vec<String>>,
    a_id:          Mutex<String>,
    b_id:          Mutex<String>,
    tx_count:      AtomicU64,
    rx_count:      AtomicU64,

    spectrum_plot:  Arc<SpectrumPlot>,
    waterfall_plot: Arc<WaterfallPlot>,

    use_uhd:        AtomicBool,
    uhd_args:       Mutex<String>,
    uhd_freq_hz:    Mutex<f64>,
    uhd_rx_gain_db: Mutex<f64>,
    uhd_tx_gain_db: Mutex<f64>,
    rebuild_driver: AtomicBool,
    uhd_loading:    AtomicBool,
    uhd_warning:    Mutex<Option<String>>,

    auto_tx:        AtomicBool,
    tx_queue:       Mutex<VecDeque<String>>,

    mode:           Mutex<SimMode>,
    rebuild_nodes:  AtomicBool,
    node_short:     Mutex<String>,
    node_long:      Mutex<String>,
    tx_dest:        Mutex<u32>,
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
            sf:           Mutex::new(DEFAULT_SF),
            signal_db:    Mutex::new(DEFAULT_SIGNAL_DB),
            noise_db:     Mutex::new(DEFAULT_NOISE_DB),
            interval_ms:  Mutex::new(DEFAULT_INTERVAL_MS),
            log:          Mutex::new(VecDeque::new()),
            a_neighbours: Mutex::new(vec![]),
            b_neighbours: Mutex::new(vec![]),
            a_id:         Mutex::new(String::new()),
            b_id:         Mutex::new(String::new()),
            tx_count:     AtomicU64::new(0),
            rx_count:     AtomicU64::new(0),
            spectrum_plot,
            waterfall_plot,
            use_uhd:        AtomicBool::new(false),
            uhd_args:       Mutex::new(String::new()),
            uhd_freq_hz:    Mutex::new(915e6),
            uhd_rx_gain_db: Mutex::new(40.0),
            uhd_tx_gain_db: Mutex::new(40.0),
            rebuild_driver: AtomicBool::new(false),
            uhd_loading:    AtomicBool::new(false),
            uhd_warning:    Mutex::new(None),
            auto_tx:        AtomicBool::new(true),
            tx_queue:       Mutex::new(VecDeque::new()),
            mode:           Mutex::new(SimMode::TwoNodeTest),
            rebuild_nodes:  AtomicBool::new(false),
            node_short:     Mutex::new("TERM".into()),
            node_long:      Mutex::new("Mesh Terminal".into()),
            tx_dest:        Mutex::new(BROADCAST),
        })
    }
}

fn db_to_amp(db: f32) -> f32 { 10_f32.powf(db / 20.0) }

fn now_hms() -> String {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
        // UTC HH:MM:SS — avoids a chrono dependency.
        let h = (t / 3600) % 24;
        let m = (t / 60) % 60;
        let s = t % 60;
        format!("{h:02}:{m:02}:{s:02}")
    }
    #[cfg(target_arch = "wasm32")]
    {
        let d = js_sys::Date::new_0();
        format!("{:02}:{:02}:{:02}",
            d.get_hours(), d.get_minutes(), d.get_seconds())
    }
}

fn push_log(shared: &SimShared, dir: MsgDir, text: String) {
    push_log_ex(shared, dir, text, None, None);
}

fn push_log_ex(shared: &SimShared, dir: MsgDir, text: String, from_id: Option<u32>, hops: Option<u8>) {
    let mut log = shared.log.lock().unwrap();
    log.push_back(LogEntry { time: now_hms(), dir, text, from_id, hops });
    if log.len() > MAX_LOG { log.pop_front(); }
}

// ── Driver factory ───────────────────────────────────────────────────────────

fn make_driver(shared: &SimShared) -> Box<dyn Driver> {
    let noise_sigma = db_to_amp(*shared.noise_db.lock().unwrap()) / std::f32::consts::SQRT_2;
    let signal_amp  = db_to_amp(*shared.signal_db.lock().unwrap());

    #[cfg(feature = "uhd")]
    if shared.use_uhd.load(Ordering::Relaxed) {
        let args    = shared.uhd_args.lock().unwrap().clone();
        let freq    = *shared.uhd_freq_hz.lock().unwrap();
        let rx_gain = *shared.uhd_rx_gain_db.lock().unwrap();
        let tx_gain = *shared.uhd_tx_gain_db.lock().unwrap();
        let sr_hz   = SR_HZ as f64;
        let bw_hz   = sr_hz / OS_FACTOR as f64;
        match UhdDevice::new(&args, freq, sr_hz, bw_hz, rx_gain, tx_gain) {
            Ok(dev) => return Box::new(dev),
            Err(e)  => {
                let msg = format!("UHD open failed: {e}");
                eprintln!("[uhd] {msg} — falling back to sim");
                *shared.uhd_warning.lock().unwrap() = Some(msg);
                shared.use_uhd.store(false, Ordering::Relaxed);
            }
        }
    }

    Box::new(Channel::new(noise_sigma, signal_amp))
}

// ── Simulation loop (async, runs on tokio native / spawn_local wasm) ─────────

/// Node state — either two-node test or single-node terminal.
enum Nodes {
    TwoNode {
        tx_node: MeshNode,       // sends (node A)
        rx_node: MeshNode,       // receives (node B)
    },
    Single {
        node: MeshNode,          // sends and receives
    },
}

impl Nodes {
    fn tx_node(&self) -> &MeshNode {
        match self { Nodes::TwoNode { tx_node, .. } => tx_node, Nodes::Single { node } => node }
    }
    fn rx_node_mut(&mut self) -> &mut MeshNode {
        match self { Nodes::TwoNode { rx_node, .. } => rx_node, Nodes::Single { node } => node }
    }
}

fn build_nodes(mode: SimMode, shared: &SimShared) -> Nodes {
    let channel_cfg = ChannelConfig::default();
    match mode {
        SimMode::TwoNodeTest => {
            let tx_node = MeshNode::with_identity(channel_cfg.clone(), "MSIM", "Mesh-Sim Node A");
            let rx_node = MeshNode::with_identity(channel_cfg, "MRCV", "Mesh-Sim Node B");
            *shared.a_id.lock().unwrap() = format!("!{:08x}", tx_node.node_id());
            *shared.b_id.lock().unwrap() = format!("!{:08x}", rx_node.node_id());
            Nodes::TwoNode { tx_node, rx_node }
        }
        SimMode::Terminal => {
            let short = shared.node_short.lock().unwrap().clone();
            let long  = shared.node_long.lock().unwrap().clone();
            let node = MeshNode::with_identity(channel_cfg, &short, &long);
            *shared.a_id.lock().unwrap() = format!("!{:08x}", node.node_id());
            *shared.b_id.lock().unwrap() = String::new();
            *shared.b_neighbours.lock().unwrap() = vec![];
            Nodes::Single { node }
        }
    }
}

async fn sim_loop(shared: Arc<SimShared>) {
    let mode = *shared.mode.lock().unwrap();
    let mut nodes = build_nodes(mode, &shared);

    let mut driver: Box<dyn Driver> = make_driver(&shared);
    let mut analyzer = SpectrumAnalyzer::new(FFT_SIZE);

    let mut tx_modem = Tx::new(DEFAULT_SF, CR, OS_FACTOR, SYNC_WORD, PREAMBLE);
    let mut rx_modem = Rx::new(DEFAULT_SF, CR, OS_FACTOR, SYNC_WORD, PREAMBLE);
    let mut cur_sf: u8 = DEFAULT_SF;

    let mut rx_buffer: Vec<Complex<f32>> = Vec::new();

    let mut produced: u64 = 0;
    let mut seq: u32 = 0;
    let mut next_tx_at: u64 = SR_HZ;
    let mut next_beacon_at: u64 = SR_HZ / 2;
    let mut prev_interval_ms: u64 = DEFAULT_INTERVAL_MS;

    let samples_per_tick = (SR_HZ as f64 * TICK.as_secs_f64()).round() as usize;

    const MAX_RX_BUF: usize = 4_000_000;

    let mut last_uhd_rx_gain = f64::NAN;
    let mut last_uhd_tx_gain = f64::NAN;
    #[cfg(feature = "uhd")]
    let mut parked_uhd: Option<(Box<dyn Driver>, String)> = None;
    #[cfg(feature = "uhd")]
    let mut active_uhd_args = String::new();

    loop {
        #[cfg(not(target_arch = "wasm32"))]
        let tick_start = std::time::Instant::now();

        if !shared.running.load(Ordering::Relaxed) {
            tick_sleep(Duration::from_millis(50)).await;
            continue;
        }

        // ── Node rebuild (mode change) ────────────────────────────────────
        if shared.rebuild_nodes.swap(false, Ordering::Relaxed) {
            let new_mode = *shared.mode.lock().unwrap();
            nodes = build_nodes(new_mode, &shared);
            push_log(&shared, MsgDir::System, match new_mode {
                SimMode::TwoNodeTest => "Mode → Two-Node Test".into(),
                SimMode::Terminal    => "Mode → Terminal".into(),
            });
        }

        // ── Driver rebuild ───────────────────────────────────────────────
        let should_rebuild = shared.rebuild_driver.swap(false, Ordering::Relaxed);
        if should_rebuild {
            #[cfg(feature = "uhd")]
            {
                let use_uhd  = shared.use_uhd.load(Ordering::Relaxed);
                let cur_args = shared.uhd_args.lock().unwrap().clone();
                if use_uhd {
                    let freq  = *shared.uhd_freq_hz.lock().unwrap();
                    let rxg   = *shared.uhd_rx_gain_db.lock().unwrap();
                    let txg   = *shared.uhd_tx_gain_db.lock().unwrap();
                    let sr_hz = SR_HZ as f64;
                    let bw_hz = sr_hz / OS_FACTOR as f64;

                    if driver.is_parkable() && active_uhd_args == cur_args {
                        driver.park();
                        driver.unpark(freq, sr_hz, bw_hz, rxg, txg);
                    } else {
                        let reuse = parked_uhd.as_ref()
                            .map(|(_, args)| args == &cur_args)
                            .unwrap_or(false);
                        if reuse {
                            let (mut dev, _) = parked_uhd.take().unwrap();
                            dev.unpark(freq, sr_hz, bw_hz, rxg, txg);
                            driver = dev;
                        } else {
                            parked_uhd = None;
                            driver = Box::new(Channel::new(0.0, 1.0));
                            shared.uhd_loading.store(true, Ordering::Relaxed);
                            let result = UhdDevice::new(&cur_args, freq, sr_hz, bw_hz, rxg, txg);
                            shared.uhd_loading.store(false, Ordering::Relaxed);
                            match result {
                                Ok(dev) => driver = Box::new(dev),
                                Err(e) => {
                                    let msg = format!("UHD open failed: {e}");
                                    eprintln!("[uhd] {msg} — falling back to sim");
                                    *shared.uhd_warning.lock().unwrap() = Some(msg);
                                    shared.use_uhd.store(false, Ordering::Relaxed);
                                }
                            }
                        }
                        active_uhd_args = cur_args;
                    }
                    last_uhd_rx_gain = f64::NAN;
                    last_uhd_tx_gain = f64::NAN;
                } else {
                    if driver.is_parkable() {
                        let noise = db_to_amp(*shared.noise_db.lock().unwrap()) / std::f32::consts::SQRT_2;
                        let sig   = db_to_amp(*shared.signal_db.lock().unwrap());
                        let mut old = std::mem::replace(&mut driver, Box::new(Channel::new(noise, sig)));
                        old.park();
                        parked_uhd = Some((old, cur_args));
                    } else {
                        driver = make_driver(&shared);
                    }
                }
            }
            #[cfg(not(feature = "uhd"))]
            {
                driver = make_driver(&shared);
            }
            rx_buffer.clear();
            produced        = 0;
            next_tx_at      = SR_HZ;
            next_beacon_at  = SR_HZ / 2;
        }

        // ── Read params ──────────────────────────────────────────────────
        let sf         = *shared.sf.lock().unwrap();
        let signal_amp = db_to_amp(*shared.signal_db.lock().unwrap());
        let noise_sigma = db_to_amp(*shared.noise_db.lock().unwrap()) / std::f32::consts::SQRT_2;
        let interval_ms = *shared.interval_ms.lock().unwrap();
        let interval_samples = interval_ms * SR_HZ / 1000;

        if interval_ms != prev_interval_ms {
            prev_interval_ms = interval_ms;
            let earliest = produced + interval_samples;
            if next_tx_at > earliest {
                next_tx_at = earliest;
            }
        }

        driver.set_signal_amp(signal_amp);
        driver.set_noise_sigma(noise_sigma);

        let uhd_rx_gain = *shared.uhd_rx_gain_db.lock().unwrap();
        let uhd_tx_gain = *shared.uhd_tx_gain_db.lock().unwrap();
        if uhd_rx_gain != last_uhd_rx_gain { last_uhd_rx_gain = uhd_rx_gain; driver.set_hw_rx_gain(uhd_rx_gain); }
        if uhd_tx_gain != last_uhd_tx_gain { last_uhd_tx_gain = uhd_tx_gain; driver.set_hw_tx_gain(uhd_tx_gain); }

        if sf != cur_sf {
            cur_sf = sf;
            tx_modem = Tx::new(sf, CR, OS_FACTOR, SYNC_WORD, PREAMBLE);
            rx_modem = Rx::new(sf, CR, OS_FACTOR, SYNC_WORD, PREAMBLE);
            rx_buffer.clear();
            driver.clear();
        }

        // ── NodeInfo beacon ──────────────────────────────────────────────
        if produced >= next_beacon_at {
            next_beacon_at = produced + BEACON_INTERVAL;

            // TX node always beacons.
            if let Some(frame) = nodes.tx_node().build_nodeinfo_frame() {
                let iq = tx_modem.modulate(&frame.to_bytes());
                driver.push_samples(iq);
            }
            // In TwoNodeTest mode, RX node also beacons.
            if let Nodes::TwoNode { rx_node, .. } = &nodes {
                if let Some(frame) = rx_node.build_nodeinfo_frame() {
                    let iq = tx_modem.modulate(&frame.to_bytes());
                    driver.push_samples(iq);
                }
            }

            *shared.a_neighbours.lock().unwrap() = nodes.tx_node().neighbours()
                .iter().map(|n| format!("{} (!{:08x})", n.short_name, n.node_id)).collect();
            if let Nodes::TwoNode { rx_node, .. } = &nodes {
                *shared.b_neighbours.lock().unwrap() = rx_node.neighbours()
                    .iter().map(|n| format!("{} (!{:08x})", n.short_name, n.node_id)).collect();
            }
        }

        // ── Read destination ──────────────────────────────────────────────
        let tx_dest = *shared.tx_dest.lock().unwrap();

        // ── Manual TX (drain user-typed messages) ─────────────────────────
        {
            let manual: Vec<String> = shared.tx_queue.lock().unwrap().drain(..).collect();
            for text in manual {
                if let Some(frame) = nodes.tx_node().build_text_frame(tx_dest, &text) {
                    shared.tx_count.fetch_add(1, Ordering::Relaxed);
                    push_log(&shared, MsgDir::Tx, format!("\"{text}\""));
                    let iq = tx_modem.modulate(&frame.to_bytes());
                    driver.push_samples(iq);
                }
            }
        }

        // ── Auto TX ─────────────────────────────────────────────────────────
        if shared.auto_tx.load(Ordering::Relaxed) && produced >= next_tx_at {
            next_tx_at = produced + interval_samples;
            seq += 1;
            let text = format!("Test #{seq}");

            if let Some(frame) = nodes.tx_node().build_text_frame(tx_dest, &text) {
                shared.tx_count.fetch_add(1, Ordering::Relaxed);
                push_log(&shared, MsgDir::Tx, format!("\"{text}\""));
                let iq = tx_modem.modulate(&frame.to_bytes());
                driver.push_samples(iq);
            }
        }

        // ── Tick driver → mixed IQ ───────────────────────────────────────
        let mixed = driver.tick(samples_per_tick);
        produced += mixed.len() as u64;

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

        if rx_buffer.len() > MAX_RX_BUF {
            let drain = rx_buffer.len() - MAX_RX_BUF / 2;
            rx_buffer.drain(..drain);
        }

        loop {
            match rx_modem.decode_streaming(&rx_buffer) {
                Some((payload, consumed)) => {
                    rx_buffer.drain(..consumed);
                    match nodes.rx_node_mut().process_rx_frame(&payload) {
                        Ok((Some(msg), fwd)) => {
                            shared.rx_count.fetch_add(1, Ordering::Relaxed);
                            let label = if let Some(t) = msg.data.text() {
                                format!("\"{}\"", t)
                            } else {
                                format!("portnum={} len={}", msg.data.portnum, msg.data.payload.len())
                            };
                            push_log_ex(&shared, MsgDir::Rx, label,
                                Some(msg.from), Some(msg.hop_limit));
                            if let Some(fwd_frame) = fwd {
                                let iq = tx_modem.modulate(&fwd_frame.to_bytes());
                                driver.push_samples(iq);
                                push_log(&shared, MsgDir::Fwd, "relay".into());
                            }
                        }
                        Ok((None, Some(fwd_frame))) => {
                            let iq = tx_modem.modulate(&fwd_frame.to_bytes());
                            driver.push_samples(iq);
                            push_log(&shared, MsgDir::Fwd, "relay".into());
                        }
                        Ok((None, None)) => {}
                        Err(e) => push_log(&shared, MsgDir::Error, format!("{e}")),
                    }
                }
                None => break,
            }
        }

        // Sleep for the remainder of the tick.
        #[cfg(not(target_arch = "wasm32"))]
        {
            let elapsed = tick_start.elapsed();
            if elapsed < TICK { tick_sleep(TICK - elapsed).await; }
        }
        #[cfg(target_arch = "wasm32")]
        tick_sleep(TICK).await;
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
    use_uhd:         bool,
    uhd_args:        String,
    uhd_freq_mhz:    f64,
    uhd_rx_gain_db:  f64,
    uhd_tx_gain_db:  f64,
    uhd_warning:     Option<String>,
    auto_tx:         bool,
    msg_input:       String,
    mode:            SimMode,
    node_short:      String,
    node_long:       String,
    tx_dest:         u32,
    tx_dest_input:   String,
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
            sf: DEFAULT_SF,
            signal_db: DEFAULT_SIGNAL_DB,
            noise_db: DEFAULT_NOISE_DB,
            interval_ms: DEFAULT_INTERVAL_MS,
            preset_idx: DEFAULT_PRESET_IDX,
            spectrum_chart,
            waterfall_chart,
            use_uhd:        shared.use_uhd.load(Ordering::Relaxed),
            uhd_args:        String::new(),
            uhd_freq_mhz:   915.0,
            uhd_rx_gain_db:  40.0,
            uhd_tx_gain_db:  40.0,
            uhd_warning:     None,
            auto_tx:         true,
            msg_input:       String::new(),
            mode:            SimMode::TwoNodeTest,
            node_short:      "TERM".into(),
            node_long:       "Mesh Terminal".into(),
            tx_dest:         BROADCAST,
            tx_dest_input:   String::new(),
        }
    }

    fn apply_preset(&mut self, p: &ModemPreset) {
        self.sf = p.sf;
        *self.shared.sf.lock().unwrap() = p.sf;
    }

    fn restore_defaults(&mut self) {
        self.preset_idx  = DEFAULT_PRESET_IDX;
        self.sf          = DEFAULT_SF;
        self.signal_db   = DEFAULT_SIGNAL_DB;
        self.noise_db    = DEFAULT_NOISE_DB;
        self.interval_ms = DEFAULT_INTERVAL_MS;
        *self.shared.sf.lock().unwrap()          = self.sf;
        *self.shared.signal_db.lock().unwrap()   = self.signal_db;
        *self.shared.noise_db.lock().unwrap()    = self.noise_db;
        *self.shared.interval_ms.lock().unwrap() = self.interval_ms;
        self.auto_tx = true;
        self.shared.auto_tx.store(true, Ordering::Relaxed);
    }

    fn reset_stats(&self) {
        self.shared.tx_count.store(0, Ordering::Relaxed);
        self.shared.rx_count.store(0, Ordering::Relaxed);
        self.shared.log.lock().unwrap().clear();
    }

    fn trigger_rebuild(&self) {
        self.shared.rebuild_driver.store(true, Ordering::Relaxed);
    }

    fn snr_db(&self) -> f32 { self.signal_db - self.noise_db }
}

impl eframe::App for MeshSimApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(32));

        // Sync UHD state — the sim loop may have flipped use_uhd off on failure.
        #[cfg(feature = "uhd")]
        {
            let shared_uhd = self.shared.use_uhd.load(Ordering::Relaxed);
            if self.use_uhd && !shared_uhd {
                self.use_uhd = false;
                // Pick up the warning message set by the sim loop.
                self.uhd_warning = self.shared.uhd_warning.lock().unwrap().take();
            }
        }

        #[cfg(feature = "uhd")]
        if self.shared.uhd_loading.load(Ordering::Relaxed) {
            egui::Modal::new(egui::Id::new("uhd_loading")).show(ctx, |ui| {
                ui.set_min_width(220.0);
                ui.vertical_centered(|ui| {
                    ui.add_space(8.0);
                    ui.add(egui::Spinner::new().size(32.0));
                    ui.add_space(6.0);
                    ui.heading("Opening USRP…");
                    ui.add_space(8.0);
                });
            });
        }

        // ── Left settings panel ──────────────────────────────────────────
        egui::SidePanel::left("settings").min_width(200.0).show(ctx, |ui| {
            ui.heading("Settings");
            ui.horizontal(|ui| {
                if ui.selectable_label(self.mode == SimMode::TwoNodeTest, "Two-Node Test").clicked()
                    && self.mode != SimMode::TwoNodeTest
                {
                    self.mode = SimMode::TwoNodeTest;
                    *self.shared.mode.lock().unwrap() = self.mode;
                    self.shared.rebuild_nodes.store(true, Ordering::Relaxed);
                }
                if ui.selectable_label(self.mode == SimMode::Terminal, "Terminal").clicked()
                    && self.mode != SimMode::Terminal
                {
                    self.mode = SimMode::Terminal;
                    *self.shared.mode.lock().unwrap() = self.mode;
                    self.shared.rebuild_nodes.store(true, Ordering::Relaxed);
                }
            });
            ui.separator();

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

            // TX gain — sim: signal_db (dBFS);  UHD: uhd_tx_gain_db (dB)
            ui.add_space(4.0);
            if !self.use_uhd {
                ui.label(format!("TX gain  {:.0} dBFS", self.signal_db));
                if ui.add(egui::Slider::new(&mut self.signal_db, -40.0_f32..=20.0).show_value(false)).changed() {
                    *self.shared.signal_db.lock().unwrap() = self.signal_db;
                }
            } else {
                ui.label(format!("TX gain  {:.0} dB", self.uhd_tx_gain_db));
                if ui.add(egui::Slider::new(&mut self.uhd_tx_gain_db, 0.0_f64..=89.0).show_value(false)).changed() {
                    *self.shared.uhd_tx_gain_db.lock().unwrap() = self.uhd_tx_gain_db;
                }
            }

            // Noise (sim) / RX gain (UHD)
            if !self.use_uhd {
                ui.label(format!("Noise  {:.0} dBFS", self.noise_db));
                if ui.add(egui::Slider::new(&mut self.noise_db, -80.0_f32..=0.0).show_value(false)).changed() {
                    *self.shared.noise_db.lock().unwrap() = self.noise_db;
                }
                ui.label(RichText::new(format!("SNR  {:.0} dB", self.snr_db()))
                    .color(if self.snr_db() > 0.0 { Color32::GREEN } else { Color32::YELLOW }));
            } else {
                ui.label(format!("RX gain  {:.0} dB", self.uhd_rx_gain_db));
                if ui.add(egui::Slider::new(&mut self.uhd_rx_gain_db, 0.0_f64..=76.0).show_value(false)).changed() {
                    *self.shared.uhd_rx_gain_db.lock().unwrap() = self.uhd_rx_gain_db;
                }
            }

            ui.add_space(4.0);
            if ui.checkbox(&mut self.auto_tx, "Auto TX").changed() {
                self.shared.auto_tx.store(self.auto_tx, Ordering::Relaxed);
            }
            ui.add_enabled_ui(self.auto_tx, |ui| {
                ui.label(format!("Interval  {} ms", self.interval_ms));
                if ui.add(egui::Slider::new(&mut self.interval_ms, 200_u64..=10000).show_value(false)).changed() {
                    *self.shared.interval_ms.lock().unwrap() = self.interval_ms;
                }
            });

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let running = self.shared.running.load(Ordering::Relaxed);
                if ui.button(if running { "⏸ Pause" } else { "▶ Resume" }).clicked() {
                    self.shared.running.store(!running, Ordering::Relaxed);
                }
                if ui.button("Defaults").clicked() {
                    self.restore_defaults();
                }
            });

            ui.separator();

            // ── Driver selection ─────────────────────────────────────────
            ui.heading("Driver");
            ui.horizontal(|ui| {
                if ui.selectable_label(!self.use_uhd, "Sim").clicked() && self.use_uhd {
                    self.use_uhd = false;
                    self.shared.use_uhd.store(false, Ordering::Relaxed);
                    self.trigger_rebuild();
                }
                #[cfg(feature = "uhd")]
                if ui.selectable_label(self.use_uhd, "UHD").clicked() && !self.use_uhd {
                    self.use_uhd = true;
                    self.uhd_warning = None;
                    self.shared.use_uhd.store(true, Ordering::Relaxed);
                    self.trigger_rebuild();
                }
                #[cfg(not(feature = "uhd"))]
                {
                    ui.add_enabled(false, egui::SelectableLabel::new(false, "UHD (disabled)"));
                }
            });
            if let Some(warn) = &self.uhd_warning {
                ui.colored_label(egui::Color32::from_rgb(255, 160, 0), warn);
            }

            if self.use_uhd {
                ui.add_space(4.0);
                ui.label("Args");
                let args_resp = ui.add(
                    egui::TextEdit::singleline(&mut self.uhd_args)
                        .hint_text("addr=… or empty")
                        .desired_width(f32::INFINITY),
                );
                if args_resp.lost_focus() {
                    *self.shared.uhd_args.lock().unwrap() = self.uhd_args.clone();
                    self.trigger_rebuild();
                }

                ui.horizontal(|ui| {
                    ui.label("Freq");
                    if ui.add(
                        egui::DragValue::new(&mut self.uhd_freq_mhz)
                            .range(1.0..=6000.0)
                            .speed(0.1)
                            .suffix(" MHz"),
                    ).changed() {
                        *self.shared.uhd_freq_hz.lock().unwrap() = self.uhd_freq_mhz * 1e6;
                        self.trigger_rebuild();
                    }
                });
            }

            ui.separator();

            let tx = self.shared.tx_count.load(Ordering::Relaxed);
            let rx = self.shared.rx_count.load(Ordering::Relaxed);
            ui.horizontal(|ui| {
                ui.label(format!("TX {tx}"));
                ui.label(format!("RX {rx}"));
            });
            if tx > 0 {
                let per = (tx - rx.min(tx)) as f32 / tx as f32 * 100.0;
                let per_color = if per < 5.0 {
                    Color32::from_rgb(100, 220, 100)  // green
                } else if per < 20.0 {
                    Color32::from_rgb(255, 200, 80)   // amber
                } else {
                    Color32::from_rgb(220, 100, 100)  // red
                };
                ui.label(RichText::new(format!("PER  {per:.1}%")).color(per_color));
            }
            if ui.small_button("Reset stats").clicked() {
                self.reset_stats();
            }

            ui.separator();

            ui.heading("Nodes");
            let a_id = self.shared.a_id.lock().unwrap().clone();
            if self.mode == SimMode::TwoNodeTest {
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
            } else {
                ui.label(format!("ID  {a_id}"));
                ui.horizontal(|ui| {
                    ui.label("Short");
                    if ui.add(
                        egui::TextEdit::singleline(&mut self.node_short)
                            .desired_width(40.0)
                            .char_limit(4),
                    ).lost_focus() {
                        *self.shared.node_short.lock().unwrap() = self.node_short.clone();
                        self.shared.rebuild_nodes.store(true, Ordering::Relaxed);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Name");
                    if ui.add(
                        egui::TextEdit::singleline(&mut self.node_long)
                            .desired_width(ui.available_width()),
                    ).lost_focus() {
                        *self.shared.node_long.lock().unwrap() = self.node_long.clone();
                        self.shared.rebuild_nodes.store(true, Ordering::Relaxed);
                    }
                });
                ui.add_space(4.0);
                ui.label("Neighbours:");
                for n in self.shared.a_neighbours.lock().unwrap().iter() {
                    ui.label(format!("  {n}"));
                }
            }
        });

        // ── Central: spectrum + waterfall + messages ─────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            let avail = ui.available_height();

            let spec_h = (avail * 0.25).max(80.0);
            ui.allocate_ui(egui::vec2(ui.available_width(), spec_h), |ui| {
                self.spectrum_chart.ui(ui);
            });

            let wf_h = (avail * 0.35).max(100.0);
            ui.allocate_ui(egui::vec2(ui.available_width(), wf_h), |ui| {
                self.waterfall_chart.ui(ui);
            });

            ui.separator();

            ui.heading("Mesh Messages");

            // ── Destination + message input row ──────────────────────────
            ui.horizontal(|ui| {
                ui.label("To:");
                let dest_label = if self.tx_dest == BROADCAST {
                    "Broadcast".to_string()
                } else {
                    format!("!{:08x}", self.tx_dest)
                };
                egui::ComboBox::from_id_salt("tx_dest")
                    .selected_text(&dest_label)
                    .width(110.0)
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(self.tx_dest == BROADCAST, "Broadcast").clicked() {
                            self.tx_dest = BROADCAST;
                            *self.shared.tx_dest.lock().unwrap() = BROADCAST;
                        }
                        // Populate from neighbour table.
                        let neighbours = self.shared.a_neighbours.lock().unwrap().clone();
                        for entry in &neighbours {
                            // Entries are formatted "SHORT (!aabbccdd)" — parse the ID.
                            if let Some(hex) = entry.split("(!").nth(1).and_then(|s| s.strip_suffix(')')) {
                                if let Ok(id) = u32::from_str_radix(hex, 16) {
                                    let label = entry.split(" (!").next().unwrap_or(hex);
                                    if ui.selectable_label(self.tx_dest == id, label).clicked() {
                                        self.tx_dest = id;
                                        *self.shared.tx_dest.lock().unwrap() = id;
                                    }
                                }
                            }
                        }
                    });
                // Manual hex entry.
                let hex_resp = ui.add(
                    egui::TextEdit::singleline(&mut self.tx_dest_input)
                        .hint_text("or hex ID…")
                        .desired_width(72.0),
                );
                if hex_resp.lost_focus() && !self.tx_dest_input.is_empty() {
                    let cleaned = self.tx_dest_input.trim_start_matches("!").trim_start_matches("0x");
                    if let Ok(id) = u32::from_str_radix(cleaned, 16) {
                        self.tx_dest = id;
                        *self.shared.tx_dest.lock().unwrap() = id;
                    }
                    self.tx_dest_input.clear();
                }
            });
            ui.horizontal(|ui| {
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.msg_input)
                        .hint_text("Type a message…")
                        .desired_width(ui.available_width() - 55.0),
                );
                let enter = resp.lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button("Send").clicked() || enter) && !self.msg_input.is_empty() {
                    let text = std::mem::take(&mut self.msg_input);
                    self.shared.tx_queue.lock().unwrap().push_back(text);
                    resp.request_focus();
                }
            });
            ScrollArea::vertical()
                .auto_shrink([false; 2])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    let log = self.shared.log.lock().unwrap();
                    for entry in log.iter() {
                        let (prefix, color) = match entry.dir {
                            MsgDir::Tx     => ("TX ", Color32::from_rgb(100, 180, 255)),
                            MsgDir::Rx     => ("RX ", Color32::from_rgb(100, 220, 100)),
                            MsgDir::Fwd    => ("FWD", Color32::from_rgb(255, 200, 80)),
                            MsgDir::System => ("SYS", Color32::from_rgb(180, 180, 180)),
                            MsgDir::Error  => ("ERR", Color32::from_rgb(220, 100, 100)),
                        };
                        let mut line = format!("[{}] {}", entry.time, prefix);
                        if let Some(id) = entry.from_id {
                            line.push_str(&format!(" !{:08x}", id));
                        }
                        line.push_str(&format!(" {}", entry.text));
                        if let Some(h) = entry.hops {
                            line.push_str(&format!(" (hops: {h})"));
                        }
                        ui.label(RichText::new(line).color(color).monospace());
                    }
                });
        });
    }
}

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

        let shared = SimShared::new();
        let shared_sim = Arc::clone(&shared);
        wasm_bindgen_futures::spawn_local(sim_loop(shared_sim));

        let canvas = web_sys::window().unwrap()
            .document().unwrap()
            .get_element_by_id("canvas").unwrap()
            .unchecked_into::<HtmlCanvasElement>();

        let _ = eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(move |_cc| Ok(Box::new(MeshSimApp::new(shared)))),
            )
            .await;
    }
}

// ── Native entry point ───────────────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result<()> {
    let shared = SimShared::new();

    // Auto-detect USRP at startup.
    #[cfg(feature = "uhd")]
    {
        eprint!("[uhd] probing for USRP… ");
        if lora::uhd::probe() {
            eprintln!("found — switching to UHD mode");
            shared.use_uhd.store(true, Ordering::Relaxed);
            shared.rebuild_driver.store(true, Ordering::Relaxed);
        } else {
            eprintln!("none found — using simulator");
        }
    }

    let shared_sim = Arc::clone(&shared);

    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(sim_loop(shared_sim));
    });

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
