//! Shared view-model — state observed by the GUI and mutated by the radio loop.
//!
//! Both the desktop GUI (egui native) and the web GUI (egui wasm, future) will
//! render off this same struct. On native it's owned by an `Arc<ViewModel>` and
//! mutated in place via the interior `Mutex`/`Atomic` fields; later stages will
//! introduce a command channel and a serializable snapshot for the web client.

use std::collections::VecDeque;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};

use lora::ui::{SpectrumPlot, WaterfallPlot};

use crate::mac::packet::BROADCAST;
use crate::presets::{DEFAULT_REGION_IDX, PRESETS, REGIONS};

// ── Defaults ────────────────────────────────────────────────────────────────

pub const DEFAULT_SF: u8 = 11;
pub const DEFAULT_SIGNAL_DB: f32 = -20.0;
pub const DEFAULT_NOISE_DB: f32 = -60.0;
pub const DEFAULT_INTERVAL_MS: u64 = 2000;
pub const DEFAULT_PRESET_IDX: usize = 5;
/// FFT size used by the spectrum / waterfall plots. The plots are sized to
/// this on construction.
pub const FFT_SIZE: usize = 2048;
/// Sample rate driving the radio loop. Exposed here so the GUI can label its
/// time/frequency axes against the same rate.
pub const SR_HZ: u64 = 1_000_000;

// ── Operating mode ──────────────────────────────────────────────────────────

/// Operating mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SimMode {
    /// Normal operation. Manual + auto TX/RX on a single node. Use with UHD
    /// to talk to real Meshtastic radios.
    Terminal,
    /// RX-only mode.  No TX at all — no beacons, no forwarding, no user messages.
    /// The USRP stays completely silent.  Good for passive monitoring.
    Listen,
}

// ── Log ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum MsgDir { Tx, Rx, Fwd, System, Error }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub time:        String,
    pub dir:         MsgDir,
    pub text:        String,
    pub from_id:     Option<u32>,
    pub hops:        Option<u8>,
    pub self_origin: bool,
}

// ── Commands ────────────────────────────────────────────────────────────────

/// A user-initiated mutation of the radio state.
///
/// The radio loop is the sole writer of [`ViewModel`]; UI code (desktop egui
/// today, web egui in the future) dispatches `Command`s on an `mpsc` channel
/// and the loop calls [`Command::apply`] on each one. Side effects like
/// flipping `rebuild_driver` or `rebuild_nodes` are part of `apply`, so a
/// caller doesn't need to know which mutations require a rebuild.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", content = "c")]
pub enum Command {
    SetSf(u8),
    SetSignalDb(f32),
    SetNoiseDb(f32),
    SetIntervalMs(u64),
    SetMode(SimMode),
    SetNodeShort(String),
    SetNodeLong(String),
    SetTxDest(u32),
    SetUhdEnabled(bool),
    SetUhdArgs(String),
    SetUhdFreqHz(f64),
    SetUhdRxGainDb(f64),
    SetUhdTxGainDb(f64),
    SetAutoTx(bool),
    SetRunning(bool),
    SendText(String),
    ResetStats,
}

impl Command {
    pub fn apply(self, vm: &ViewModel) {
        match self {
            Command::SetSf(n)         => *vm.sf.lock().unwrap() = n,
            Command::SetSignalDb(v)   => *vm.signal_db.lock().unwrap() = v,
            Command::SetNoiseDb(v)    => *vm.noise_db.lock().unwrap() = v,
            Command::SetIntervalMs(v) => *vm.interval_ms.lock().unwrap() = v,
            Command::SetMode(m) => {
                *vm.mode.lock().unwrap() = m;
                vm.rebuild_nodes.store(true, Ordering::Relaxed);
            }
            Command::SetNodeShort(s) => {
                *vm.node_short.lock().unwrap() = s;
                vm.rebuild_nodes.store(true, Ordering::Relaxed);
            }
            Command::SetNodeLong(s) => {
                *vm.node_long.lock().unwrap() = s;
                vm.rebuild_nodes.store(true, Ordering::Relaxed);
            }
            Command::SetTxDest(id) => *vm.tx_dest.lock().unwrap() = id,
            Command::SetUhdEnabled(on) => {
                vm.use_uhd.store(on, Ordering::Relaxed);
                if on { *vm.uhd_warning.lock().unwrap() = None; }
                vm.rebuild_driver.store(true, Ordering::Relaxed);
            }
            Command::SetUhdArgs(s) => {
                *vm.uhd_args.lock().unwrap() = s;
                if vm.use_uhd.load(Ordering::Relaxed) {
                    vm.rebuild_driver.store(true, Ordering::Relaxed);
                }
            }
            Command::SetUhdFreqHz(hz) => {
                *vm.uhd_freq_hz.lock().unwrap() = hz;
                if vm.use_uhd.load(Ordering::Relaxed) {
                    vm.rebuild_driver.store(true, Ordering::Relaxed);
                }
            }
            Command::SetUhdRxGainDb(v) => *vm.uhd_rx_gain_db.lock().unwrap() = v,
            Command::SetUhdTxGainDb(v) => *vm.uhd_tx_gain_db.lock().unwrap() = v,
            Command::SetAutoTx(on)     => vm.auto_tx.store(on, Ordering::Relaxed),
            Command::SetRunning(on)    => vm.running.store(on, Ordering::Relaxed),
            Command::SendText(s)       => vm.tx_queue.lock().unwrap().push_back(s),
            Command::ResetStats => {
                vm.tx_count.store(0, Ordering::Relaxed);
                vm.rx_count.store(0, Ordering::Relaxed);
                vm.log.lock().unwrap().clear();
            }
        }
    }
}

