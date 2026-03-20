/// Headless Meshtastic node — text or serial-protobuf I/O over simulated or
/// real RF (UHD).
///
/// **Text mode** (default): lines from stdin → TEXT_MESSAGE_APP broadcasts.
/// Received messages printed to stdout as `[RX] !aabb1234: "text"`.
///
/// **Serial mode** (`--serial`): speaks the Meshtastic serial protobuf framing
/// protocol on stdin/stdout (binary).  Compatible with the Meshtastic Python
/// CLI, web client, and mobile apps (via a serial-over-USB bridge).
///
/// Usage:
///   cargo run --bin mesh_node [OPTIONS]
///
/// Options:
///   --serial              Serial protobuf mode (binary stdin/stdout)
///   --name <SHORT>        Short name (≤4 chars, default: "MRST")
///   --long <LONG>         Long name (default: "meshtastic-rs")
///   --sf <7..12>          Spreading factor (default: 11)
///   --preset <name>       Modem preset name (e.g. "LongFast")
///   --uhd                 Use UHD (USRP) driver
///   --freq <MHz>          UHD center frequency (default: 906.875)
///   --args <str>          UHD device args (default: "")
///   --tx-gain <dB>        UHD TX gain (default: 40)
///   --rx-gain <dB>        UHD RX gain (default: 40)
///   --signal <dBFS>       Sim signal level (default: -20)
///   --noise <dBFS>        Sim noise level (default: -60)

use std::{
    io::{self, BufRead, Read, Write},
    sync::{Arc, atomic::{AtomicBool, Ordering}},
    time::{Duration, Instant},
};

use prost::Message as _;
use lora::channel::{Channel, Driver};
use lora::modem::{Tx, Rx, StreamDecodeResult};
use rustfft::num_complex::Complex;
#[cfg(feature = "uhd")]
use lora::uhd::UhdDevice;

use mesh::{
    app::{ChannelConfig, MeshMessage, MeshNode},
    mac::packet::BROADCAST,
    presets::preset_by_name,
    proto::{
        User, MeshPacket, MyNodeInfo, FromRadio, ToRadio,
        NodeInfo as NodeInfoProto,
        from_radio, to_radio, mesh_packet,
    },
    serial,
};
#[cfg(feature = "mqtt")]
use mesh::mqtt::{MqttConfig, spawn_mqtt_bridge, mqtt_packet_to_raw};
#[cfg(feature = "ws")]
use mesh::ws::{WsCommand, WsEvent, WsServer, spawn_ws_server};

// ── Constants ────────────────────────────────────────────────────────────────

const OS_FACTOR: u32 = 4;
const CR: u8 = 4;
const SYNC_WORD: u8 = 0x2B;
const PREAMBLE: u16 = 16;
const SR_HZ: u64 = 1_000_000;
const TICK: Duration = Duration::from_millis(16);
const MAX_RX_BUF: usize = 4_000_000;
const BEACON_INTERVAL: u64 = 15 * SR_HZ;
const FIRMWARE_VERSION: &str = "meshtastic-rs 0.1.0";

fn db_to_amp(db: f32) -> f32 { 10_f32.powf(db / 20.0) }

// ── CLI args ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Mode { Text, Serial, Mqtt }

struct Config {
    mode:        Mode,
    short_name:  String,
    long_name:   String,
    sf:          u8,
    signal_db:   f32,
    noise_db:    f32,
    use_uhd:     bool,
    uhd_freq_mhz: f64,
    uhd_args:    String,
    uhd_tx_gain: f64,
    uhd_rx_gain: f64,
    // MQTT
    mqtt_host:   String,
    mqtt_port:   u16,
    mqtt_user:   String,
    mqtt_pass:   String,
    mqtt_topic:  String,
    // WebSocket
    ws_port:     Option<u16>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode:        Mode::Text,
            short_name:  "MRST".into(),
            long_name:   "meshtastic-rs".into(),
            sf:          11,
            signal_db:   -20.0,
            noise_db:    -60.0,
            use_uhd:     false,
            uhd_freq_mhz: 906.875,
            uhd_args:    String::new(),
            uhd_tx_gain: 40.0,
            uhd_rx_gain: 40.0,
            mqtt_host:   "mqtt.meshtastic.org".into(),
            mqtt_port:   1883,
            mqtt_user:   "meshdev".into(),
            mqtt_pass:   "large4cats".into(),
            mqtt_topic:  "msh/2/c".into(),
            ws_port:     None,
        }
    }
}

