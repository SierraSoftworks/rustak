//! Generated TAK Protocol v1 types (see `build.rs` and
//! `proto/tak_protocol_v1.proto`), plus the thin wrappers around them.

#![allow(clippy::all)] // generated code is not ours to lint

include!(concat!(env!("OUT_DIR"), "/rustak.cot.v1.rs"));

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    /// Proves the protox/prost-build pipeline: a `TakMessage` survives an
    /// encode/decode round trip with every field intact, without `protoc`
    /// having been available anywhere in the build.
    #[test]
    fn tak_message_round_trips_through_the_wire_format() {
        let message = TakMessage {
            tak_control: Some(TakControl {
                min_proto_version: 1,
                max_proto_version: 2,
                contact_uid: "TEST-CONTACT-UID".to_string(),
            }),
            cot_event: Some(CotEvent {
                r#type: "a-f-G-U-C".to_string(),
                access: String::new(),
                qos: String::new(),
                opex: String::new(),
                uid: "TEST-UID".to_string(),
                send_time: 1_726_000_000_000,
                start_time: 1_726_000_000_000,
                stale_time: 1_726_000_060_000,
                how: "m-g".to_string(),
                lat: 51.5074,
                lon: -0.1278,
                hae: 35.0,
                ce: 9999999.0,
                le: 9999999.0,
                detail: Some(Detail {}),
                caveat: String::new(),
                releasable_to: String::new(),
            }),
        };

        let bytes = message.encode_to_vec();
        let decoded = TakMessage::decode(bytes.as_slice()).expect("decode what we just encoded");

        assert_eq!(decoded, message);
    }
}
