//! `<__chat>` and `<remarks>` — GeoChat.
//!
//! GeoChat is ordinary routed CoT: `b-t-f` with a `<__chat>` element naming
//! the conversation and a sibling `<remarks>` holding the text. Direct
//! messages are addressed with `<marti><dest callsign=…>`; room messages carry
//! no `<marti>` at all.
//!
//! ATAK identifies a message as chat purely from its `b-t-f` type prefix, so
//! `<__chat>` is genuinely optional on the wire.

use super::{Detail, Element, Node, TypedDetail, apply_extras, extra_attrs};

/// The conversation every client shows by default.
pub const DEFAULT_CHATROOM: &str = "All Chat Rooms";

/// The legacy name for [`DEFAULT_CHATROOM`], still normalised on receive.
pub const LEGACY_CHATROOM: &str = "All Streaming";

/// `<__chat>` — which conversation a message belongs to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Chat {
    /// Conversation id. For a direct message this is the peer's uid.
    pub id: Option<String>,
    /// Display name of the conversation.
    pub chatroom: Option<String>,
    /// Callsign of the sender, falling back to their uid.
    pub sender_callsign: Option<String>,
    /// Whether the sender owns the group conversation.
    pub group_owner: bool,
    /// Unique id of this message, used to match receipts.
    pub message_id: Option<String>,
    /// Parent conversation, for threaded rooms.
    pub parent: Option<String>,
    /// The `<chatgrp>` child's attributes in document order (`id`, `uid0`, …).
    pub chatgrp: Vec<(String, String)>,
    /// Attributes we do not model, preserved in document order.
    pub extra: Vec<(String, String)>,
}

impl Chat {
    const KNOWN: &'static [&'static str] = &[
        "id",
        "chatroom",
        "senderCallsign",
        "groupOwner",
        "messageId",
        "parent",
    ];

    /// The participant uids listed by `<chatgrp>`, in order, without repeats.
    ///
    /// `uid0` is the sender; the rest are the recipients.
    #[must_use]
    pub fn participants(&self) -> Vec<&str> {
        let mut seen: Vec<&str> = Vec::new();
        for (name, value) in &self.chatgrp {
            if name.starts_with("uid") && !seen.contains(&value.as_str()) {
                seen.push(value);
            }
        }
        seen
    }

    /// The conversation display name with the legacy alias normalised.
    #[must_use]
    pub fn room(&self) -> &str {
        match self.chatroom.as_deref() {
            Some(LEGACY_CHATROOM) | None => DEFAULT_CHATROOM,
            Some(room) => room,
        }
    }
}

impl TypedDetail for Chat {
    const NAME: &'static str = "__chat";

    fn from_element(element: &Element) -> Option<Self> {
        let attr = |name: &str| element.get(name).map(ToOwned::to_owned);
        Some(Self {
            id: attr("id"),
            chatroom: attr("chatroom"),
            sender_callsign: attr("senderCallsign"),
            group_owner: element
                .get("groupOwner")
                .is_some_and(|value| value.eq_ignore_ascii_case("true")),
            message_id: attr("messageId"),
            parent: attr("parent"),
            chatgrp: element
                .child("chatgrp")
                .map(|child| child.attrs.clone())
                .unwrap_or_default(),
            extra: extra_attrs(element, Self::KNOWN),
        })
    }

    fn to_element(&self) -> Element {
        let mut element = Element::new(Self::NAME)
            .attr_opt("id", self.id.clone())
            .attr_opt("chatroom", self.chatroom.clone())
            .attr_opt("senderCallsign", self.sender_callsign.clone())
            .attr("groupOwner", self.group_owner.to_string())
            .attr_opt("messageId", self.message_id.clone())
            .attr_opt("parent", self.parent.clone());
        apply_extras(&mut element, &self.extra);
        if !self.chatgrp.is_empty() {
            let mut chatgrp = Element::new("chatgrp");
            chatgrp.attrs.clone_from(&self.chatgrp);
            element.push(chatgrp);
        }
        element
    }
}

/// `<remarks>` — the message body.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Remarks {
    /// Originating subsystem, e.g. `BAO.F.ATAK.<uid>`.
    pub source: Option<String>,
    /// Recipient. Present on direct messages only, never on room chat.
    pub to: Option<String>,
    /// When the message was composed.
    pub time: Option<String>,
    /// The message text.
    pub text: String,
}

