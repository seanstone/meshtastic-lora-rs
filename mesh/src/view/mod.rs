//! Egui rendering of the [`ViewModel`].
//!
//! Used by the native `mesh_radio` binary today and by the upcoming web
//! `mesh_web` binary. Holds local UI state (sliders, text inputs, chart
//! widgets) and reads/writes the shared [`ViewModel`] for cross-thread state.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::time::Duration;

use eframe::egui::{self, Color32, RichText, ScrollArea};
use lora::ui::Chart;

use crate::mac::packet::BROADCAST;
use crate::presets::{DEFAULT_REGION_IDX, ModemPreset, PRESETS, REGIONS, Region};
use crate::model::{
    ViewModel, SimMode, MsgDir, Command,
    DEFAULT_SF, DEFAULT_SIGNAL_DB, DEFAULT_NOISE_DB, DEFAULT_INTERVAL_MS,
    DEFAULT_PRESET_IDX, FFT_SIZE, SR_HZ,
};

// ── View constants ──────────────────────────────────────────────────────────

const GITHUB_ICON: char = egui::special_emojis::GITHUB;
const REPO_URL: &str = "https://github.com/seanstone/meshtastic-lora-rs";
const MOBILE_BREAKPOINT: f32 = 600.0;

// ── App ─────────────────────────────────────────────────────────────────────

pub struct MeshSimApp {
    shared:          Arc<ViewModel>,
    cmd_tx:          Sender<Command>,
    sf:              u8,
    signal_db:       f32,
    noise_db:        f32,
    interval_ms:     u64,
    preset_idx:      usize,
    region_idx:      usize,
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
    last_synced_x:   [f64; 2],
    // Mobile layout state.
    menu_open:       bool,
    msg_drawer_open: bool,
    // QR code popup.
    show_qr:         bool,
    qr_texture:      Option<egui::TextureHandle>,
}

impl MeshSimApp {
    pub fn new(shared: Arc<ViewModel>, cmd_tx: Sender<Command>) -> Self {
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

        let init_uhd = shared.use_uhd.load(Ordering::Relaxed);
        if init_uhd {
            let _ = cmd_tx.send(Command::SetAutoTx(false));
            let _ = cmd_tx.send(Command::SetMode(SimMode::Listen));
        }
        Self {
            shared,
            cmd_tx,
            sf: DEFAULT_SF,
            signal_db: DEFAULT_SIGNAL_DB,
            noise_db: DEFAULT_NOISE_DB,
            interval_ms: DEFAULT_INTERVAL_MS,
            preset_idx: DEFAULT_PRESET_IDX,
            region_idx: DEFAULT_REGION_IDX,
            spectrum_chart,
            waterfall_chart,
            use_uhd:        init_uhd,
            uhd_args:        String::new(),
            uhd_freq_mhz:   REGIONS[DEFAULT_REGION_IDX].channel_freq(PRESETS[DEFAULT_PRESET_IDX].bw_khz),
            uhd_rx_gain_db:  40.0,
            uhd_tx_gain_db:  40.0,
            uhd_warning:     None,
            auto_tx:         !init_uhd,
            msg_input:       String::new(),
            mode:            if init_uhd { SimMode::Listen } else { SimMode::Terminal },
            node_short:      "TERM".into(),
            node_long:       "Mesh Terminal".into(),
            tx_dest:         BROADCAST,
            tx_dest_input:   String::new(),
            last_synced_x:   [0.0, FFT_SIZE as f64],
            menu_open:       false,
            msg_drawer_open: false,
            show_qr:         false,
            qr_texture:      None,
        }
    }

    fn send(&self, cmd: Command) {
        let _ = self.cmd_tx.send(cmd);
    }

    fn apply_preset(&mut self, p: &ModemPreset) {
        self.sf = p.sf;
        self.send(Command::SetSf(p.sf));
        // Recalculate frequency — channel centre depends on BW.
        let r = &REGIONS[self.region_idx];
        self.uhd_freq_mhz = r.channel_freq(p.bw_khz);
        self.send(Command::SetUhdFreqHz(self.uhd_freq_mhz * 1e6));
    }

