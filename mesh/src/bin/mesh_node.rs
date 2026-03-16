/// Headless Meshtastic node — stdin/stdout text messaging over simulated or
/// real RF (UHD).
///
/// Lines read from stdin are transmitted as TEXT_MESSAGE_APP broadcasts.
/// Received messages and events are printed to stdout.
///
/// Usage:
///   cargo run --bin mesh_node [OPTIONS]
///
/// Options:
///   --name <SHORT>      Short name (≤4 chars, default: "MRST")
///   --long <LONG>       Long name (default: "meshtastic-rs")
///   --sf <7..12>        Spreading factor (default: 11)
///   --preset <name>     Modem preset name (e.g. "LongFast")
///   --uhd               Use UHD (USRP) driver instead of simulated channel
///   --freq <MHz>        UHD center frequency (default: 906.875)
///   --args <str>        UHD device args (default: "")
///   --tx-gain <dB>      UHD TX gain (default: 40)
///   --rx-gain <dB>      UHD RX gain (default: 40)
///   --signal <dBFS>     Sim signal level (default: -20)
///   --noise <dBFS>      Sim noise level (default: -60)

use std::{
    io::{self, BufRead, Write},
    sync::{Arc, atomic::{AtomicBool, Ordering}},
    time::{Duration, Instant},
};

use lora::channel::{Channel, Driver};
use lora::modem::{Tx, Rx};
use rustfft::num_complex::Complex;
#[cfg(feature = "uhd")]
use lora::uhd::UhdDevice;

use mesh::{
    app::{ChannelConfig, MeshNode},
    mac::packet::BROADCAST,
    presets::{PRESETS, preset_by_name},
};

// ── Defaults ─────────────────────────────────────────────────────────────────

const OS_FACTOR: u32 = 4;
const CR: u8 = 4;
const SYNC_WORD: u8 = 0x2B;
const PREAMBLE: u16 = 16;
const SR_HZ: u64 = 1_000_000;
const TICK: Duration = Duration::from_millis(16);
const MAX_RX_BUF: usize = 4_000_000;
const BEACON_INTERVAL: u64 = 15 * SR_HZ; // 15 s (longer than sim)

fn db_to_amp(db: f32) -> f32 { 10_f32.powf(db / 20.0) }

// ── CLI args (simple hand-rolled parser) ─────────────────────────────────────

struct Config {
    short_name: String,
    long_name:  String,
    sf:         u8,
    signal_db:  f32,
    noise_db:   f32,
    use_uhd:    bool,
    uhd_freq_mhz: f64,
    uhd_args:   String,
    uhd_tx_gain: f64,
    uhd_rx_gain: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            short_name: "MRST".into(),
            long_name:  "meshtastic-rs".into(),
            sf:         11,
            signal_db:  -20.0,
            noise_db:   -60.0,
            use_uhd:    false,
            uhd_freq_mhz: 906.875,
            uhd_args:   String::new(),
            uhd_tx_gain: 40.0,
            uhd_rx_gain: 40.0,
        }
    }
}

fn parse_args() -> Config {
    let args: Vec<String> = std::env::args().collect();
    let mut cfg = Config::default();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--name"    => { i += 1; cfg.short_name = args[i].clone(); }
            "--long"    => { i += 1; cfg.long_name  = args[i].clone(); }
            "--sf"      => { i += 1; cfg.sf         = args[i].parse().unwrap_or(11); }
            "--preset"  => {
                i += 1;
                if let Some(p) = preset_by_name(&args[i]) {
                    cfg.sf = p.sf;
                }
            }
            "--uhd"     => { cfg.use_uhd = true; }
            "--freq"    => { i += 1; cfg.uhd_freq_mhz = args[i].parse().unwrap_or(906.875); }
            "--args"    => { i += 1; cfg.uhd_args     = args[i].clone(); }
            "--tx-gain" => { i += 1; cfg.uhd_tx_gain  = args[i].parse().unwrap_or(40.0); }
            "--rx-gain" => { i += 1; cfg.uhd_rx_gain  = args[i].parse().unwrap_or(40.0); }
            "--signal"  => { i += 1; cfg.signal_db    = args[i].parse().unwrap_or(-20.0); }
            "--noise"   => { i += 1; cfg.noise_db     = args[i].parse().unwrap_or(-60.0); }
            other       => { eprintln!("unknown arg: {other}"); }
        }
        i += 1;
    }
    cfg
}

// ── Driver factory ───────────────────────────────────────────────────────────

fn make_driver(cfg: &Config) -> Box<dyn Driver> {
    #[cfg(feature = "uhd")]
    if cfg.use_uhd {
        let sr_hz = SR_HZ as f64;
        let bw_hz = sr_hz / OS_FACTOR as f64;
        match UhdDevice::new(
            &cfg.uhd_args,
            cfg.uhd_freq_mhz * 1e6,
            sr_hz, bw_hz,
            cfg.uhd_rx_gain, cfg.uhd_tx_gain,
        ) {
            Ok(dev) => return Box::new(dev),
            Err(e)  => {
                eprintln!("[uhd] open failed: {e} — falling back to sim");
            }
        }
    }
    #[cfg(not(feature = "uhd"))]
    if cfg.use_uhd {
        eprintln!("[uhd] not compiled in — falling back to sim");
    }

    let noise_sigma = db_to_amp(cfg.noise_db) / std::f32::consts::SQRT_2;
    let signal_amp  = db_to_amp(cfg.signal_db);
    Box::new(Channel::new(noise_sigma, signal_amp))
}