// ── ViewModel ───────────────────────────────────────────────────────────────

pub struct ViewModel {
    pub running:       AtomicBool,
    pub sf:            Mutex<u8>,
    pub signal_db:     Mutex<f32>,
    pub noise_db:      Mutex<f32>,
    pub interval_ms:   Mutex<u64>,
    pub log:           Mutex<VecDeque<LogEntry>>,
    pub neighbours:    Mutex<Vec<String>>,
    pub node_id_str:   Mutex<String>,
    pub tx_count:      AtomicU64,
    pub rx_count:      AtomicU64,

    pub spectrum_plot:  Arc<SpectrumPlot>,
    pub waterfall_plot: Arc<WaterfallPlot>,

    /// Latest FFT-peak spectrum (just the dB values; bin index is the array
    /// position). Mirrored over WS so web clients can render the same plots.
    pub latest_spectrum: Mutex<Vec<f32>>,
    /// Latest waterfall row to be appended (same shape as `latest_spectrum`).
    /// The wire protocol carries one row per snapshot — the web waterfall
    /// scrolls at the snapshot rate (~10 Hz), not the radio's FFT rate.
    pub latest_waterfall_row: Mutex<Vec<f32>>,

    pub use_uhd:        AtomicBool,
    pub uhd_args:       Mutex<String>,
    pub uhd_freq_hz:    Mutex<f64>,
    pub uhd_rx_gain_db: Mutex<f64>,
    pub uhd_tx_gain_db: Mutex<f64>,
    pub rebuild_driver: AtomicBool,
    pub uhd_loading:    AtomicBool,
    pub uhd_warning:    Mutex<Option<String>>,

    pub auto_tx:        AtomicBool,
    pub tx_queue:       Mutex<VecDeque<String>>,

    pub mode:           Mutex<SimMode>,
    pub rebuild_nodes:  AtomicBool,
    pub node_short:     Mutex<String>,
    pub node_long:      Mutex<String>,
    pub node_id:        Mutex<u32>,
    pub tx_dest:        Mutex<u32>,
}

impl ViewModel {
    pub fn new() -> Arc<Self> {
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
            neighbours:   Mutex::new(vec![]),
            node_id_str:  Mutex::new(String::new()),
            tx_count:     AtomicU64::new(0),
            rx_count:     AtomicU64::new(0),
            spectrum_plot,
            waterfall_plot,
            latest_spectrum: Mutex::new(Vec::new()),
            latest_waterfall_row: Mutex::new(Vec::new()),
            use_uhd:        AtomicBool::new(false),
            uhd_args:       Mutex::new(String::new()),
            uhd_freq_hz:    Mutex::new(REGIONS[DEFAULT_REGION_IDX].channel_freq(PRESETS[DEFAULT_PRESET_IDX].bw_khz) * 1e6),
            uhd_rx_gain_db: Mutex::new(40.0),
            uhd_tx_gain_db: Mutex::new(40.0),
            rebuild_driver: AtomicBool::new(false),
            uhd_loading:    AtomicBool::new(false),
            uhd_warning:    Mutex::new(None),
            auto_tx:        AtomicBool::new(true),
            tx_queue:       Mutex::new(VecDeque::new()),
            mode:           Mutex::new(SimMode::Terminal),
            rebuild_nodes:  AtomicBool::new(false),
            node_short:     Mutex::new("TERM".into()),
            node_long:      Mutex::new("Mesh Terminal".into()),
            node_id:        Mutex::new(rand::random::<u32>()),
            tx_dest:        Mutex::new(BROADCAST),
        })
    }
}
