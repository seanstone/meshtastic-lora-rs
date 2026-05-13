//! WebSocket wire types.
//!
//! Shared between the server (`mesh::server`, native only) and the web GUI
//! (`mesh_web` wasm binary). Kept free of axum / tokio-runtime dependencies so
//! the wasm side can pull it in without baggage.

use std::sync::atomic::Ordering;

use serde::{Deserialize, Serialize};

use crate::model::{LogEntry, SimMode, ViewModel};

/// Server → client message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", content = "c")]
pub enum ServerMsg {
    /// Periodic full snapshot of the radio state.
    Snapshot(Snapshot),
    /// A single log line appended (reserved — not emitted yet).
    LogAppend(LogEntry),
    /// A deserialization or protocol error from the offending client.
    Error(String),
}

/// Full state snapshot. Sent on WS connect and re-sent at a low rate
/// (~10 Hz today). Mirrors the user-visible parts of [`ViewModel`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub running:        bool,
    pub sf:             u8,
    pub signal_db:      f32,
    pub noise_db:       f32,
    pub interval_ms:    u64,
    pub mode:           SimMode,
    pub node_id_str:    String,
    pub node_short:     String,
    pub node_long:      String,
    pub neighbours:     Vec<String>,
    pub tx_count:       u64,
    pub rx_count:       u64,
    pub use_uhd:        bool,
    pub uhd_args:       String,
    pub uhd_freq_hz:    f64,
    pub uhd_rx_gain_db: f64,
    pub uhd_tx_gain_db: f64,
    pub uhd_loading:    bool,
    pub uhd_warning:    Option<String>,
    pub auto_tx:        bool,
    pub tx_dest:        u32,

    /// FFT-peak spectrum: dB value per bin; bin index is array position.
    /// `Vec<f32>` keeps the wire compact (~16 KB JSON per array vs ~30 KB
    /// for `[bin, value]` pairs). Empty before the first FFT.
    pub spectrum: Vec<f32>,
    /// One waterfall row, same shape as `spectrum`. Sent once per snapshot.
    pub waterfall_row: Vec<f32>,
}

impl Snapshot {
    /// Read every relevant field out of `vm`.
    pub fn from_view(vm: &ViewModel) -> Self {
        Self {
            running:        vm.running.load(Ordering::Relaxed),
            sf:             *vm.sf.lock().unwrap(),
            signal_db:      *vm.signal_db.lock().unwrap(),
            noise_db:       *vm.noise_db.lock().unwrap(),
            interval_ms:    *vm.interval_ms.lock().unwrap(),
            mode:           *vm.mode.lock().unwrap(),
            node_id_str:    vm.node_id_str.lock().unwrap().clone(),
            node_short:     vm.node_short.lock().unwrap().clone(),
            node_long:      vm.node_long.lock().unwrap().clone(),
            neighbours:     vm.neighbours.lock().unwrap().clone(),
            tx_count:       vm.tx_count.load(Ordering::Relaxed),
            rx_count:       vm.rx_count.load(Ordering::Relaxed),
            use_uhd:        vm.use_uhd.load(Ordering::Relaxed),
            uhd_args:       vm.uhd_args.lock().unwrap().clone(),
            uhd_freq_hz:    *vm.uhd_freq_hz.lock().unwrap(),
            uhd_rx_gain_db: *vm.uhd_rx_gain_db.lock().unwrap(),
            uhd_tx_gain_db: *vm.uhd_tx_gain_db.lock().unwrap(),
            uhd_loading:    vm.uhd_loading.load(Ordering::Relaxed),
            uhd_warning:    vm.uhd_warning.lock().unwrap().clone(),
            auto_tx:        vm.auto_tx.load(Ordering::Relaxed),
            tx_dest:        *vm.tx_dest.lock().unwrap(),
            spectrum:       vm.latest_spectrum.lock().unwrap().clone(),
            waterfall_row:  vm.latest_waterfall_row.lock().unwrap().clone(),
        }
    }

    /// Write every field of this snapshot back into a local `ViewModel`
    /// mirror — used by the web GUI to stay in sync with the server.
    pub fn apply(&self, vm: &ViewModel) {
        vm.running.store(self.running, Ordering::Relaxed);
        *vm.sf.lock().unwrap() = self.sf;
        *vm.signal_db.lock().unwrap() = self.signal_db;
        *vm.noise_db.lock().unwrap() = self.noise_db;
        *vm.interval_ms.lock().unwrap() = self.interval_ms;
        *vm.mode.lock().unwrap() = self.mode;
        *vm.node_id_str.lock().unwrap() = self.node_id_str.clone();
        *vm.node_short.lock().unwrap() = self.node_short.clone();
        *vm.node_long.lock().unwrap() = self.node_long.clone();
        *vm.neighbours.lock().unwrap() = self.neighbours.clone();
        vm.tx_count.store(self.tx_count, Ordering::Relaxed);
        vm.rx_count.store(self.rx_count, Ordering::Relaxed);
        vm.use_uhd.store(self.use_uhd, Ordering::Relaxed);
        *vm.uhd_args.lock().unwrap() = self.uhd_args.clone();
        *vm.uhd_freq_hz.lock().unwrap() = self.uhd_freq_hz;
        *vm.uhd_rx_gain_db.lock().unwrap() = self.uhd_rx_gain_db;
        *vm.uhd_tx_gain_db.lock().unwrap() = self.uhd_tx_gain_db;
        vm.uhd_loading.store(self.uhd_loading, Ordering::Relaxed);
        *vm.uhd_warning.lock().unwrap() = self.uhd_warning.clone();
        vm.auto_tx.store(self.auto_tx, Ordering::Relaxed);
        *vm.tx_dest.lock().unwrap() = self.tx_dest;

        // Spectrum + waterfall: reconstruct `[bin, dB]` pairs and push
        // through the same `update` API the desktop radio loop uses, so the
        // local plots scroll just like they would in-process.
        if !self.spectrum.is_empty() {
            let pairs: Vec<[f64; 2]> = self.spectrum.iter().enumerate()
                .map(|(i, &v)| [i as f64, v as f64]).collect();
            vm.spectrum_plot.update(pairs);
        }
        if !self.waterfall_row.is_empty() {
            let pairs: Vec<[f64; 2]> = self.waterfall_row.iter().enumerate()
                .map(|(i, &v)| [i as f64, v as f64]).collect();
            vm.waterfall_plot.update(pairs);
        }
    }
}