// ── Main loop ────────────────────────────────────────────────────────────────

fn main() {
    let cfg = parse_args();

    let channel_cfg = ChannelConfig::default();
    let mut node = MeshNode::with_identity(channel_cfg, &cfg.short_name, &cfg.long_name);

    eprintln!("node !{:08x}  name={}/{}  sf={}  driver={}",
        node.node_id(), cfg.short_name, cfg.long_name, cfg.sf,
        if cfg.use_uhd { "uhd" } else { "sim" });
    if cfg.use_uhd {
        eprintln!("  freq={:.3} MHz  tx_gain={:.0} dB  rx_gain={:.0} dB  args=\"{}\"",
            cfg.uhd_freq_mhz, cfg.uhd_tx_gain, cfg.uhd_rx_gain, cfg.uhd_args);
    } else {
        eprintln!("  signal={:.0} dBFS  noise={:.0} dBFS  snr={:.0} dB",
            cfg.signal_db, cfg.noise_db, cfg.signal_db - cfg.noise_db);
    }
    eprintln!("type a line and press Enter to transmit (Ctrl-D to quit)");

    let mut driver = make_driver(&cfg);
    let tx_modem = Tx::new(cfg.sf, CR, OS_FACTOR, SYNC_WORD, PREAMBLE);
    let rx_modem = Rx::new(cfg.sf, CR, OS_FACTOR, SYNC_WORD, PREAMBLE);

    let samples_per_tick = (SR_HZ as f64 * TICK.as_secs_f64()).round() as usize;

    let mut rx_buffer: Vec<Complex<f32>> = Vec::new();
    let mut produced: u64 = 0;
    let mut next_beacon_at: u64 = SR_HZ; // first beacon after 1 s

    // Non-blocking stdin.
    let running = Arc::new(AtomicBool::new(true));
    let running2 = running.clone();
    let (tx_lines, rx_lines) = std::sync::mpsc::channel::<String>();

    std::thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(l) if l.is_empty() => continue,
                Ok(l) => { if tx_lines.send(l).is_err() { break; } }
                Err(_) => break,
            }
        }
        running2.store(false, Ordering::Relaxed);
    });

    while running.load(Ordering::Relaxed) {
        let tick_start = Instant::now();

        // ── TX: drain stdin lines ────────────────────────────────────────
        while let Ok(line) = rx_lines.try_recv() {
            if let Some(frame) = node.build_text_frame(BROADCAST, &line) {
                let iq = tx_modem.modulate(&frame.to_bytes());
                driver.push_samples(iq);
                println!("[TX] \"{}\"", line);
                io::stdout().flush().ok();
            } else {
                eprintln!("[err] message too long");
            }
        }

        // ── NodeInfo beacon ──────────────────────────────────────────────
        if produced >= next_beacon_at {
            next_beacon_at = produced + BEACON_INTERVAL;
            if let Some(frame) = node.build_nodeinfo_frame() {
                let iq = tx_modem.modulate(&frame.to_bytes());
                driver.push_samples(iq);
            }
        }

        // ── Tick driver ──────────────────────────────────────────────────
        let mixed = driver.tick(samples_per_tick);
        produced += mixed.len() as u64;

        // ── RX decode ────────────────────────────────────────────────────
        rx_buffer.extend_from_slice(&mixed);

        if rx_buffer.len() > MAX_RX_BUF {
            let drain = rx_buffer.len() - MAX_RX_BUF / 2;
            rx_buffer.drain(..drain);
        }

        loop {
            match rx_modem.decode_streaming(&rx_buffer) {
                Some((payload, consumed)) => {
                    rx_buffer.drain(..consumed);
                    match node.process_rx_frame(&payload) {
                        Ok((Some(msg), fwd)) => {
                            if let Some(t) = msg.data.text() {
                                println!("[RX] !{:08x}: \"{}\"  (hops={})",
                                    msg.from, t, msg.hop_limit);
                            } else {
                                println!("[RX] !{:08x}: portnum={} len={}",
                                    msg.from, msg.data.portnum, msg.data.payload.len());
                            }
                            io::stdout().flush().ok();

                            // Forward if needed.
                            if let Some(fwd_frame) = fwd {
                                let iq = tx_modem.modulate(&fwd_frame.to_bytes());
                                driver.push_samples(iq);
                                eprintln!("[fwd] relayed packet from !{:08x}", msg.from);
                            }
                        }
                        Ok((None, Some(fwd_frame))) => {
                            let iq = tx_modem.modulate(&fwd_frame.to_bytes());
                            driver.push_samples(iq);
                            eprintln!("[fwd] relayed packet (not for us)");
                        }
                        Ok((None, None)) => {} // dup or drop
                        Err(e) => eprintln!("[err] {e}"),
                    }
                }
                None => break,
            }
        }

        // Sleep remainder of tick.
        let elapsed = tick_start.elapsed();
        if let Some(remaining) = TICK.checked_sub(elapsed) {
            std::thread::sleep(remaining);
        }
    }

    eprintln!("stdin closed, exiting");
}
