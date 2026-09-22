use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Serialize, Deserialize, Clone)]
pub struct Chat {
    #[serde(skip)]
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip)]
    pub other_members: Vec<String>,
    #[serde(skip, default = "OffsetDateTime::now_utc")]
    pub last_activity: OffsetDateTime,
}

#[derive(Clone)]
pub struct Invite {
    pub proposal_id: String,
    pub chat_id: String,
    pub proposer: String,
    pub proposed: OffsetDateTime,
}

pub struct Message {
    pub id: String,
    pub sender: String,
    pub sent: OffsetDateTime,
    pub body: String,
}
