/// MQTT bridge — connects a MeshNode to a Meshtastic MQTT broker.
///
/// Meshtastic nodes use MQTT topics of the form:
///   `msh/2/c/{channel_name}/{gateway_id}`   — encrypted protobuf (ServiceEnvelope)
///   `msh/2/e/{channel_name}/{gateway_id}`   — PKC encrypted (not implemented)
///
/// This module provides [`MqttBridge`] which:
/// - Subscribes to `msh/2/c/{channel}/+` to receive packets from other gateways.
/// - Publishes local TX and relayed packets to `msh/2/c/{channel}/{our_gateway_id}`.
/// - Converts between `ServiceEnvelope` (MQTT) and `MeshPacket` (serial proto).
///
/// The bridge runs on a background tokio task and communicates with the main
/// loop via `mpsc` channels.

#[cfg(feature = "mqtt")]
pub use bridge::*;

#[cfg(feature = "mqtt")]
mod bridge {
    use prost::Message as _;
    use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, QoS};
    use std::time::Duration;
    use tokio::sync::mpsc;

    use crate::mac::crypto::MeshCrypto;
    use crate::mac::packet::{BROADCAST, MeshFrame, MeshHeader};
    use crate::proto::radio::{MeshPacket, mesh_packet};
    use crate::proto::service_envelope::ServiceEnvelope;

    /// Configuration for the MQTT bridge.
    #[derive(Clone)]
    pub struct MqttConfig {
        /// Broker host (default: `mqtt.meshtastic.org`).
        pub host: String,
        /// Broker port (default: 1883).
        pub port: u16,
        /// MQTT username (default: `meshdev`).
        pub username: String,
        /// MQTT password (default: `large4cats`).
        pub password: String,
        /// Channel name used in topic path (default: `LongFast`).
        pub channel: String,
        /// Root topic prefix (default: `msh/2/c`).
        pub topic_root: String,
    }

    impl Default for MqttConfig {
        fn default() -> Self {
            Self {
                host:       "mqtt.meshtastic.org".into(),
                port:       1883,
                username:   "meshdev".into(),
                password:   "large4cats".into(),
                channel:    "LongFast".into(),
                topic_root: "msh/2/c".into(),
            }
        }
    }

    impl MqttConfig {
        /// Subscribe topic: `msh/2/c/{channel}/+`
        pub fn sub_topic(&self) -> String {
            format!("{}/{}/+", self.topic_root, self.channel)
        }

        /// Publish topic: `msh/2/c/{channel}/{gateway_id}`
        pub fn pub_topic(&self, gateway_id: u32) -> String {
            format!("{}/{}/%21{:08x}", self.topic_root, self.channel, gateway_id)
        }
    }

    /// A packet received from MQTT, ready for local processing.
    pub struct MqttRx {
        /// The MeshPacket extracted from the ServiceEnvelope.
        pub packet: MeshPacket,
        /// Gateway that relayed it.
        pub gateway_id: u32,
    }

    /// Handle to a running MQTT bridge.
    ///
    /// Created by [`spawn_mqtt_bridge`].  The caller reads from `rx` to get
    /// packets arriving from MQTT, and calls [`publish`] to send packets out.
    pub struct MqttBridge {
        /// Incoming packets from MQTT.
        pub rx: mpsc::Receiver<MqttRx>,
        client:     AsyncClient,
        pub_topic:  String,
        gateway_id: u32,
        channel_id: String,
    }

    impl MqttBridge {
        /// Publish a MeshPacket to the MQTT broker wrapped in a ServiceEnvelope.
        pub async fn publish(&self, packet: &MeshPacket) {
            let envelope = ServiceEnvelope {
                packet:     Some(packet.clone()),
                channel_id: self.channel_id.clone(),
                gateway_id: self.gateway_id,
            };
            let payload = envelope.encode_to_vec();
            let _ = self.client
                .publish(&self.pub_topic, QoS::AtLeastOnce, false, payload)
                .await;
        }

        /// Publish a raw MeshFrame (from local RX or TX) as a MeshPacket on MQTT.
        ///
        /// The frame's encrypted payload is sent as-is (we don't decrypt for MQTT).
        pub async fn publish_frame(&self, frame: &MeshFrame) {
            let h = &frame.header;
            let pkt = MeshPacket {
                from:      h.from,
                to:        h.to,
                id:        h.id,
                channel:   0,
                rx_time:   0,
                rx_snr:    0.0,
                hop_limit: h.hop_limit as u32,
                want_ack:  h.want_ack,
                rx_rssi:   0,
                payload_variant: Some(mesh_packet::PayloadVariant::Encrypted(
                    frame.payload.clone(),
                )),
            };
            self.publish(&pkt).await;
        }
    }

    /// Spawn the MQTT bridge on the current tokio runtime.
    ///
    /// Returns a [`MqttBridge`] handle for publishing and receiving.
    pub async fn spawn_mqtt_bridge(
        config:     MqttConfig,
        gateway_id: u32,
    ) -> Result<MqttBridge, String> {
        let client_id = format!("meshrs-{:08x}", gateway_id);
        let mut opts = MqttOptions::new(&client_id, &config.host, config.port);
        opts.set_credentials(&config.username, &config.password);
        opts.set_keep_alive(Duration::from_secs(30));

        let (client, mut eventloop) = AsyncClient::new(opts, 64);

        // Subscribe.
        let sub_topic = config.sub_topic();
        client.subscribe(&sub_topic, QoS::AtLeastOnce).await
            .map_err(|e| format!("mqtt subscribe: {e}"))?;

        let pub_topic  = config.pub_topic(gateway_id);
        let channel_id = config.channel.clone();

        let (tx, rx) = mpsc::channel::<MqttRx>(64);

        // Background task: read MQTT events, decode ServiceEnvelopes, forward.
        let own_gateway = gateway_id;
        tokio::spawn(async move {
            loop {
                match eventloop.poll().await {
                    Ok(Event::Incoming(Incoming::Publish(msg))) => {
                        if let Ok(env) = ServiceEnvelope::decode(msg.payload.as_ref()) {
                            // Skip our own messages.
                            if env.gateway_id == own_gateway { continue; }
                            if let Some(packet) = env.packet {
                                let _ = tx.send(MqttRx {
                                    packet,
                                    gateway_id: env.gateway_id,
                                }).await;
                            }
                        }
                    }
                    Ok(_) => {} // connack, suback, pingresp, etc.
                    Err(e) => {
                        eprintln!("[mqtt] connection error: {e}");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        });

        Ok(MqttBridge { rx, client, pub_topic, gateway_id, channel_id })
    }

    /// Convert an MQTT-received MeshPacket (with encrypted payload) into a
    /// raw byte buffer suitable for `MeshNode::process_rx_frame`.
    ///
    /// This reconstructs the OTA binary format (16-byte header + ciphertext).
    pub fn mqtt_packet_to_raw(pkt: &MeshPacket) -> Option<Vec<u8>> {
        let encrypted = match pkt.payload_variant.as_ref()? {
            mesh_packet::PayloadVariant::Encrypted(e) => e.clone(),
            mesh_packet::PayloadVariant::Decoded(_) => {
                // Decoded packets can't be fed to process_rx_frame directly
                // (it expects encrypted).  The caller should handle these
                // at the app layer.
                return None;
            }
        };

        let header = MeshHeader {
            to:           pkt.to,
            from:         pkt.from,
            id:           pkt.id,
            hop_limit:    pkt.hop_limit.min(7) as u8,
            hop_start:    pkt.hop_limit.min(7) as u8,
            want_ack:     pkt.want_ack,
            via_mqtt:     true,
            channel_hash: 0, // Not available from MQTT; process_rx_frame will
                             // need to skip the check for via_mqtt packets.
        };
        let frame = MeshFrame { header, payload: encrypted };
        Some(frame.to_bytes())
    }
}