fn parse_args() -> Config {
    let args: Vec<String> = std::env::args().collect();
    let mut cfg = Config::default();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--serial"     => { cfg.mode = Mode::Serial; }
            "--mqtt"       => { cfg.mode = Mode::Mqtt; }
            "--name"       => { i += 1; cfg.short_name = args[i].clone(); }
            "--long"       => { i += 1; cfg.long_name  = args[i].clone(); }
            "--sf"         => { i += 1; cfg.sf         = args[i].parse().unwrap_or(11); }
            "--preset"     => {
                i += 1;
                if let Some(p) = preset_by_name(&args[i]) {
                    cfg.sf = p.sf;
                }
            }
            "--uhd"        => { cfg.use_uhd = true; }
            "--freq"       => { i += 1; cfg.uhd_freq_mhz = args[i].parse().unwrap_or(906.875); }
            "--args"       => { i += 1; cfg.uhd_args     = args[i].clone(); }
            "--tx-gain"    => { i += 1; cfg.uhd_tx_gain  = args[i].parse().unwrap_or(40.0); }
            "--rx-gain"    => { i += 1; cfg.uhd_rx_gain  = args[i].parse().unwrap_or(40.0); }
            "--signal"     => { i += 1; cfg.signal_db    = args[i].parse().unwrap_or(-20.0); }
            "--noise"      => { i += 1; cfg.noise_db     = args[i].parse().unwrap_or(-60.0); }
            "--mqtt-host"  => { i += 1; cfg.mqtt_host    = args[i].clone(); }
            "--mqtt-port"  => { i += 1; cfg.mqtt_port    = args[i].parse().unwrap_or(1883); }
            "--mqtt-user"  => { i += 1; cfg.mqtt_user    = args[i].clone(); }
            "--mqtt-pass"  => { i += 1; cfg.mqtt_pass    = args[i].clone(); }
            "--mqtt-topic" => { i += 1; cfg.mqtt_topic   = args[i].clone(); }
            "--ws"         => { cfg.ws_port = Some(9001); }
            "--ws-port"    => { i += 1; cfg.ws_port = Some(args[i].parse().unwrap_or(9001)); }
            other          => { eprintln!("unknown arg: {other}"); }
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
            &cfg.uhd_args, cfg.uhd_freq_mhz * 1e6,
            sr_hz, bw_hz, cfg.uhd_rx_gain, cfg.uhd_tx_gain,
        ) {
            Ok(dev) => return Box::new(dev),
            Err(e)  => eprintln!("[uhd] open failed: {e} — falling back to sim"),
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

// ── Serial-mode helpers ──────────────────────────────────────────────────────

/// Send a FromRadio message to stdout with serial framing.
fn send_from_radio(out: &mut impl Write, msg: &FromRadio) {
    let pb = msg.encode_to_vec();
    if let Some(frame) = serial::encode(&pb) {
        let _ = out.write_all(&frame);
        let _ = out.flush();
    }
}

/// Convert a received MeshMessage to a FromRadio packet.
fn msg_to_from_radio(msg: &MeshMessage, seq: &mut u32) -> FromRadio {
    *seq += 1;
    FromRadio {
        id: *seq,
        payload_variant: Some(from_radio::PayloadVariant::Packet(MeshPacket {
            from:      msg.from,
            to:        msg.to,
            hop_limit: msg.hop_limit as u32,
            payload_variant: Some(mesh_packet::PayloadVariant::Decoded(msg.data.clone())),
            ..Default::default()
        })),
    }
}

/// Handle the config handshake: send MyNodeInfo, all known nodes, config_complete.
fn send_config(
    out:     &mut impl Write,
    node:    &MeshNode,
    cfg:     &Config,
    want_id: u32,
    seq:     &mut u32,
) {
    // MyNodeInfo
    *seq += 1;
    send_from_radio(out, &FromRadio {
        id: *seq,
        payload_variant: Some(from_radio::PayloadVariant::MyInfo(MyNodeInfo {
            my_node_num:   node.node_id(),
            ..Default::default()
        })),
    });

    // Our own NodeInfo
    *seq += 1;
    send_from_radio(out, &FromRadio {
        id: *seq,
        payload_variant: Some(from_radio::PayloadVariant::NodeInfo(NodeInfoProto {
            num:  node.node_id(),
            user: Some(User {
                id:         format!("!{:08x}", node.node_id()),
                long_name:  cfg.long_name.clone(),
                short_name: cfg.short_name.clone(),
                ..Default::default()
            }),
            ..Default::default()
        })),
    });

    // Known neighbours
    for n in node.neighbours() {
        *seq += 1;
        send_from_radio(out, &FromRadio {
            id: *seq,
            payload_variant: Some(from_radio::PayloadVariant::NodeInfo(NodeInfoProto {
                num:  n.node_id,
                user: Some(User {
                    id:         format!("!{:08x}", n.node_id),
                    long_name:  n.long_name.clone(),
                    short_name: n.short_name.clone(),
                    ..Default::default()
                }),
                ..Default::default()
            })),
        });
    }

    // ConfigComplete
    *seq += 1;
    send_from_radio(out, &FromRadio {
        id: *seq,
        payload_variant: Some(from_radio::PayloadVariant::ConfigCompleteId(want_id)),
    });
}

/// Process a decoded ToRadio message.  Returns an optional MeshFrame to TX.
fn handle_to_radio(
    to_radio: &ToRadio,
    node:     &MeshNode,
    out:      &mut impl Write,
    cfg:      &Config,
    seq:      &mut u32,
) -> Option<mesh::mac::packet::MeshFrame> {
    let variant = to_radio.payload_variant.as_ref()?;
    match variant {
        to_radio::PayloadVariant::WantConfigId(want_id) => {
            send_config(out, node, cfg, *want_id, seq);
            None
        }
        to_radio::PayloadVariant::Packet(pkt) => {
            // Extract Data from the packet.
            let data = match pkt.payload_variant.as_ref()? {
                mesh_packet::PayloadVariant::Decoded(d) => d.clone(),
                mesh_packet::PayloadVariant::Encrypted(_) => {
                    eprintln!("[serial] ignoring encrypted ToRadio packet");
                    return None;
                }
            };
            let to = if pkt.to == 0 { BROADCAST } else { pkt.to };
            node.build_frame(to, &data)
        }
        _ => None, // Disconnect, XmodemPacket, MqttClientProxyMessage, etc.
    }
}

// ── PHY RX handler (shared between modes) ────────────────────────────────────

struct PhyState {
    rx_buffer: Vec<Complex<f32>>,
    produced:  u64,
    next_beacon_at: u64,
}

impl PhyState {
    fn new() -> Self {
        Self {
            rx_buffer: Vec::new(),
            produced:  0,
            next_beacon_at: SR_HZ,
        }
    }
}

/// Run one tick: push beacons, tick driver, decode RX, return decoded messages.
fn tick_phy(
    state:    &mut PhyState,
    node:     &mut MeshNode,
    driver:   &mut Box<dyn Driver>,
    tx_modem: &Tx,
    rx_modem: &Rx,
    samples_per_tick: usize,
) -> Vec<(Option<MeshMessage>, Option<mesh::mac::packet::MeshFrame>)> {
    // NodeInfo beacon
    if state.produced >= state.next_beacon_at {
        state.next_beacon_at = state.produced + BEACON_INTERVAL;
        if let Some(frame) = node.build_nodeinfo_frame() {
            driver.push_samples(tx_modem.modulate(&frame.to_bytes()));
        }
    }

    // Tick driver
    let mixed = driver.tick(samples_per_tick);
    state.produced += mixed.len() as u64;

    // RX decode
    state.rx_buffer.extend_from_slice(&mixed);
    if state.rx_buffer.len() > MAX_RX_BUF {
        let drain = state.rx_buffer.len() - MAX_RX_BUF / 2;
        state.rx_buffer.drain(..drain);
    }

    let mut results = Vec::new();
    loop {
        match rx_modem.decode_streaming(&state.rx_buffer) {
            StreamDecodeResult::Ok { payload, consumed, .. } => {
                state.rx_buffer.drain(..consumed);
                match node.process_rx_frame(&payload) {
                    Ok(pair) => results.push(pair),
                    Err(e)   => eprintln!("[err] {e}"),
                }
            }
            StreamDecodeResult::CrcFail { consumed, .. } |
            StreamDecodeResult::DecodeFailed { consumed } => {
                state.rx_buffer.drain(..consumed);
            }
            StreamDecodeResult::None => break,
        }
    }
    results
}

// ── WS event helper ──────────────────────────────────────────────────────────

#[cfg(feature = "ws")]
fn msg_to_ws_event(m: &MeshMessage) -> WsEvent {
    WsEvent::Rx {
        from:        m.from,
        to:          m.to,
        portnum:     m.data.portnum,
        text:        m.data.text().map(|s| s.to_owned()),
        payload_len: m.data.payload.len(),
        hops:        m.hop_limit,
    }
}

// ── Text mode ────────────────────────────────────────────────────────────────

fn run_text_mode(cfg: Config) {
    // If --ws is given we need a tokio runtime for the WS server.
    #[cfg(feature = "ws")]
    if cfg.ws_port.is_some() {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(run_text_mode_async(cfg));
        return;
    }
    run_text_mode_sync(cfg);
}

fn run_text_mode_sync(cfg: Config) {
    let channel_cfg = ChannelConfig::default();
    let mut node = MeshNode::with_identity(channel_cfg, &cfg.short_name, &cfg.long_name);

    eprintln!("node !{:08x}  name={}/{}  sf={}  driver={}",
        node.node_id(), cfg.short_name, cfg.long_name, cfg.sf,
        if cfg.use_uhd { "uhd" } else { "sim" });
    eprintln!("type a line and press Enter to transmit (Ctrl-D to quit)");

    let mut driver = make_driver(&cfg);
    let tx_modem = Tx::new(cfg.sf, CR, OS_FACTOR, SYNC_WORD, PREAMBLE);
    let rx_modem = Rx::new_with_freq(
        cfg.sf, CR, OS_FACTOR, SYNC_WORD, PREAMBLE,
        cfg.uhd_freq_mhz * 1e6, SR_HZ as f64 / OS_FACTOR as f64,
    );
    let samples_per_tick = (SR_HZ as f64 * TICK.as_secs_f64()).round() as usize;

    let mut phy = PhyState::new();

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

        while let Ok(line) = rx_lines.try_recv() {
            if let Some(frame) = node.build_text_frame(BROADCAST, &line) {
                driver.push_samples(tx_modem.modulate(&frame.to_bytes()));
                println!("[TX] \"{}\"", line);
                io::stdout().flush().ok();
            }
        }

        for (msg, fwd) in tick_phy(&mut phy, &mut node, &mut driver, &tx_modem, &rx_modem, samples_per_tick) {
            if let Some(ref m) = msg {
                if let Some(t) = m.data.text() {
                    println!("[RX] !{:08x}: \"{}\"  (hops={})", m.from, t, m.hop_limit);
                } else {
                    println!("[RX] !{:08x}: portnum={} len={}", m.from, m.data.portnum, m.data.payload.len());
                }
                io::stdout().flush().ok();
            }
            if let Some(fwd_frame) = fwd {
                driver.push_samples(tx_modem.modulate(&fwd_frame.to_bytes()));
            }
        }

        let elapsed = tick_start.elapsed();
        if let Some(remaining) = TICK.checked_sub(elapsed) {
            std::thread::sleep(remaining);
        }
    }
    eprintln!("stdin closed, exiting");
}

