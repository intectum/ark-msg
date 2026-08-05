//! ark_msg — simple messaging on top of [ark](../../ark).
//!
//! Data model:
//! - A chat is a directory under `apps/msg/chats/`, named by its id, holding
//!   a `chat.json`. Members are the dir's writer set; owners can change
//!   membership.
//! - A direct chat is a fixed pair, owned by both. A group chat holds its
//!   non-owner permissions in an 'all members' group, so members can come and
//!   go.
//! - A message is a `msg_<timestamp>.md` file inside that dir. Sender is the
//!   file's owner; all other members are readers. Ark encrypts + wraps the
//!   file key per member.

pub mod chat;
pub mod direct;
pub mod group;
pub mod invite;
pub mod message;
pub mod paths;
pub mod reltime;
pub mod tui;
pub mod types;