    fn restore_defaults(&mut self) {
        self.preset_idx  = DEFAULT_PRESET_IDX;
        self.region_idx  = DEFAULT_REGION_IDX;
        self.sf          = DEFAULT_SF;
        self.signal_db   = DEFAULT_SIGNAL_DB;
        self.noise_db    = DEFAULT_NOISE_DB;
        self.interval_ms = DEFAULT_INTERVAL_MS;
        self.auto_tx     = true;
        self.send(Command::SetSf(self.sf));
        self.send(Command::SetSignalDb(self.signal_db));
        self.send(Command::SetNoiseDb(self.noise_db));
        self.send(Command::SetIntervalMs(self.interval_ms));
        self.send(Command::SetAutoTx(true));
    }

    fn reset_stats(&self) {
        self.send(Command::ResetStats);
    }

    /// Get or create the QR code texture for the GitHub repo URL.
    fn qr_texture(&mut self, ctx: &egui::Context) -> egui::TextureHandle {
        if let Some(ref tex) = self.qr_texture {
            return tex.clone();
        }
        use qrcode::{QrCode, EcLevel};
        let code = QrCode::with_error_correction_level(REPO_URL, EcLevel::M)
            .expect("QR encode");
        let modules = code.to_colors();
        let w = code.width() as usize;
        // Add a 2-module quiet zone on each side.
        let qz = 2;
        let size = w + 2 * qz;
        let mut pixels = vec![egui::Color32::WHITE; size * size];
        for (i, &dark) in modules.iter().enumerate() {
            let row = i / w;
            let col = i % w;
            if dark == qrcode::Color::Dark {
                pixels[(row + qz) * size + (col + qz)] = egui::Color32::BLACK;
            }
        }
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [size, size],
            &pixels.iter()
                .flat_map(|c| [c.r(), c.g(), c.b(), c.a()])
                .collect::<Vec<u8>>(),
        );
        let tex = ctx.load_texture("qr_github", image, egui::TextureOptions::NEAREST);
        self.qr_texture = Some(tex.clone());
        tex
    }

    fn snr_db(&self) -> f32 { self.signal_db - self.noise_db }

    // ── Shared UI helpers (used by both desktop and mobile layouts) ───────

    fn ui_mode_selector(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.selectable_label(self.mode == SimMode::Terminal, "Terminal").clicked()
                && self.mode != SimMode::Terminal
            {
                self.mode = SimMode::Terminal;
                self.send(Command::SetMode(self.mode));
            }
            if ui.selectable_label(self.mode == SimMode::Listen, "Listen").clicked()
                && self.mode != SimMode::Listen
            {
                self.mode = SimMode::Listen;
                self.send(Command::SetMode(self.mode));
            }
        });
    }

    fn ui_preset_selector(&mut self, ui: &mut egui::Ui) {
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
    }

    fn ui_region_selector(&mut self, ui: &mut egui::Ui) {
        egui::ComboBox::from_id_salt("region")
            .selected_text(REGIONS[self.region_idx].name)
            .show_ui(ui, |ui| {
                for (i, r) in REGIONS.iter().enumerate() {
                    let label = format!("{} ({:.1} MHz)", r.name, r.freq_start);
                    if ui.selectable_label(self.region_idx == i, label).clicked() {
                        self.region_idx = i;
                        self.apply_region(r);
                    }
                }
            });
    }

    fn apply_region(&mut self, r: &Region) {
        let bw = PRESETS[self.preset_idx].bw_khz;
        self.uhd_freq_mhz = r.channel_freq(bw);
        self.send(Command::SetUhdFreqHz(self.uhd_freq_mhz * 1e6));
    }

    fn ui_sf_slider(&mut self, ui: &mut egui::Ui) {
        ui.label(format!("SF  {}", self.sf));
        if ui.add(egui::Slider::new(&mut self.sf, 7_u8..=12).show_value(false)).changed() {
            self.send(Command::SetSf(self.sf));
        }
    }

    fn ui_tx_gain(&mut self, ui: &mut egui::Ui) {
        if !self.use_uhd {
            ui.label(format!("TX gain  {:.0} dBFS", self.signal_db));
            if ui.add(egui::Slider::new(&mut self.signal_db, -40.0_f32..=20.0).show_value(false)).changed() {
                self.send(Command::SetSignalDb(self.signal_db));
            }
        } else {
            ui.label(format!("TX gain  {:.0} dB", self.uhd_tx_gain_db));
            if ui.add(egui::Slider::new(&mut self.uhd_tx_gain_db, 0.0_f64..=89.0).show_value(false)).changed() {
                self.send(Command::SetUhdTxGainDb(self.uhd_tx_gain_db));
            }
        }
    }

    fn ui_rx_gain_or_noise(&mut self, ui: &mut egui::Ui) {
        if !self.use_uhd {
            ui.label(format!("Noise  {:.0} dBFS", self.noise_db));
            if ui.add(egui::Slider::new(&mut self.noise_db, -80.0_f32..=0.0).show_value(false)).changed() {
                self.send(Command::SetNoiseDb(self.noise_db));
            }
            ui.label(RichText::new(format!("SNR  {:.0} dB", self.snr_db()))
                .color(if self.snr_db() > 0.0 { Color32::GREEN } else { Color32::YELLOW }));
        } else {
            ui.label(format!("RX gain  {:.0} dB", self.uhd_rx_gain_db));
            if ui.add(egui::Slider::new(&mut self.uhd_rx_gain_db, 0.0_f64..=76.0).show_value(false)).changed() {
                self.send(Command::SetUhdRxGainDb(self.uhd_rx_gain_db));
            }
        }
    }

    fn ui_auto_tx(&mut self, ui: &mut egui::Ui) {
        if ui.checkbox(&mut self.auto_tx, "Auto TX").changed() {
            self.send(Command::SetAutoTx(self.auto_tx));
        }
        ui.add_enabled_ui(self.auto_tx, |ui| {
            ui.label(format!("Interval  {} ms", self.interval_ms));
            if ui.add(egui::Slider::new(&mut self.interval_ms, 200_u64..=10000).show_value(false)).changed() {
                self.send(Command::SetIntervalMs(self.interval_ms));
            }
        });
    }

    fn ui_playback_and_defaults(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let running = self.shared.running.load(Ordering::Relaxed);
            if ui.button(if running { "⏸ Pause" } else { "▶ Resume" }).clicked() {
                self.send(Command::SetRunning(!running));
            }
            if ui.button("Defaults").clicked() {
                self.restore_defaults();
            }
        });
    }

    fn ui_driver_selector(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.selectable_label(!self.use_uhd, "Sim").clicked() && self.use_uhd {
                self.use_uhd = false;
                self.send(Command::SetUhdEnabled(false));
            }
            #[cfg(feature = "uhd")]
            if ui.selectable_label(self.use_uhd, "UHD").clicked() && !self.use_uhd {
                self.use_uhd = true;
                self.uhd_warning = None;
                self.send(Command::SetUhdEnabled(true));
            }
            #[cfg(not(feature = "uhd"))]
            {
                ui.add_enabled(false, egui::SelectableLabel::new(false, "UHD (disabled)"));
            }
        });
        if let Some(warn) = &self.uhd_warning {
            ui.colored_label(egui::Color32::from_rgb(255, 160, 0), warn);
        }
    }

    fn ui_uhd_settings(&mut self, ui: &mut egui::Ui) {
        if !self.use_uhd { return; }
        ui.label("Args");
        let args_resp = ui.add(
            egui::TextEdit::singleline(&mut self.uhd_args)
                .hint_text("addr=… or empty")
                .desired_width(f32::INFINITY),
        );
        if args_resp.lost_focus() {
            self.send(Command::SetUhdArgs(self.uhd_args.clone()));
        }
        ui.horizontal(|ui| {
            ui.label("Freq");
            if ui.add(
                egui::DragValue::new(&mut self.uhd_freq_mhz)
                    .range(1.0..=6000.0)
                    .speed(0.1)
                    .suffix(" MHz"),
            ).changed() {
                self.send(Command::SetUhdFreqHz(self.uhd_freq_mhz * 1e6));
            }
        });
    }

    fn ui_stats(&mut self, ui: &mut egui::Ui) {
        let tx = self.shared.tx_count.load(Ordering::Relaxed);
        let rx = self.shared.rx_count.load(Ordering::Relaxed);
        ui.horizontal(|ui| {
            ui.label(format!("TX {tx}"));
            ui.label(format!("RX {rx}"));
        });
        if tx > 0 {
            let per = (tx - rx.min(tx)) as f32 / tx as f32 * 100.0;
            let per_color = if per < 5.0 {
                Color32::from_rgb(100, 220, 100)
            } else if per < 20.0 {
                Color32::from_rgb(255, 200, 80)
            } else {
                Color32::from_rgb(220, 100, 100)
            };
            ui.label(RichText::new(format!("PER  {per:.1}%")).color(per_color));
        }
        if ui.small_button("Reset stats").clicked() {
            self.reset_stats();
        }
    }

    fn ui_nodes_info(&mut self, ui: &mut egui::Ui) {
        let id = self.shared.node_id_str.lock().unwrap().clone();
        ui.label(format!("ID  {id}"));
        ui.horizontal(|ui| {
            ui.label("Short");
            if ui.add(
                egui::TextEdit::singleline(&mut self.node_short)
                    .desired_width(40.0)
                    .char_limit(4),
            ).lost_focus() {
                self.send(Command::SetNodeShort(self.node_short.clone()));
            }
        });
        ui.horizontal(|ui| {
            ui.label("Name");
            if ui.add(
                egui::TextEdit::singleline(&mut self.node_long)
                    .desired_width(ui.available_width()),
            ).lost_focus() {
                self.send(Command::SetNodeLong(self.node_long.clone()));
            }
        });
        ui.add_space(4.0);
        ui.label("Neighbours:");
        for n in self.shared.neighbours.lock().unwrap().iter() {
            ui.label(format!("  {n}"));
        }
    }

    fn ui_dest_and_input(&mut self, ui: &mut egui::Ui) {
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
                        self.send(Command::SetTxDest(BROADCAST));
                    }
                    let neighbours = self.shared.neighbours.lock().unwrap().clone();
                    for entry in &neighbours {
                        if let Some(hex) = entry.split("(!").nth(1).and_then(|s| s.strip_suffix(')')) {
                            if let Ok(id) = u32::from_str_radix(hex, 16) {
                                let label = entry.split(" (!").next().unwrap_or(hex);
                                if ui.selectable_label(self.tx_dest == id, label).clicked() {
                                    self.tx_dest = id;
                                    self.send(Command::SetTxDest(id));
                                }
                            }
                        }
                    }
                });
            let hex_resp = ui.add(
                egui::TextEdit::singleline(&mut self.tx_dest_input)
                    .hint_text("or hex ID…")
                    .desired_width(72.0),
            );
            if hex_resp.lost_focus() && !self.tx_dest_input.is_empty() {
                let cleaned = self.tx_dest_input.trim_start_matches("!").trim_start_matches("0x");
                if let Ok(id) = u32::from_str_radix(cleaned, 16) {
                    self.tx_dest = id;
                    self.send(Command::SetTxDest(id));
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
                self.send(Command::SendText(text));
                resp.request_focus();
            }
        });
    }

    fn ui_message_log(&self, ui: &mut egui::Ui) {
        ScrollArea::vertical()
            .auto_shrink([false; 2])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                let log = self.shared.log.lock().unwrap();
                for entry in log.iter() {
                    let (prefix, color) = if entry.self_origin {
                        (match entry.dir {
                            MsgDir::Tx => "TX ",  MsgDir::Rx => "RX ",
                            MsgDir::Fwd => "FWD", MsgDir::System => "SYS",
                            MsgDir::Error => "ERR",
                        }, Color32::from_rgb(90, 90, 90))
                    } else {
                        match entry.dir {
                            MsgDir::Tx     => ("TX ", Color32::from_rgb(100, 180, 255)),
                            MsgDir::Rx     => ("RX ", Color32::from_rgb(100, 220, 100)),
                            MsgDir::Fwd    => ("FWD", Color32::from_rgb(255, 200, 80)),
                            MsgDir::System => ("SYS", Color32::from_rgb(180, 180, 180)),
                            MsgDir::Error  => ("ERR", Color32::from_rgb(220, 100, 100)),
                        }
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
    }

    fn unread_count(&self) -> usize {
        self.shared.log.lock().unwrap().iter()
            .filter(|e| matches!(e.dir, MsgDir::Rx))
            .count()
    }
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

        // QR code popup window.
        if self.show_qr {
            let tex = self.qr_texture(ctx);
            let mut open = self.show_qr;
            egui::Window::new("QR Code")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    let qr_size = 200.0;
                    ui.vertical_centered(|ui| {
                        ui.image(egui::load::SizedTexture::new(tex.id(), egui::vec2(qr_size, qr_size)));
                        ui.add_space(4.0);
                        ui.hyperlink_to(REPO_URL, REPO_URL);
                    });
                });
            self.show_qr = open;
        }

        // Update x-axis: show MHz when UHD active, bin indices otherwise.
        if self.use_uhd {
            let center_hz = self.uhd_freq_mhz * 1e6;
            let bw_hz     = PRESETS[self.preset_idx].bw_khz as f64 * 1000.0;
            let fft       = FFT_SIZE;
            self.spectrum_chart .set_x_freq_display(center_hz, bw_hz, fft);
            self.waterfall_chart.set_x_freq_display(center_hz, bw_hz, fft);
        } else {
            self.spectrum_chart .clear_x_freq_display();
            self.waterfall_chart.clear_x_freq_display();
        }

        let screen_w = ctx.input(|i| i.screen_rect().width());
        let is_mobile = screen_w < MOBILE_BREAKPOINT;

        if is_mobile {
            self.update_mobile(ctx);
        } else {
            self.update_desktop(ctx);
        }

        // Cross-sync X bounds: whichever chart changed since last frame
        // propagates its new range to the other one next frame.
        let sx = self.spectrum_chart.last_x_bounds();
        let wx = self.waterfall_chart.last_x_bounds();
        if sx != self.last_synced_x {
            self.waterfall_chart.sync_x_bounds(sx);
            self.last_synced_x = sx;
        } else if wx != self.last_synced_x {
            self.spectrum_chart.sync_x_bounds(wx);
            self.last_synced_x = wx;
        }
    }
}