#[cfg(feature = "ws")]
async fn run_text_mode_async(cfg: Config) {
    let channel_cfg = ChannelConfig::default();
    let mut node = MeshNode::with_identity(channel_cfg, &cfg.short_name, &cfg.long_name);

    eprintln!("node !{:08x}  name={}/{}  sf={}  driver={}",
        node.node_id(), cfg.short_name, cfg.long_name, cfg.sf,
        if cfg.use_uhd { "uhd" } else { "sim" });

    let mut ws = spawn_ws_server(cfg.ws_port.unwrap()).await.expect("ws server");

    let mut driver = make_driver(&cfg);
    let tx_modem = Tx::new(cfg.sf, CR, OS_FACTOR, SYNC_WORD, PREAMBLE);
    let rx_modem = Rx::new_with_freq(
        cfg.sf, CR, OS_FACTOR, SYNC_WORD, PREAMBLE,
        cfg.uhd_freq_mhz * 1e6, SR_HZ as f64 / OS_FACTOR as f64,
    );
    let samples_per_tick = (SR_HZ as f64 * TICK.as_secs_f64()).round() as usize;

    let mut phy = PhyState::new();

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

    eprintln!("type a line and press Enter to transmit (Ctrl-D to quit)");

    while running.load(Ordering::Relaxed) {
        let tick_start = Instant::now();

        // TX from stdin
        while let Ok(line) = rx_lines.try_recv() {
            if let Some(frame) = node.build_text_frame(BROADCAST, &line) {
                driver.push_samples(tx_modem.modulate(&frame.to_bytes()));
                ws.broadcast(&WsEvent::Tx { text: line.clone() });
                println!("[TX] \"{}\"", line);
                io::stdout().flush().ok();
            }
        }

        // TX from WebSocket
        while let Ok(cmd) = ws.commands.try_recv() {
            match cmd {
                WsCommand::SendText { to, text } => {
                    if let Some(frame) = node.build_text_frame(to, &text) {
                        driver.push_samples(tx_modem.modulate(&frame.to_bytes()));
                        ws.broadcast(&WsEvent::Tx { text: text.clone() });
                        println!("[WS TX] \"{}\"", text);
                        io::stdout().flush().ok();
                    }
                }
            }
        }

        // PHY tick
        for (msg, fwd) in tick_phy(&mut phy, &mut node, &mut driver, &tx_modem, &rx_modem, samples_per_tick) {
            if let Some(ref m) = msg {
                ws.broadcast(&msg_to_ws_event(m));
                if let Some(t) = m.data.text() {
                    println!("[RX] !{:08x}: \"{}\"  (hops={})", m.from, t, m.hop_limit);
                } else {
                    println!("[RX] !{:08x}: portnum={} len={}", m.from, m.data.portnum, m.data.payload.len());
                }
                io::stdout().flush().ok();
            }
            if let Some(fwd_frame) = fwd {
                driver.push_samples(tx_modem.modulate(&fwd_frame.to_bytes()));
            }
        }

        let elapsed = tick_start.elapsed();
        if elapsed < TICK {
            tokio::time::sleep(TICK - elapsed).await;
        }
    }
    eprintln!("exiting");
}

