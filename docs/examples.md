# Examples & Guides

Step-by-step walkthroughs for common use cases.  Assumes you have Rust
installed (`rustup`) and have cloned this repo with submodules:

```sh
git clone --recurse-submodules https://github.com/youruser/meshtastic-lora-rs
cd meshtastic-lora-rs
cargo build
```

---

## Background: what is Meshtastic?

[Meshtastic](https://meshtastic.org) is an open-source project that turns
cheap LoRa radios (typically ~$20 ESP32 + SX1262 boards like the Heltec V3
or LilyGo T-Beam) into a long-range, off-grid mesh network.

```
              5-20 km LoRa link
  [Node A] ~~~~~~~~~~~~~~~~~~~~~~~~~~~~ [Node B]
  Heltec V3          chirp spread         T-Beam
  SF11/250kHz        spectrum             SF11/250kHz
                     AES-256-CTR
                     hop_limit=3
```

Key concepts:

- **LoRa** is a chirp-spread-spectrum modulation that trades data rate for
  extreme range.  A single packet at SF11/250 kHz (the "LongFast" preset)
  carries ~30 bytes/second but can travel 5-20 km line-of-sight.

- **Mesh routing** means every node relays packets it hears.  A message
  from node A can reach node C via node B, even if A and C are out of
  direct range.  Each packet has a `hop_limit` (default 3) that is
  decremented on each relay to prevent infinite loops.

```
  [A] --RF--> [B] --RF--> [C]
  hop=3       hop=2       hop=1 (delivered)
```

- **Encryption** is mandatory.  Every packet body is AES-256-CTR encrypted
  with a channel PSK (pre-shared key).  The default public channel uses a
  well-known key (`0x01` padded to 32 bytes), so anyone can read "LongFast"
  traffic — but private channels use a random 256-bit key.

- **Channels** are identified by a name (e.g. "LongFast") and a PSK.  Nodes
  on different channels can't decode each other's packets.

- **MQTT bridging** lets a node forward mesh packets to an internet MQTT
  broker (default: `mqtt.meshtastic.org`).  This extends the mesh globally —
  a message sent on RF in Tokyo can be read by a subscriber in Berlin.

```
  [Radio A] --RF--> [Gateway B] --MQTT--> mqtt.meshtastic.org
                                              |
                                              +--> [Subscriber in Berlin]
                                              +--> [Dashboard in London]
```

This project (`meshtastic-lora-rs`) implements the Meshtastic protocol
stack in pure Rust, from the LoRa PHY layer up through mesh routing, with
multiple I/O interfaces for integration.

```
  ┌──────────────────────────────────────────────────┐
  │                meshtastic-lora-rs                 │
  │                                                  │
  │   stdin ──┐                                      │
  │   MQTT  ──┼──> MeshNode ──> Tx ──> Driver ──> RF │
  │   WS    ──┘       |                    |         │
  │                    |        Rx <── Driver         │
  │   stdout <─┐      v                              │
  │   MQTT   <─┼── process_rx                        │
  │   WS     <─┘                                     │
  └──────────────────────────────────────────────────┘
```

---

## 1. Exploring the GUI simulator

The GUI simulator runs two virtual Meshtastic nodes communicating over a
simulated AWGN (noise) channel.  No hardware needed.

```
  ┌─────────────────── mesh_sim ───────────────────────┐
  │                                                     │
  │  ┌──────────┐    AWGN Channel    ┌──────────┐      │
  │  │  Node A  │ ──── IQ + noise ──>│  Node B  │      │
  │  │ (sender) │                    │(receiver)│      │
  │  └──────────┘                    └──────────┘      │
  │       |                               |            │
  │       v                               v            │
  │  ┌─────────────────────────────────────────────┐   │
  │  │  egui GUI                                   │   │
  │  │  [spectrum] [waterfall] [message log]        │   │
  │  │  [settings: SF, gain, noise, preset]         │   │
  │  └─────────────────────────────────────────────┘   │
  └─────────────────────────────────────────────────────┘
```

```sh
cargo run
```

**What you'll see:**

- **Left panel**: modem preset selector (LongFast is default), spreading
  factor slider, TX gain / noise sliders, TX interval, pause/resume, packet
  counters (TX, RX, PER), and the two simulated nodes with their neighbour
  tables.

- **Center panel**: live RF spectrum (top), waterfall spectrogram (middle),
  and a scrolling message log (bottom).

**Things to try:**

1. **Change the noise level** — drag the "Noise" slider up towards 0 dBFS.
   Watch the PER (packet error rate) climb as the SNR drops.  Below ~5 dB
   SNR at SF11, most packets fail CRC.

2. **Change the spreading factor** — lower SF (7) = faster but shorter range.
   Higher SF (12) = slower but more robust.  The waterfall shows the chirp
   bandwidth changing.

3. **Switch presets** — try "ShortTurbo" (SF7/500 kHz) for fast, short-range
   packets, or "VeryLongSlow" (SF12/62.5 kHz) for maximum range simulation.

4. **Pause and inspect** — hit Pause, then zoom into the waterfall to see
   individual chirp symbols.

### WASM version (browser)

```sh
make wasm-serve
# Open http://localhost:3000
```

Same GUI, runs entirely in the browser.  Useful for demos and sharing.

---

## 2. Chatting in text mode

The simplest integration: a single node that loops back through the
simulated channel.

```
  ┌─────────── Terminal ───────────┐
  │                                │
  │  stdin ──> mesh_node           │
  │              |                 │
  │              v                 │
  │           MeshNode             │
  │              |                 │
  │       build_text_frame         │
  │              |                 │
  │              v                 │
  │    Tx::modulate ──> Channel    │
  │                       |        │
  │              Rx::decode <──┘   │
  │              |                 │
  │       process_rx_frame         │
  │              |                 │
  │              v                 │
  │  stdout <── "[RX] ..."        │
  └────────────────────────────────┘
```

```sh
cargo run --bin mesh_node
```

Type a message and press Enter:

```
hello from terminal 1
[TX] "hello from terminal 1"
```

In the simulated channel, the node transmits and receives its own packet
(loopback).  You'll see:

```
[RX] !a1b2c3d4: "hello from terminal 1"  (hops=3)
```

The `!a1b2c3d4` is the randomly-generated 32-bit node ID.

### Customising the node

```sh
cargo run --bin mesh_node -- --name HIKE --long "Trail Camera Node" --sf 12
```

---

## 3. Listening to the global Meshtastic MQTT network

Meshtastic nodes worldwide bridge their local RF traffic to
`mqtt.meshtastic.org`.  You can listen in without any radio hardware:

```
  ┌─────── mesh_node ───────┐     ┌──── mqtt.meshtastic.org ────┐
  │                          │     │                              │
  │  MeshNode                │     │  msh/2/c/LongFast/+         │
  │     ^                    │     │     ^           |            │
  │     |                    │     │     |           v            │
  │  process_rx_frame        │<────│  ServiceEnvelope (protobuf)  │
  │     |                    │     │     ^                        │
  │     v                    │     │     |                        │
  │  stdout: [MQTT RX] ...  │     │  [Radio nodes worldwide]     │
  │                          │     │  Portland, Tokyo, Berlin...  │
  │  stdin ──> build_frame ──│────>│                              │
  │                          │     └──────────────────────────────┘
  └──────────────────────────┘
```

```sh
cargo run --bin mesh_node -- --mqtt
```

Output:

```
[mqtt] node !e4f5a6b7  broker=mqtt.meshtastic.org:1883  topic=msh/2/c/LongFast/+
[mqtt] connected, listening...
[MQTT RX] !aabb1234: "Hello from Portland"  (hops=2)
[MQTT RX] !ccdd5678: portnum=67 len=24
```

- `portnum=1` is a text message (shown as a string)
- `portnum=3` is a GPS position
- `portnum=4` is a node info beacon
- `portnum=67` is telemetry (battery, temperature, etc.)

### Sending a message to the global network

Type a line and press Enter — it goes out over MQTT to all subscribers:

```
hello from meshtastic-rs!
[TX] "hello from meshtastic-rs!"
```

Anyone subscribed to the LongFast channel on MQTT will see it.

### Using a private MQTT broker

```sh
cargo run --bin mesh_node -- --mqtt \
  --mqtt-host broker.local \
  --mqtt-port 1883 \
  --mqtt-user myuser \
  --mqtt-pass mypass
```

---

## 4. Building a web dashboard with WebSocket

The `--ws` flag starts a WebSocket server so you can build a real-time web
dashboard, integrate with Node.js, or feed data to Home Assistant.

```
  ┌──────── mesh_node ────────┐
  │                            │
  │  MQTT <──> MeshNode        │
  │              |   ^         │
  │              v   |         │
  │           WsServer (:9001) │
  │           /    |    \      │
  └──────────/─────|─────\─────┘
            v      v      v
     ┌──────────┐ ┌─────┐ ┌────────────┐
     │ Browser  │ │ CLI │ │ Node.js    │
     │ dashboard│ │ tool│ │ automation │
     └──────────┘ └─────┘ └────────────┘

  JSON over WebSocket:
    --> { "type": "send_text", "text": "hello" }
    <-- { "type": "rx", "from": ..., "text": "reply" }
```

### Step 1: Start the node with WebSocket

```sh
cargo run --bin mesh_node -- --mqtt --ws
```

This connects to the global MQTT network AND starts a WebSocket server on
`ws://localhost:9001`.

### Step 2: Connect from JavaScript

Create a simple HTML file:

```html
<!DOCTYPE html>
<html>
<body>
<h2>Mesh Monitor</h2>
<div id="log" style="font-family:monospace; white-space:pre"></div>
<input id="msg" placeholder="Type a message..." style="width:300px">
<button onclick="send()">Send</button>

<script>
const ws = new WebSocket('ws://localhost:9001');
const log = document.getElementById('log');

ws.onmessage = e => {
  const msg = JSON.parse(e.data);
  if (msg.type === 'rx') {
    const text = msg.text || `portnum=${msg.portnum} (${msg.payload_len}B)`;
    log.textContent += `[RX] !${msg.from.toString(16)}: ${text}\n`;
  } else if (msg.type === 'tx') {
    log.textContent += `[TX] "${msg.text}"\n`;
  }
  log.scrollTop = log.scrollHeight;
};

function send() {
  const input = document.getElementById('msg');
  ws.send(JSON.stringify({ type: 'send_text', text: input.value }));
  input.value = '';
}
</script>
</body>
</html>
```

Open this file in a browser.  Messages from the mesh appear in real time,
and you can send messages back.

### Step 3: Connect from Node.js

```js
const WebSocket = require('ws');
const ws = new WebSocket('ws://localhost:9001');

ws.on('message', data => {
  const msg = JSON.parse(data);
  if (msg.type === 'rx' && msg.text) {
    console.log(`${new Date().toISOString()} !${msg.from.toString(16)}: ${msg.text}`);
  }
});

// Send a message every 60 seconds
setInterval(() => {
  ws.send(JSON.stringify({ type: 'send_text', text: 'automated ping' }));
}, 60000);
```

---

## 5. Home Assistant integration

You can bridge Meshtastic messages to Home Assistant via the WebSocket
interface or MQTT directly.

### Option A: MQTT (recommended)

```
  ┌─────── mesh_node ───────┐   ┌───── Home Assistant ──────┐
  │                          │   │                            │
  │  [global Meshtastic      │   │  Mosquitto add-on          │
  │   MQTT traffic]          │   │     |                      │
  │        |                 │   │     v                      │
  │        v                 │   │  MQTT sensor               │
  │     MeshNode ──> MQTT ───│──>│  "msh/2/c/LongFast/+"     │
  │                          │   │     |                      │
  │                          │   │     v                      │
  │                          │   │  Automations / dashboard   │
  └──────────────────────────┘   └────────────────────────────┘
```

If Home Assistant already has an MQTT broker (Mosquitto add-on), point
`mesh_node` at it:

```sh
cargo run --bin mesh_node -- --mqtt \
  --mqtt-host homeassistant.local \
  --mqtt-port 1883 \
  --mqtt-user ha_mqtt_user \
  --mqtt-pass ha_mqtt_pass \
  --mqtt-topic msh/2/c
```

Then in Home Assistant, create an MQTT sensor:

```yaml
# configuration.yaml
mqtt:
  sensor:
    - name: "Meshtastic Last Message"
      state_topic: "msh/2/c/LongFast/+"
      value_template: "{{ value }}"
```

Note: the MQTT payload is a binary protobuf (`ServiceEnvelope`), so for
full decoding you'd need a custom component or an intermediary script that
converts to JSON.

### Option B: WebSocket + Node-RED

```
  mesh_node --mqtt --ws
       |              |
       v              v
    MQTT broker    WsServer (:9001)
                      |
                      v
                   Node-RED
                      |
                      v
                 Home Assistant
                 (REST / MQTT)
```

1. Start `mesh_node` with `--mqtt --ws`
2. In Node-RED, use a WebSocket client node connecting to
   `ws://localhost:9001`
3. Parse the incoming JSON and route to Home Assistant entities

---

## 6. Sending alerts from a sensor network

Suppose you have remote sensors (weather stations, trail cameras, water
level monitors) reporting via Meshtastic.  You can capture their telemetry:

```
  [Weather Station]   [Trail Cam]   [Water Sensor]
        |                  |               |
        v                  v               v
  LoRa RF mesh (906.875 MHz, LongFast)
        |
        v
  ┌──── mesh_node --mqtt ────┐
  │                           │
  │  stdout:                  │
  │  [MQTT RX] portnum=67 .. │──> tee mesh_log.txt
  │  [MQTT RX] portnum=3  .. │──> filter script
  │  [MQTT RX] portnum=1  .. │──> alert webhook
  └───────────────────────────┘
```

```sh
# Log all mesh traffic to a file
cargo run --bin mesh_node -- --mqtt 2>/dev/null | tee mesh_log.txt
```

Or filter for specific port numbers in a script:

```sh
cargo run --bin mesh_node -- --mqtt 2>/dev/null | while read line; do
  case "$line" in
    *portnum=67*) echo "[TELEMETRY] $line" ;;
    *portnum=3*)  echo "[POSITION] $line" ;;
    *portnum=1*)  echo "[TEXT] $line" ;;
  esac
done
```

---

## 7. Setting up a USRP SDR gateway

If you have an Ettus Research USRP (B200, B210, N310, etc.), you can use it
as a Meshtastic-compatible LoRa radio.  This turns your PC into a
full-power mesh node.

```
  ┌──────── Your PC ────────────────────────────────────┐
  │                                                      │
  │  mesh_node --uhd --mqtt --ws                         │
  │     |         |        |                             │
  │     |         |        +---> WsServer (:9001)        │
  │     |         +---> mqtt.meshtastic.org              │
  │     v                                                │
  │  lora::uhd::UhdDevice                                │
  │     |                                                │
  └─────|────────────────────────────────────────────────┘
        | USB 3.0 / Ethernet
        v
  ┌──────────┐       LoRa RF         ┌──────────────┐
  │  USRP    │ ~~~~~~~~~~~~~~~~~~~>> │  Heltec V3   │
  │  B210    │ <<~~~~~~~~~~~~~~~~~~~ │  (off-shelf)  │
  │  906 MHz │       5-20 km         │  Meshtastic  │
  └──────────┘                       └──────────────┘
```

### Prerequisites

- UHD (USRP Hardware Driver) installed: `brew install uhd` (macOS) or
  `apt install libuhd-dev` (Ubuntu)
- The `uhd` feature enabled (default)
- A USRP connected via USB or Ethernet

### Step 1: Verify the USRP is visible

```sh
uhd_find_devices
```

### Step 2: Start the node

US915 band, LongFast channel (906.875 MHz is Meshtastic slot 0):

```sh
cargo run --bin mesh_node -- \
  --uhd \
  --freq 906.875 \
  --tx-gain 50 \
  --rx-gain 40 \
  --name USRP \
  --long "SDR Gateway"
```

EU868 band:

```sh
cargo run --bin mesh_node -- \
  --uhd \
  --freq 869.525 \
  --tx-gain 40 \
  --rx-gain 40
```

### Step 3: Bridge to MQTT + WebSocket

```sh
cargo run --bin mesh_node -- \
  --uhd --freq 906.875 --tx-gain 50 --rx-gain 40 \
  --mqtt \
  --ws
```

This creates a four-way bridge:

```
  stdin  <──┐         ┌──>  MQTT (internet)
             \       /
              MeshNode
             /       \
  RF (UHD) <──┘         └──>  WebSocket (dashboard)
```

Messages received on RF are published to MQTT and pushed to WebSocket
clients.  Messages from MQTT or WebSocket are transmitted on RF.

### Step 4: Use the GUI simulator with real RF

```sh
cargo run --bin mesh_sim
```

In the left panel, switch from "Sim" to "UHD", enter the frequency, and
adjust gains.  The spectrum and waterfall now show real RF — you can see
Meshtastic packets from nearby nodes as chirp sweeps on the waterfall.

---

## 8. Running in the browser (WASM)

The GUI simulator compiles to WebAssembly and runs in any modern browser.

```
  ┌──────────── Browser ──────────────┐
  │                                    │
  │  index.html + mesh_sim.wasm        │
  │     |                              │
  │     v                              │
  │  ┌───────────────────────────┐     │
  │  │  Node A ──> Channel ──>   │     │
  │  │             (AWGN sim)    │     │
  │  │         <── Channel <── B │     │
  │  └───────────────────────────┘     │
  │     |                              │
  │     v                              │
  │  [spectrum] [waterfall] [messages] │
  │                                    │
  │  No server, no RF, no internet     │
  └────────────────────────────────────┘
```

### Build and serve locally

```sh
make wasm-serve
```

Open `http://localhost:3000`.  The simulation runs entirely client-side.
Useful for:

- Demos and presentations
- Teaching LoRa modulation (watch the chirps form on the waterfall)
- Testing mesh routing logic without hardware

### Deploy to GitHub Pages

Push to the `master` branch.  The GitHub Actions workflow
(`.github/workflows/pages.yml`) automatically builds the WASM binary and
deploys to GitHub Pages.

---

## CLI reference

Full list of `mesh_node` flags:

| Flag | Default | Description |
|------|---------|-------------|
| `--name <SHORT>` | `MRST` | Short name (up to 4 characters, shown in mesh) |
| `--long <LONG>` | `meshtastic-rs` | Long name (shown in node info) |
| `--sf <7-12>` | `11` | Spreading factor |
| `--preset <name>` | — | Modem preset (e.g. `LongFast`, `ShortTurbo`) |
| `--serial` | — | Serial protobuf mode (binary stdin/stdout) |
| `--mqtt` | — | Connect to MQTT broker |
| `--mqtt-host <host>` | `mqtt.meshtastic.org` | MQTT broker hostname |
| `--mqtt-port <port>` | `1883` | MQTT broker port |
| `--mqtt-user <user>` | `meshdev` | MQTT username |
| `--mqtt-pass <pass>` | `large4cats` | MQTT password |
| `--mqtt-topic <root>` | `msh/2/c` | MQTT topic root |
| `--ws` | — | Start WebSocket server |
| `--ws-port <port>` | `9001` | WebSocket server port |
| `--uhd` | — | Use USRP hardware |
| `--freq <MHz>` | `906.875` | UHD center frequency |
| `--args <str>` | `""` | UHD device args |
| `--tx-gain <dB>` | `40` | UHD TX gain |
| `--rx-gain <dB>` | `40` | UHD RX gain |
| `--signal <dBFS>` | `-20` | Sim signal level |
| `--noise <dBFS>` | `-60` | Sim noise floor |

Flags are combinable.  For example, `--mqtt --ws --uhd` creates a
four-way bridge between stdin, MQTT, WebSocket, and RF.

---

## Meshtastic frequency reference

Common Meshtastic frequencies by region:

| Region | Band | Default freq | Notes |
|--------|------|-------------|-------|
| US/CA | 915 MHz ISM | 906.875 MHz | Slot 0, 27 dBm max |
| EU | 868 MHz ISM | 869.525 MHz | 1% duty cycle, 14 dBm ERP |
| AU/NZ | 915 MHz ISM | 916.0 MHz | |
| JP | 920 MHz | 923.2 MHz | |
| CN | 470 MHz | 470.0 MHz | |
| IN | 865 MHz | 865.0 MHz | |

The `--freq` flag accepts any frequency in MHz.  Make sure your antenna and
local regulations match.