impl MeshSimApp {
    // ── Desktop layout ───────────────────────────────────────────────────

    fn update_desktop(&mut self, ctx: &egui::Context) {
        // Left settings panel.
        egui::SidePanel::left("settings").min_width(200.0).show(ctx, |ui| {
            ui.heading("Settings");
            self.ui_mode_selector(ui);
            ui.separator();
            ui.label("Region");
            self.ui_region_selector(ui);
            ui.add_space(4.0);
            ui.label("Preset");
            self.ui_preset_selector(ui);
            ui.add_space(4.0);
            ui.label(format!("Freq  {:.3} MHz", self.uhd_freq_mhz));
            ui.add_space(4.0);
            self.ui_sf_slider(ui);
            ui.add_space(4.0);
            self.ui_tx_gain(ui);
            self.ui_rx_gain_or_noise(ui);
            ui.add_space(4.0);
            self.ui_auto_tx(ui);
            ui.add_space(6.0);
            self.ui_playback_and_defaults(ui);
            ui.separator();
            ui.heading("Driver");
            self.ui_driver_selector(ui);
            self.ui_uhd_settings(ui);
            ui.separator();
            self.ui_stats(ui);
            ui.separator();
            ui.heading("Nodes");
            self.ui_nodes_info(ui);
            ui.add_space(8.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.hyperlink_to(
                    format!("{GITHUB_ICON} meshtastic-lora-rs"),
                    REPO_URL,
                );
                if ui.small_button("⊞").on_hover_text("Show QR code").clicked() {
                    self.show_qr = !self.show_qr;
                }
            });
        });

