//! Radio simulation / driver loop.
//!
//! Owns the modems, the SDR driver (simulated AWGN channel or UHD/USRP), and a
//! single [`MeshNode`]. Reads control flags out of the shared
//! [`crate::model::ViewModel`] and pushes status (log, neighbours, stats,
//! spectrum/waterfall samples) back into it.
//!
//! Run on a tokio runtime on native, or via `wasm_bindgen_futures::spawn_local`
//! on wasm — the loop's `await` points hide the difference.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedReceiver;

use rustfft::num_complex::Complex;

use lora::channel::{Channel, Driver};
use lora::modem::{Tx, Rx, StreamDecodeResult};
use lora::ui::SpectrumAnalyzer;
#[cfg(feature = "uhd")]
use lora::uhd::UhdDevice;

use crate::app::{ChannelConfig, MeshNode};
use crate::model::{
    ViewModel, SimMode, LogEntry, MsgDir, Command,
    DEFAULT_SF, DEFAULT_INTERVAL_MS, FFT_SIZE, SR_HZ,
};

// ── Radio loop constants ────────────────────────────────────────────────────

const MAX_LOG: usize = 200;
const OS_FACTOR: u32 = 4;
const CR: u8 = 4;
const SYNC_WORD: u8 = 0x2B;
const PREAMBLE: u16 = 16;
const TICK: Duration = Duration::from_millis(16);
const BEACON_INTERVAL: u64 = 5 * SR_HZ;

// ── Platform helpers ────────────────────────────────────────────────────────

async fn tick_sleep(remaining: Duration) {
    #[cfg(not(target_arch = "wasm32"))]
    tokio::time::sleep(remaining).await;
    #[cfg(target_arch = "wasm32")]
    gloo_timers::future::TimeoutFuture::new(remaining.as_millis() as u32).await;
}

fn db_to_amp(db: f32) -> f32 { 10_f32.powf(db / 20.0) }

fn now_hms() -> String {
    #[cfg(not(target_arch = "wasm32"))]
    {
        #[repr(C)]
        struct Tm { sec: i32, min: i32, hour: i32, _rest: [i32; 6] }
        unsafe extern "C" { safe fn localtime_r(t: *const i64, result: *mut Tm) -> *mut Tm; }
        let epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
        let mut tm = Tm { sec: 0, min: 0, hour: 0, _rest: [0; 6] };
        unsafe { localtime_r(&epoch, &mut tm) };
        format!("{:02}:{:02}:{:02}", tm.hour, tm.min, tm.sec)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let d = js_sys::Date::new_0();
        format!("{:02}:{:02}:{:02}",
            d.get_hours(), d.get_minutes(), d.get_seconds())
    }
}

fn push_log(shared: &ViewModel, dir: MsgDir, text: String) {
    push_log_full(shared, dir, text, None, None, false);
}

fn push_log_full(shared: &ViewModel, dir: MsgDir, text: String, from_id: Option<u32>, hops: Option<u8>, self_origin: bool) {
    let mut log = shared.log.lock().unwrap();
    log.push_back(LogEntry { time: now_hms(), dir, text, from_id, hops, self_origin });
    if log.len() > MAX_LOG { log.pop_front(); }
}

// ── Driver factory ──────────────────────────────────────────────────────────

