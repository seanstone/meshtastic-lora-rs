# Changelog

## mesh_radio — RF communications terminal

1. Auto-TX toggle + manual TX queue + text input / Send button
2. Operating mode: TwoNodeTest (two-node sim) vs Terminal (single-node, real RF)
3. Enhanced message log — timestamps, direction (TX/RX/FWD), from-ID, hops, colour coding
4. Forward handling — re-modulate + relay `Option<MeshFrame>` from `process_rx_frame`
5. Node identity config — short/long name fields in Terminal mode
6. PER enhancements — colour-coded label (green < 5%, amber < 20%, red)
7. Destination selector — broadcast or unicast to neighbour from dropdown
8. Mobile-friendly responsive layout — toolbar + collapsible settings/message drawers on narrow screens

## mesh_node — headless node

- Text mode (stdin/stdout)
- Serial protobuf mode (`--serial`) — Meshtastic framing protocol
- MQTT bridge (`--mqtt`) — ServiceEnvelope on `msh/2/c/{channel}/+`
- WebSocket server (`--ws`) — JSON commands/events on configurable port
- UHD support (`--uhd`) — USRP hardware driver for real RF

## lora-rs — PHY library

- Sync word parameterised + validated in `frame_sync` (0x12 / 0x2B)
- BW 62.5 kHz support (VeryLongSlow preset)
- Preamble length as runtime parameter
- `lora::modem` module — `Tx` / `Rx` byte↔IQ wrappers
- `lora::channel` module — `Driver` trait + `Channel` (AWGN sim)
- `lora::uhd` module — `UhdDevice` (USRP hardware driver)
- `lora::ui::SpectrumAnalyzer` — shared spectrum/waterfall engine

## Workspace

- Cargo workspace with `lora-rs` + `mesh` crates
- `meshtastic/protobufs` submodule with `prost-build` code generation
- GitHub Pages deployment (WASM build)
- GPL-3.0 license
