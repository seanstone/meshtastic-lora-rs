/// LoRa modem configuration preset.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModemPreset {
    pub name:         &'static str,
    pub sf:           u8,
    /// Bandwidth in kHz (fractional — 62.5, 125, 250, 500).
    pub bw_khz:       f32,
    /// Coding rate denominator (4 = CR 4/5, 8 = CR 4/8).
    pub cr_denom:     u8,
    /// Meshtastic sync word (0x2B for all public channels).
    pub sync_word:    u8,
    pub preamble_len: u16,
}

/// All Meshtastic modem presets, ordered from fastest to slowest.
pub const PRESETS: &[ModemPreset] = &[
    ModemPreset { name: "ShortTurbo",   sf: 7,  bw_khz: 500.0, cr_denom: 5, sync_word: 0x2B, preamble_len: 16 },
    ModemPreset { name: "ShortFast",    sf: 7,  bw_khz: 250.0, cr_denom: 5, sync_word: 0x2B, preamble_len: 16 },
    ModemPreset { name: "ShortSlow",    sf: 8,  bw_khz: 250.0, cr_denom: 5, sync_word: 0x2B, preamble_len: 16 },
    ModemPreset { name: "MediumFast",   sf: 9,  bw_khz: 250.0, cr_denom: 5, sync_word: 0x2B, preamble_len: 16 },
    ModemPreset { name: "MediumSlow",   sf: 10, bw_khz: 250.0, cr_denom: 5, sync_word: 0x2B, preamble_len: 16 },
    ModemPreset { name: "LongFast",     sf: 11, bw_khz: 250.0, cr_denom: 5, sync_word: 0x2B, preamble_len: 16 },
    ModemPreset { name: "LongModerate", sf: 11, bw_khz: 125.0, cr_denom: 8, sync_word: 0x2B, preamble_len: 16 },
    ModemPreset { name: "LongSlow",     sf: 12, bw_khz: 125.0, cr_denom: 8, sync_word: 0x2B, preamble_len: 16 },
    ModemPreset { name: "VeryLongSlow", sf: 12, bw_khz: 62.5,  cr_denom: 8, sync_word: 0x2B, preamble_len: 16 },
];

/// Default preset used when none is specified.
pub const DEFAULT_PRESET: &ModemPreset = &PRESETS[5]; // LongFast

/// Look up a preset by name (case-insensitive).
pub fn preset_by_name(name: &str) -> Option<&'static ModemPreset> {
    PRESETS.iter().find(|p| p.name.eq_ignore_ascii_case(name))
}

// ── Regions ──────────────────────────────────────────────────────────────────

/// Regional frequency plan.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Region {
    pub name:       &'static str,
    /// Start of the ISM band (MHz).
    pub freq_start: f64,
    /// End of the ISM band (MHz).
    pub freq_end:   f64,
    /// Max TX power (dBm).
    pub tx_power:   u8,
    /// Duty cycle limit (percent, 0 = no limit).
    pub duty_cycle: u8,
}

impl Region {
    /// Default channel 0 centre frequency for a given bandwidth (MHz).
    ///
    /// `freq = freq_start + bw/2`  (channel 0, no offset).
    pub fn channel_freq(&self, bw_khz: f32) -> f64 {
        self.freq_start + bw_khz as f64 / 2000.0
    }

    /// Channel N centre frequency.
    pub fn channel_n_freq(&self, bw_khz: f32, ch: u32) -> f64 {
        self.freq_start + (ch as f64 + 0.5) * bw_khz as f64 / 1000.0
    }
}

/// All supported Meshtastic regions.
///
/// Frequencies from <https://meshtastic.org/docs/overview/radio-settings/>.
pub const REGIONS: &[Region] = &[
    Region { name: "US",       freq_start: 902.0,   freq_end: 928.0,   tx_power: 30, duty_cycle: 0 },
    Region { name: "EU_868",   freq_start: 869.4,   freq_end: 869.65,  tx_power: 27, duty_cycle: 10 },
    Region { name: "EU_433",   freq_start: 433.0,   freq_end: 434.0,   tx_power: 12, duty_cycle: 10 },
    Region { name: "CN",       freq_start: 470.0,   freq_end: 510.0,   tx_power: 19, duty_cycle: 0 },
    Region { name: "JP",       freq_start: 920.8,   freq_end: 927.8,   tx_power: 16, duty_cycle: 0 },
    Region { name: "ANZ",      freq_start: 915.0,   freq_end: 928.0,   tx_power: 30, duty_cycle: 0 },
    Region { name: "KR",       freq_start: 920.0,   freq_end: 923.0,   tx_power: 14, duty_cycle: 0 },
    Region { name: "TH",       freq_start: 920.0,   freq_end: 925.0,   tx_power: 16, duty_cycle: 0 },
    Region { name: "IN",       freq_start: 865.0,   freq_end: 867.0,   tx_power: 30, duty_cycle: 0 },
    Region { name: "NZ_865",   freq_start: 864.0,   freq_end: 868.0,   tx_power: 36, duty_cycle: 0 },
    Region { name: "TW",       freq_start: 920.0,   freq_end: 925.0,   tx_power: 27, duty_cycle: 0 },
    Region { name: "RU",       freq_start: 868.7,   freq_end: 869.2,   tx_power: 20, duty_cycle: 0 },
    Region { name: "UA",       freq_start: 868.0,   freq_end: 868.6,   tx_power: 14, duty_cycle: 1 },
    Region { name: "MY_433",   freq_start: 433.0,   freq_end: 435.0,   tx_power: 20, duty_cycle: 0 },
    Region { name: "MY_919",   freq_start: 919.0,   freq_end: 924.0,   tx_power: 27, duty_cycle: 0 },
    Region { name: "SG_923",   freq_start: 920.0,   freq_end: 925.0,   tx_power: 20, duty_cycle: 0 },
];

/// Index of the default region (TW).
pub const DEFAULT_REGION_IDX: usize = 10;

/// Look up a region by name (case-insensitive).
pub fn region_by_name(name: &str) -> Option<&'static Region> {
    REGIONS.iter().find(|r| r.name.eq_ignore_ascii_case(name))
}
