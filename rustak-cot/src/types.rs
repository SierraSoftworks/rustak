//! CoT `type` and `how` vocabularies, and the control-message predicate.
//!
//! The constants are the exact strings TAK Server and ATAK match on; the
//! grouping into modules is ours.

/// `event/@type` values rustak produces or reacts to.
pub mod cot_type {
    /// Client keepalive. Dropped without a point, answered with [`PONG`].
    pub const PING: &str = "t-x-c-t";
    /// Keepalive reply. CloudTAK treats the first one as "connection open".
    pub const PONG: &str = "t-x-c-t-r";
    /// Server protocol announcement (`<TakControl>` with the supported set).
    pub const TAKP_V: &str = "t-x-takp-v";
    /// Client protocol request (`<TakRequest version="1"/>`).
    pub const TAKP_Q: &str = "t-x-takp-q";
    /// Server protocol response (`<TakResponse status="true"/>`).
    pub const TAKP_R: &str = "t-x-takp-r";
    /// Peer went away; carries `<link relation="p-p" uid= type=/>`.
    pub const DISCONNECT: &str = "t-x-d-d";
    /// The recipient's channel membership changed; re-fetch groups.
    pub const GROUP_CHANGE: &str = "t-x-g-c";
    /// Subscription control (XPath filter / outbound connection request).
    pub const SUBSCRIBE: &str = "t-b";
    /// Geospatial filter control message.
    pub const FILTER: &str = "t-x-c-f";
    /// Client device metrics.
    pub const METRICS: &str = "t-x-c-m";
    /// Enable incognito mode for the sending subscription.
    pub const INCOGNITO_ON: &str = "t-x-c-i-e";
    /// Disable incognito mode for the sending subscription.
    pub const INCOGNITO_OFF: &str = "t-x-c-i-d";
    /// Mission content change.
    pub const MISSION_CHANGE: &str = "t-x-m-c";
    /// Mission log change.
    pub const MISSION_LOG_CHANGE: &str = "t-x-m-c-l";
    /// Mission created.
    pub const MISSION_CREATE: &str = "t-x-m-n";
    /// Mission deleted.
    pub const MISSION_DELETE: &str = "t-x-m-d";
    /// Mission invitation.
    pub const MISSION_INVITE: &str = "t-x-m-i";
    /// Mission role change (carries `mission/@type="INVITE"`, not `ROLE`).
    pub const MISSION_ROLE_CHANGE: &str = "t-x-m-r";
    /// File-share pointer (`<fileshare>` + `<ackrequest>`).
    pub const FILESHARE: &str = "b-f-t-r";
    /// File-share acknowledgement (`<ackresponse>`).
    pub const FILESHARE_ACK: &str = "b-f-t-a";
    /// GeoChat message.
    pub const CHAT: &str = "b-t-f";
    /// GeoChat delivery receipt.
    pub const CHAT_DELIVERED: &str = "b-t-f-d";
    /// GeoChat read receipt.
    pub const CHAT_READ: &str = "b-t-f-r";
    /// GeoChat pending receipt.
    pub const CHAT_PENDING: &str = "b-t-f-p";
    /// GeoChat delivery failure, sent back to the originator.
    pub const CHAT_FAILED: &str = "b-t-f-s";
}

/// `event/@how` values rustak produces.
pub mod how {
    /// Machine-generated, GPS derived. ATAK's own pings use this.
    pub const M_G: &str = "m-g";
    /// Human entered, estimated. File-share pointers use this.
    pub const H_E: &str = "h-e";
    /// Human generated, "in-group other" — the catch-all for server messages.
    pub const H_G_I_G_O: &str = "h-g-i-g-o";
}

/// Types the server consumes and never relays.
///
/// Lifted verbatim from TAK Server's control set; a message whose type matches
/// one of these (ignoring case) is handled locally and dropped from the broker
/// path, and never gets a flow tag.
pub const CONTROL_TYPES: &[&str] = &[
    "t-b",
    "t-b-a",
    "t-b-c",
    "t-b-q",
    cot_type::FILTER,
    cot_type::PING,
    cot_type::PONG,
    cot_type::TAKP_Q,
    cot_type::METRICS,
    cot_type::INCOGNITO_ON,
    cot_type::INCOGNITO_OFF,
];

/// Whether a type is consumed by the server instead of relayed.
///
/// TAK Server lower-cases before the lookup, so `T-X-C-T` is classified as
/// control (and dropped) even though its dispatch falls through to the no-op
/// branch. We match that classification exactly.
#[must_use]
pub fn is_control_type(cot_type: &str) -> bool {
    CONTROL_TYPES
        .iter()
        .any(|known| known.eq_ignore_ascii_case(cot_type))
}

/// Whether a type is an *atom* — a physical object in the CoT type hierarchy.
///
/// Atoms are the `a-…` subtree: friendly/hostile/neutral units, the things a
/// client draws on the map and the only things the latest-SA cache keeps.
#[must_use]
pub fn is_atom(cot_type: &str) -> bool {
    cot_type == "a" || cot_type.starts_with("a-")
}

/// Whether a type is a GeoChat message or receipt (`b-t-f…`).
#[must_use]
pub fn is_chat(cot_type: &str) -> bool {
    cot_type.starts_with(cot_type::CHAT)
}

/// Whether a type is a mission notification (`t-x-m-…`).
#[must_use]
pub fn is_mission_notice(cot_type: &str) -> bool {
    cot_type.starts_with("t-x-m-")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("t-x-c-t", true)]
    #[case("T-X-C-T", true)]
    #[case("t-x-c-t-r", true)]
    #[case("t-x-takp-q", true)]
    #[case("t-b", true)]
    #[case("t-b-a", true)]
    #[case("t-x-c-i-e", true)]
    #[case("t-x-takp-v", false)]
    #[case("t-x-takp-r", false)]
    #[case("t-x-d-d", false)]
    #[case("a-f-G-U-C", false)]
    #[case("", false)]
    fn control_classification_matches_tak_server(#[case] cot_type: &str, #[case] control: bool) {
        assert_eq!(is_control_type(cot_type), control, "{cot_type}");
    }

    #[rstest]
    #[case("a-f-G-U-C", true)]
    #[case("a", true)]
    #[case("a-h-A", true)]
    #[case("ab-f", false)]
    #[case("b-t-f", false)]
    #[case("t-x-c-t", false)]
    fn atoms_are_the_a_subtree(#[case] cot_type: &str, #[case] atom: bool) {
        assert_eq!(is_atom(cot_type), atom, "{cot_type}");
    }

    #[test]
    fn chat_and_mission_families_are_prefix_matched() {
        assert!(is_chat(cot_type::CHAT));
        assert!(is_chat(cot_type::CHAT_READ));
        assert!(!is_chat("b-f-t-r"));
        assert!(is_mission_notice(cot_type::MISSION_CHANGE));
        assert!(is_mission_notice(cot_type::MISSION_LOG_CHANGE));
        assert!(!is_mission_notice("t-x-c-t"));
    }

    #[test]
    fn the_control_set_is_exactly_the_eleven_tak_server_types() {
        assert_eq!(CONTROL_TYPES.len(), 11);
        assert!(!CONTROL_TYPES.contains(&cot_type::DISCONNECT));
        assert!(!CONTROL_TYPES.contains(&cot_type::TAKP_V));
    }
}