// ── Serial mode ──────────────────────────────────────────────────────────────

fn run_serial_mode(cfg: Config) {
    let channel_cfg = ChannelConfig::default();
    let mut node = MeshNode::with_identity(channel_cfg, &cfg.short_name, &cfg.long_name);

    eprintln!("[serial] node !{:08x}  sf={}  driver={}",
        node.node_id(), cfg.sf, if cfg.use_uhd { "uhd" } else { "sim" });

    let mut driver = make_driver(&cfg);
    let tx_modem = Tx::new(cfg.sf, CR, OS_FACTOR, SYNC_WORD, PREAMBLE);
    let rx_modem = Rx::new_with_freq(
        cfg.sf, CR, OS_FACTOR, SYNC_WORD, PREAMBLE,
        cfg.uhd_freq_mhz * 1e6, SR_HZ as f64 / OS_FACTOR as f64,
    );
    let samples_per_tick = (SR_HZ as f64 * TICK.as_secs_f64()).round() as usize;

    let mut phy = PhyState::new();
    let mut decoder = serial::StreamDecoder::new();
    let mut from_radio_seq: u32 = 0;

    // Non-blocking binary stdin reader.
    let running = Arc::new(AtomicBool::new(true));
    let running2 = running.clone();
    let (tx_bytes, rx_bytes) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        let mut buf = [0u8; 1024];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => break, // EOF
                Ok(n) => { if tx_bytes.send(buf[..n].to_vec()).is_err() { break; } }
                Err(_) => break,
            }
        }
        running2.store(false, Ordering::Relaxed);
    });

    let mut stdout = io::stdout().lock();

    while running.load(Ordering::Relaxed) {
        let tick_start = Instant::now();

        // ── Process incoming serial bytes ────────────────────────────────
        while let Ok(chunk) = rx_bytes.try_recv() {
            decoder.push(&chunk);
        }
        while let Some(pb_bytes) = decoder.next_frame() {
            match ToRadio::decode(pb_bytes.as_slice()) {
                Ok(to_radio) => {
                    if let Some(frame) = handle_to_radio(&to_radio, &node, &mut stdout, &cfg, &mut from_radio_seq) {
                        driver.push_samples(tx_modem.modulate(&frame.to_bytes()));
                        eprintln!("[serial] TX frame ({} bytes)", frame.payload.len());
                    }
                }
                Err(e) => eprintln!("[serial] bad ToRadio: {e}"),
            }
        }

        // ── PHY tick ─────────────────────────────────────────────────────
        for (msg, fwd) in tick_phy(&mut phy, &mut node, &mut driver, &tx_modem, &rx_modem, samples_per_tick) {
            if let Some(ref m) = msg {
                let fr = msg_to_from_radio(m, &mut from_radio_seq);
                send_from_radio(&mut stdout, &fr);
                eprintln!("[serial] RX from !{:08x} portnum={}", m.from, m.data.portnum);
            }
            if let Some(fwd_frame) = fwd {
                driver.push_samples(tx_modem.modulate(&fwd_frame.to_bytes()));
                eprintln!("[serial] fwd");
            }
        }

        let elapsed = tick_start.elapsed();
        if let Some(remaining) = TICK.checked_sub(elapsed) {
            std::thread::sleep(remaining);
        }
    }
    eprintln!("[serial] stdin closed, exiting");
}