impl TypedDetail for Remarks {
    const NAME: &'static str = "remarks";

    fn from_element(element: &Element) -> Option<Self> {
        Some(Self {
            source: element.get("source").map(ToOwned::to_owned),
            to: element.get("to").map(ToOwned::to_owned),
            time: element.get("time").map(ToOwned::to_owned),
            text: element.text(),
        })
    }

    fn to_element(&self) -> Element {
        let mut element = Element::new(Self::NAME)
            .attr_opt("source", self.source.clone())
            .attr_opt("to", self.to.clone())
            .attr_opt("time", self.time.clone());
        if !self.text.is_empty() {
            element.push(Node::Text(self.text.clone()));
        }
        element
    }
}

/// The first `<remarks>` child of a detail.
#[must_use]
pub fn remarks(detail: &Detail) -> Option<Remarks> {
    detail.get::<Remarks>()
}

/// The uid ATAK builds for a chat message: `GeoChat.<sender>.<id>.<messageId>`.
#[must_use]
pub fn chat_uid(sender_uid: &str, conversation_id: &str, message_id: &str) -> String {
    format!("GeoChat.{sender_uid}.{conversation_id}.{message_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element() -> Element {
        Element::new("__chat")
            .attr("id", "CONV-1")
            .attr("chatroom", "All Chat Rooms")
            .attr("senderCallsign", "ALPHA")
            .attr("groupOwner", "false")
            .attr("messageId", "MSG-1")
            .with(
                Element::new("chatgrp")
                    .attr("id", "CONV-1")
                    .attr("uid0", "UID-A")
                    .attr("uid1", "UID-B")
                    .attr("uid2", "UID-A"),
            )
    }

    #[test]
    fn reads_the_attributes_and_the_chatgrp_child() {
        let chat = Chat::from_element(&element()).unwrap();
        assert_eq!(chat.id.as_deref(), Some("CONV-1"));
        assert_eq!(chat.sender_callsign.as_deref(), Some("ALPHA"));
        assert!(!chat.group_owner);
        assert_eq!(chat.participants(), vec!["UID-A", "UID-B"]);
        assert_eq!(chat.room(), DEFAULT_CHATROOM);
    }

    #[test]
    fn writes_back_the_same_shape() {
        let chat = Chat::from_element(&element()).unwrap();
        let written = chat.to_element();
        assert_eq!(written.get("messageId"), Some("MSG-1"));
        assert_eq!(written.child("chatgrp").unwrap().attrs.len(), 4);
        assert_eq!(Chat::from_element(&written), Some(chat));
    }

    #[test]
    fn the_legacy_room_name_is_normalised() {
        let mut chat = Chat::from_element(&element()).unwrap();
        chat.chatroom = Some(LEGACY_CHATROOM.into());
        assert_eq!(chat.room(), DEFAULT_CHATROOM);
        chat.chatroom = None;
        assert_eq!(chat.room(), DEFAULT_CHATROOM);
        chat.chatroom = Some("Recon".into());
        assert_eq!(chat.room(), "Recon");
    }

    #[test]
    fn group_owner_is_read_case_insensitively() {
        let element = Element::new("__chat").attr("groupOwner", "TRUE");
        assert!(Chat::from_element(&element).unwrap().group_owner);
    }

    #[test]
    fn remarks_carry_the_text_and_the_direct_message_marker() {
        let mut detail = Detail::new();
        detail.push(
            Element::new("remarks")
                .attr("source", "BAO.F.ATAK.UID-A")
                .attr("to", "UID-B")
                .with(Node::Text("on my way".into())),
        );
        let found = remarks(&detail).unwrap();
        assert_eq!(found.text, "on my way");
        assert_eq!(found.to.as_deref(), Some("UID-B"));
        assert_eq!(found.to_element().text(), "on my way");
    }

    #[test]
    fn remarks_concatenate_cdata_bodies() {
        let element = Element::new("remarks").with(Node::CData("<b>bold</b>".into()));
        assert_eq!(Remarks::from_element(&element).unwrap().text, "<b>bold</b>");
    }

    #[test]
    fn chat_uids_follow_the_atak_layout() {
        assert_eq!(
            chat_uid("UID-A", "CONV-1", "MSG-1"),
            "GeoChat.UID-A.CONV-1.MSG-1"
        );
    }
}
