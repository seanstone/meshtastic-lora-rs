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