        // Central: spectrum + waterfall + messages.
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
            self.ui_dest_and_input(ui);
            self.ui_message_log(ui);
        });
    }

    // ── Mobile layout ────────────────────────────────────────────────────

    fn update_mobile(&mut self, ctx: &egui::Context) {
        let running = self.shared.running.load(Ordering::Relaxed);
        let tx = self.shared.tx_count.load(Ordering::Relaxed);
        let rx = self.shared.rx_count.load(Ordering::Relaxed);
        let rx_count = self.unread_count();

        // ── Top toolbar ──────────────────────────────────────────────────
        egui::TopBottomPanel::top("mobile_toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                // Play / Pause.
                if ui.button(if running { "⏸" } else { "▶" }).clicked() {
                    self.send(Command::SetRunning(!running));
                }

                // Menu toggle.
                let menu_label = if self.menu_open { "✕" } else { "☰" };
                if ui.button(menu_label).clicked() {
                    self.menu_open = !self.menu_open;
                }

                // Preset badge.
                ui.label(
                    RichText::new(PRESETS[self.preset_idx].name)
                        .small()
                        .color(Color32::LIGHT_GRAY),
                );

                // TX/RX counts.
                ui.label(RichText::new(format!("TX:{tx} RX:{rx}")).small());

                // PER badge.
                if tx > 0 {
                    let per = (tx - rx.min(tx)) as f32 / tx as f32 * 100.0;
                    let per_color = if per < 5.0 {
                        Color32::from_rgb(100, 220, 100)
                    } else if per < 20.0 {
                        Color32::from_rgb(255, 200, 80)
                    } else {
                        Color32::from_rgb(220, 100, 100)
                    };
                    ui.label(RichText::new(format!("{per:.0}%")).small().color(per_color));
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Message drawer toggle with unread badge.
                    let msg_label = if rx_count > 0 {
                        format!("✉({})", rx_count)
                    } else {
                        "✉".into()
                    };
                    if ui.button(msg_label).clicked() {
                        self.msg_drawer_open = !self.msg_drawer_open;
                    }

                    // GitHub icon.
                    ui.hyperlink_to(GITHUB_ICON.to_string(), REPO_URL);

                    // Driver indicator.
                    let drv = if self.use_uhd { "UHD" } else { "Sim" };
                    ui.label(RichText::new(drv).small().color(Color32::LIGHT_GRAY));
                });
            });
        });

        // ── Collapsible settings drawer ──────────────────────────────────
        if self.menu_open {
            egui::TopBottomPanel::top("mobile_settings").show(ctx, |ui| {
                ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    egui::Grid::new("mobile_settings_grid")
                        .num_columns(2)
                        .spacing([8.0, 6.0])
                        .show(ui, |ui| {
                            // Mode.
                            ui.label("Mode");
                            self.ui_mode_selector(ui);
                            ui.end_row();

                            // Region.
                            ui.label("Region");
                            self.ui_region_selector(ui);
                            ui.end_row();

                            // Preset.
                            ui.label("Preset");
                            self.ui_preset_selector(ui);
                            ui.end_row();

                            // Freq.
                            ui.label("Freq");
                            ui.label(format!("{:.3} MHz", self.uhd_freq_mhz));
                            ui.end_row();

                            // SF.
                            ui.label(format!("SF {}", self.sf));
                            if ui.add(
                                egui::Slider::new(&mut self.sf, 7_u8..=12)
                                    .show_value(false),
                            )
                            .changed()
                            {
                                self.send(Command::SetSf(self.sf));
                            }
                            ui.end_row();

                            // TX gain.
                            if !self.use_uhd {
                                ui.label(format!("TX {:.0}dBFS", self.signal_db));
                                if ui.add(
                                    egui::Slider::new(&mut self.signal_db, -40.0_f32..=20.0)
                                        .show_value(false),
                                )
                                .changed()
                                {
                                    self.send(Command::SetSignalDb(self.signal_db));
                                }
                            } else {
                                ui.label(format!("TX {:.0}dB", self.uhd_tx_gain_db));
                                if ui.add(
                                    egui::Slider::new(&mut self.uhd_tx_gain_db, 0.0_f64..=89.0)
                                        .show_value(false),
                                )
                                .changed()
                                {
                                    self.send(Command::SetUhdTxGainDb(self.uhd_tx_gain_db));
                                }
                            }
                            ui.end_row();

                            // Noise / RX gain.
                            if !self.use_uhd {
                                ui.label(format!("Noise {:.0}dBFS", self.noise_db));
                                if ui.add(
                                    egui::Slider::new(&mut self.noise_db, -80.0_f32..=0.0)
                                        .show_value(false),
                                )
                                .changed()
                                {
                                    self.send(Command::SetNoiseDb(self.noise_db));
                                }
                                ui.end_row();
                                ui.label("SNR");
                                ui.label(
                                    RichText::new(format!("{:.0} dB", self.snr_db())).color(
                                        if self.snr_db() > 0.0 {
                                            Color32::GREEN
                                        } else {
                                            Color32::YELLOW
                                        },
                                    ),
                                );
                            } else {
                                ui.label(format!("RX {:.0}dB", self.uhd_rx_gain_db));
                                if ui.add(
                                    egui::Slider::new(&mut self.uhd_rx_gain_db, 0.0_f64..=76.0)
                                        .show_value(false),
                                )
                                .changed()
                                {
                                    self.send(Command::SetUhdRxGainDb(self.uhd_rx_gain_db));
                                }
                            }
                            ui.end_row();

                            // Auto TX + interval.
                            ui.label("Auto TX");
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut self.auto_tx, "").changed() {
                                    self.send(Command::SetAutoTx(self.auto_tx));
                                }
                                if self.auto_tx {
                                    ui.label(format!("{} ms", self.interval_ms));
                                }
                            });
                            ui.end_row();

                            if self.auto_tx {
                                ui.label("Interval");
                                if ui.add(
                                    egui::Slider::new(&mut self.interval_ms, 200_u64..=10000)
                                        .show_value(false),
                                )
                                .changed()
                                {
                                    self.send(Command::SetIntervalMs(self.interval_ms));
                                }
                                ui.end_row();
                            }

                            // Driver.
                            ui.label("Driver");
                            self.ui_driver_selector(ui);
                            ui.end_row();

                            if self.use_uhd {
                                ui.label("UHD Args");
                                let args_resp = ui.add(
                                    egui::TextEdit::singleline(&mut self.uhd_args)
                                        .hint_text("addr=…")
                                        .desired_width(ui.available_width()),
                                );
                                if args_resp.lost_focus() {
                                    self.send(Command::SetUhdArgs(self.uhd_args.clone()));
                                }
                                ui.end_row();
                                ui.label("Freq");
                                if ui.add(
                                    egui::DragValue::new(&mut self.uhd_freq_mhz)
                                        .range(1.0..=6000.0)
                                        .speed(0.1)
                                        .suffix(" MHz"),
                                )
                                .changed()
                                {
                                    self.send(Command::SetUhdFreqHz(self.uhd_freq_mhz * 1e6));
                                }
                                ui.end_row();
                            }
                        });

                    // Defaults + Reset row.
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui.button("Defaults").clicked() {
                            self.restore_defaults();
                        }
                        if ui.small_button("Reset stats").clicked() {
                            self.reset_stats();
                        }
                    });
                });
            });
        }

        // ── Bottom message drawer ────────────────────────────────────────
        if self.msg_drawer_open {
            egui::TopBottomPanel::bottom("mobile_messages")
                .resizable(true)
                .min_height(120.0)
                .default_height(220.0)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading("Messages");
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if ui.button("✕").clicked() {
                                    self.msg_drawer_open = false;
                                }
                            },
                        );
                    });
                    self.ui_dest_and_input(ui);
                    self.ui_message_log(ui);
                });
        }

        // ── Central: spectrum + waterfall ────────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            let avail = ui.available_height();

            let spec_h = (avail * 0.45).max(80.0);
            ui.allocate_ui(egui::vec2(ui.available_width(), spec_h), |ui| {
                self.spectrum_chart.ui(ui);
            });

            let wf_h = (avail * 0.45).max(80.0);
            ui.allocate_ui(egui::vec2(ui.available_width(), wf_h), |ui| {
                self.waterfall_chart.ui(ui);
            });
        });
    }
}