// ── MQTT mode ────────────────────────────────────────────────────────────────

#[cfg(feature = "mqtt")]
fn run_mqtt_mode(cfg: Config) {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(async move {
        let channel_cfg = ChannelConfig::default();
        let mut node = MeshNode::with_identity(channel_cfg, &cfg.short_name, &cfg.long_name);
        let node_id = node.node_id();

        let mqtt_cfg = MqttConfig {
            host:       cfg.mqtt_host.clone(),
            port:       cfg.mqtt_port,
            username:   cfg.mqtt_user.clone(),
            password:   cfg.mqtt_pass.clone(),
            channel:    "LongFast".into(),
            topic_root: cfg.mqtt_topic.clone(),
        };

        eprintln!("[mqtt] node !{:08x}  broker={}:{}  topic={}",
            node_id, mqtt_cfg.host, mqtt_cfg.port, mqtt_cfg.sub_topic());

        let mut bridge = match spawn_mqtt_bridge(mqtt_cfg, node_id).await {
            Ok(b)  => b,
            Err(e) => { eprintln!("[mqtt] failed to connect: {e}"); return; }
        };
        eprintln!("[mqtt] connected, listening...");

        #[cfg(feature = "ws")]
        let mut ws: Option<WsServer> = None;
        #[cfg(feature = "ws")]
        if let Some(port) = cfg.ws_port {
            ws = Some(spawn_ws_server(port).await.expect("ws server"));
        }

        let mut driver = make_driver(&cfg);
        let tx_modem = Tx::new(cfg.sf, CR, OS_FACTOR, SYNC_WORD, PREAMBLE);
        let rx_modem = Rx::new_with_freq(
            cfg.sf, CR, OS_FACTOR, SYNC_WORD, PREAMBLE,
            cfg.uhd_freq_mhz * 1e6, SR_HZ as f64 / OS_FACTOR as f64,
        );
        let samples_per_tick = (SR_HZ as f64 * TICK.as_secs_f64()).round() as usize;
        let mut phy = PhyState::new();

        // Non-blocking stdin for text input (also works in MQTT mode).
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

        eprintln!("type a line to broadcast via RF+MQTT (Ctrl-D to quit)");

        while running.load(Ordering::Relaxed) {
            let tick_start = Instant::now();

            // ── TX from stdin ────────────────────────────────────────────
            while let Ok(line) = rx_lines.try_recv() {
                if let Some(frame) = node.build_text_frame(BROADCAST, &line) {
                    driver.push_samples(tx_modem.modulate(&frame.to_bytes()));
                    bridge.publish_frame(&frame).await;
                    #[cfg(feature = "ws")]
                    if let Some(ref ws) = ws { ws.broadcast(&WsEvent::Tx { text: line.clone() }); }
                    println!("[TX] \"{}\"", line);
                    io::stdout().flush().ok();
                }
            }

            // ── TX from WebSocket ────────────────────────────────────────
            #[cfg(feature = "ws")]
            if let Some(ref mut ws) = ws {
                while let Ok(cmd) = ws.commands.try_recv() {
                    match cmd {
                        WsCommand::SendText { to, text } => {
                            if let Some(frame) = node.build_text_frame(to, &text) {
                                driver.push_samples(tx_modem.modulate(&frame.to_bytes()));
                                bridge.publish_frame(&frame).await;
                                ws.broadcast(&WsEvent::Tx { text: text.clone() });
                                println!("[WS TX] \"{}\"", text);
                                io::stdout().flush().ok();
                            }
                        }
                    }
                }
            }

            // ── RX from MQTT ─────────────────────────────────────────────
            while let Ok(mqtt_rx) = bridge.rx.try_recv() {
                if let Some(raw) = mqtt_packet_to_raw(&mqtt_rx.packet) {
                    match node.process_rx_frame(&raw) {
                        Ok((Some(msg), fwd)) => {
                            #[cfg(feature = "ws")]
                            if let Some(ref ws) = ws { ws.broadcast(&msg_to_ws_event(&msg)); }
                            if let Some(t) = msg.data.text() {
                                println!("[MQTT RX] !{:08x}: \"{}\"  (hops={})",
                                    msg.from, t, msg.hop_limit);
                            } else {
                                println!("[MQTT RX] !{:08x}: portnum={} len={}",
                                    msg.from, msg.data.portnum, msg.data.payload.len());
                            }
                            io::stdout().flush().ok();
                            if let Some(fwd_frame) = fwd {
                                driver.push_samples(tx_modem.modulate(&fwd_frame.to_bytes()));
                            }
                        }
                        Ok(_) => {}
                        Err(e) => eprintln!("[mqtt] rx err: {e}"),
                    }
                }
            }

            // ── PHY tick ─────────────────────────────────────────────────
            for (msg, fwd) in tick_phy(&mut phy, &mut node, &mut driver, &tx_modem, &rx_modem, samples_per_tick) {
                if let Some(ref m) = msg {
                    #[cfg(feature = "ws")]
                    if let Some(ref ws) = ws { ws.broadcast(&msg_to_ws_event(m)); }
                    if let Some(t) = m.data.text() {
                        println!("[RF RX] !{:08x}: \"{}\"  (hops={})", m.from, t, m.hop_limit);
                    } else {
                        println!("[RF RX] !{:08x}: portnum={} len={}", m.from, m.data.portnum, m.data.payload.len());
                    }
                    io::stdout().flush().ok();
                }
                if let Some(fwd_frame) = fwd {
                    driver.push_samples(tx_modem.modulate(&fwd_frame.to_bytes()));
                    bridge.publish_frame(&fwd_frame).await;
                }
            }

            let elapsed = tick_start.elapsed();
            if let Some(remaining) = TICK.checked_sub(elapsed) {
                tokio::time::sleep(remaining).await;
            }
        }
        eprintln!("[mqtt] exiting");
    });
}

// ── Entry point ──────────────────────────────────────────────────────────────

fn main() {
    let cfg = parse_args();
    match cfg.mode {
        Mode::Text   => run_text_mode(cfg),
        Mode::Serial => run_serial_mode(cfg),
        Mode::Mqtt   => {
            #[cfg(feature = "mqtt")]
            run_mqtt_mode(cfg);
            #[cfg(not(feature = "mqtt"))]
            eprintln!("MQTT support not compiled in (enable the 'mqtt' feature)");
        }
    }
}
