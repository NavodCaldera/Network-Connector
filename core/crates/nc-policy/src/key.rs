use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

/// Authenticated 32-byte public device key, encoded as exactly 64 hex characters.
/// Parsing validates encoding/length, not identity authenticity or signature validity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PeerKey([u8; 32]);

impl PeerKey {
    /// Wraps an already authenticated public key; this does not authenticate it.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    /// Returns the original public key bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl Serialize for PeerKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut encoded = String::with_capacity(64);
        for byte in self.0 {
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 15)]));
        }
        serializer.serialize_str(&encoded)
    }
}

fn nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

impl<'de> Deserialize<'de> for PeerKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text.len() != 64 {
            return Err(de::Error::custom(
                "Device keys must contain exactly 64 hexadecimal characters.",
            ));
        }
        let mut bytes = [0; 32];
        for (output, pair) in bytes.iter_mut().zip(text.as_bytes().as_chunks::<2>().0) {
            let high = nibble(pair[0]).ok_or_else(|| {
                de::Error::custom("Device keys must contain only hexadecimal characters.")
            })?;
            let low = nibble(pair[1]).ok_or_else(|| {
                de::Error::custom("Device keys must contain only hexadecimal characters.")
            })?;
            *output = (high << 4) | low;
        }
        Ok(Self(bytes))
    }
}
