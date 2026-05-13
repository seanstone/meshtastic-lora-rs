//! Shared view-model — state observed by the GUI and mutated by the radio loop.
//!
//! Both the desktop GUI (egui native) and the web GUI (egui wasm, future) will
//! render off this same struct. On native it's owned by an `Arc<ViewModel>` and
//! mutated in place via the interior `Mutex`/`Atomic` fields; later stages will
//! introduce a command channel and a serializable snapshot for the web client.

use std::collections::VecDeque;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64},
};

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

// ── Operating mode ──────────────────────────────────────────────────────────

/// Operating mode.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SimMode {
    /// Normal operation. Manual + auto TX/RX on a single node. Use with UHD
    /// to talk to real Meshtastic radios.
    Terminal,
    /// RX-only mode.  No TX at all — no beacons, no forwarding, no user messages.
    /// The USRP stays completely silent.  Good for passive monitoring.
    Listen,
}

// ── Log ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
pub enum MsgDir { Tx, Rx, Fwd, System, Error }

#[derive(Clone)]
pub struct LogEntry {
    pub time:        String,
    pub dir:         MsgDir,
    pub text:        String,
    pub from_id:     Option<u32>,
    pub hops:        Option<u8>,
    pub self_origin: bool,
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
