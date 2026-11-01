//! UUID JSON compatibility: validate and normalize exactly as serde's UUID type did.
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

pub fn serialize<S: Serializer>(text: &str, serializer: S) -> Result<S::Ok, S::Error> {
    let id = Uuid::parse_str(text).map_err(serde::ser::Error::custom)?;
    id.serialize(serializer)
}
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    Uuid::deserialize(deserializer).map(|id| id.to_string())
}