fn make_driver(shared: &ViewModel) -> Box<dyn Driver> {
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

// ── Node construction ───────────────────────────────────────────────────────

fn build_node(shared: &ViewModel) -> MeshNode {
    let channel_cfg = ChannelConfig::default();
    let id    = *shared.node_id.lock().unwrap();
    let short = shared.node_short.lock().unwrap().clone();
    let long  = shared.node_long.lock().unwrap().clone();
    *shared.node_id_str.lock().unwrap() = format!("!{:08x}", id);
    MeshNode::with_id(channel_cfg, id, &short, &long)
}

// ── Simulation loop ─────────────────────────────────────────────────────────

/// Drive the radio. Long-running; await this until the process exits.
///
/// `cmd_rx` carries [`Command`]s from the UI (or, later, the WS server) — the
/// loop is the sole writer of [`ViewModel`].
pub async fn sim_loop(shared: Arc<ViewModel>, mut cmd_rx: UnboundedReceiver<Command>) {
    let mut node = build_node(&shared);

    let mut driver: Box<dyn Driver> = make_driver(&shared);
    shared.uhd_loading.store(false, Ordering::Relaxed);
    // Initial make_driver already opened the device; clear the rebuild flag
    // so we don't redundantly reopen.
    shared.rebuild_driver.store(false, Ordering::Relaxed);
    let mut analyzer = SpectrumAnalyzer::new(FFT_SIZE);

    let mut tx_modem = Tx::new(DEFAULT_SF, CR, OS_FACTOR, SYNC_WORD, PREAMBLE);
    let bw_hz = SR_HZ as f64 / OS_FACTOR as f64;
    let mut rx_modem = Rx::new_with_freq(
        DEFAULT_SF, CR, OS_FACTOR, SYNC_WORD, PREAMBLE,
        *shared.uhd_freq_hz.lock().unwrap(), bw_hz,
    );
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

        // ── Drain pending commands ───────────────────────────────────────
        while let Ok(cmd) = cmd_rx.try_recv() {
            cmd.apply(&shared);
        }

        if !shared.running.load(Ordering::Relaxed) {
            tick_sleep(Duration::from_millis(50)).await;
            continue;
        }

        // ── Node rebuild (name change / mode change) ──────────────────────
        if shared.rebuild_nodes.swap(false, Ordering::Relaxed) {
            node = build_node(&shared);
            let new_mode = *shared.mode.lock().unwrap();
            push_log(&shared, MsgDir::System, match new_mode {
                SimMode::Terminal => "Mode → Terminal".into(),
                SimMode::Listen   => "Mode → Listen (RX only)".into(),
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
            rx_modem = Rx::new_with_freq(
                sf, CR, OS_FACTOR, SYNC_WORD, PREAMBLE,
                *shared.uhd_freq_hz.lock().unwrap(), bw_hz,
            );
            rx_buffer.clear();
            driver.clear();
        }
        // Keep center freq in sync with runtime changes.
        rx_modem.center_freq_hz = *shared.uhd_freq_hz.lock().unwrap();

        let cur_mode = *shared.mode.lock().unwrap();
        let tx_allowed = cur_mode != SimMode::Listen;

        // ── NodeInfo beacon ──────────────────────────────────────────────
        if produced >= next_beacon_at {
            next_beacon_at = produced + BEACON_INTERVAL;

            if tx_allowed {
                if let Some(frame) = node.build_nodeinfo_frame() {
                    let iq = tx_modem.modulate(&frame.to_bytes());
                    driver.push_samples(iq);
                }
            }

            *shared.neighbours.lock().unwrap() = node.neighbours()
                .iter().map(|n| format!("{} (!{:08x})", n.short_name, n.node_id)).collect();
        }

        // ── Read destination ──────────────────────────────────────────────
        let tx_dest = *shared.tx_dest.lock().unwrap();

        // ── Manual TX (drain user-typed messages) ─────────────────────────
        if tx_allowed {
            let manual: Vec<String> = shared.tx_queue.lock().unwrap().drain(..).collect();
            for text in manual {
                if let Some(frame) = node.build_text_frame(tx_dest, &text) {
                    shared.tx_count.fetch_add(1, Ordering::Relaxed);
                    push_log(&shared, MsgDir::Tx, format!("\"{text}\""));
                    let iq = tx_modem.modulate(&frame.to_bytes());
                    driver.push_samples(iq);
                }
            }
        } else {
            shared.tx_queue.lock().unwrap().clear();
        }

        // ── Auto TX ─────────────────────────────────────────────────────────
        if tx_allowed && shared.auto_tx.load(Ordering::Relaxed) && produced >= next_tx_at {
            next_tx_at = produced + interval_samples;
            seq += 1;
            let text = format!("Test #{seq}");

            if let Some(frame) = node.build_text_frame(tx_dest, &text) {
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
                StreamDecodeResult::Ok { payload, consumed, freq_offset_bins } => {
                    rx_buffer.drain(..consumed);
                    let bw_hz = SR_HZ as f64 / OS_FACTOR as f64;
                    let off_hz = freq_offset_bins * bw_hz / (1u64 << sf) as f64;
                    match node.process_rx_frame(&payload) {
                        Ok((Some(msg), fwd)) => {
                            if !msg.self_origin {
                                shared.rx_count.fetch_add(1, Ordering::Relaxed);
                            }
                            let label = if let Some(t) = msg.data.text() {
                                format!("\"{}\"", t)
                            } else {
                                format!("portnum={} len={}", msg.data.portnum, msg.data.payload.len())
                            };
                            let off_str = if off_hz.abs() > 10.0 {
                                format!(" [{off_hz:+.0}Hz]")
                            } else {
                                String::new()
                            };
                            push_log_full(&shared, MsgDir::Rx, format!("{label}{off_str}"),
                                Some(msg.from), Some(msg.hop_limit), msg.self_origin);
                            if tx_allowed {
                                if let Some(fwd_frame) = fwd {
                                    let iq = tx_modem.modulate(&fwd_frame.to_bytes());
                                    driver.push_samples(iq);
                                    push_log(&shared, MsgDir::Fwd, "relay".into());
                                }
                            }
                        }
                        Ok((None, Some(fwd_frame))) => {
                            if tx_allowed {
                                let iq = tx_modem.modulate(&fwd_frame.to_bytes());
                                driver.push_samples(iq);
                                push_log(&shared, MsgDir::Fwd, "relay".into());
                            }
                        }
                        Ok((None, None)) => {}
                        Err(e) => push_log(&shared, MsgDir::Error, format!("{e}")),
                    }
                }
                StreamDecodeResult::CrcFail { payload_len, cr, has_crc, consumed, freq_offset_bins } => {
                    rx_buffer.drain(..consumed);
                    let bw_hz = SR_HZ as f64 / OS_FACTOR as f64;
                    let off_hz = freq_offset_bins * bw_hz / (1u64 << sf) as f64;
                    push_log(&shared, MsgDir::Error,
                        format!("CRC fail (hdr: len={payload_len} cr=4/{} crc={}) [{off_hz:+.0}Hz]",
                            cr + 4, if has_crc { "yes" } else { "no" }));
                }
                StreamDecodeResult::DecodeFailed { consumed } => {
                    rx_buffer.drain(..consumed);
                    push_log(&shared, MsgDir::Error, "decode failed (header)".into());
                }
                StreamDecodeResult::None => break,
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
