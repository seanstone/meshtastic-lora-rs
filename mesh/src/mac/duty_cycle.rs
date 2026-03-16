/// Duty-cycle tracker for LoRa transmissions.
///
/// Tracks cumulative on-air time in a rolling 3 600-second window and enforces
/// a configurable airtime budget (e.g. EU868 = 1 %, US915 = no legal cap).
///
/// Time-on-air formula (LoRa, simplified):
/// ```text
/// t_sym  = 2^SF / BW_hz
/// n_sym  = preamble + 4.25
///        + ceil((8·PL − 4·SF + 28 + 16·CRC) / (4·SF·(1+LDRO))) · (CR + 4)
/// t_air  = n_sym · t_sym
/// ```

use std::collections::VecDeque;
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(3600);

/// Duty-cycle budget and airtime ledger.
pub struct DutyCycle {
    /// Maximum fraction of the window allowed on-air (e.g. 0.01 for 1 %).
    limit_fraction: f64,
    /// Ring buffer of (timestamp, airtime) entries.
    log: VecDeque<(Instant, Duration)>,
}

impl DutyCycle {
    /// `limit_fraction` — e.g. `0.01` for EU868 1 % limit; `1.0` for no cap.
    pub fn new(limit_fraction: f64) -> Self {
        Self { limit_fraction, log: VecDeque::new() }
    }

    /// EU868 1 % duty-cycle instance.
    pub fn eu868() -> Self { Self::new(0.01) }

    /// No legal cap (US915-style) — still returns a usable tracker.
    pub fn uncapped() -> Self { Self::new(1.0) }

    /// Returns `true` if `t_air` can be sent without exceeding the budget.
    pub fn can_send(&mut self, t_air: Duration) -> bool {
        self.evict_stale();
        let used: Duration = self.log.iter().map(|(_, d)| *d).sum();
        let budget = Duration::from_secs_f64(WINDOW.as_secs_f64() * self.limit_fraction);
        used + t_air <= budget
    }

    /// Record a completed transmission of duration `t_air`.
    pub fn record_tx(&mut self, t_air: Duration) {
        self.log.push_back((Instant::now(), t_air));
    }

    /// Compute the on-air duration for a packet.
    ///
    /// - `sf`          : spreading factor (7–12)
    /// - `bw_hz`       : bandwidth in Hz
    /// - `cr`          : coding-rate denominator (5–8, for CR 4/5 … 4/8)
    /// - `payload_len` : payload bytes (after encryption)
    /// - `preamble`    : preamble symbol count
    pub fn time_on_air(sf: u8, bw_hz: f64, cr: u8, payload_len: usize, preamble: u16) -> Duration {
        let n        = 1u32 << sf;
        let t_sym    = n as f64 / bw_hz;
        let pl       = payload_len as f64;
        let sf_f     = sf as f64;
        let cr_f     = (cr - 4) as f64; // 1..4
        let inner    = (8.0 * pl - 4.0 * sf_f + 28.0 + 16.0).max(0.0);
        let n_data   = (inner / (4.0 * sf_f)).ceil() * (cr_f + 4.0);
        let n_sym    = preamble as f64 + 4.25 + 8.0 + n_data;
        Duration::from_secs_f64(n_sym * t_sym)
    }

    fn evict_stale(&mut self) {
        let cutoff = Instant::now().checked_sub(WINDOW).unwrap_or(Instant::now());
        while self.log.front().map(|(t, _)| *t < cutoff).unwrap_or(false) {
            self.log.pop_front();
        }
    }
}
