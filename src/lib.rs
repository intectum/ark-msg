//! ark_msg — simple messaging on top of [ark](../../ark).
//!
//! Data model:
//! - A conversation is a directory under `apps/msg/convos/`. Members are the
//!   dir's writer set; owners can change membership.
//! - A message is a `.md` file inside that dir. Sender is the file's owner;
//!   all other members are readers. Ark encrypts + wraps the file key per
//!   member.

pub mod convo;
pub mod message;
pub mod paths;
pub mod sync;
