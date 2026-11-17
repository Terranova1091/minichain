use sha2::{Digest, Sha256};
use std::fmt;
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Hash(pub [u8; 32]);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HashParseError {
    WrongLength { got: usize },
    InvalidChar { position: usize },
}

impl fmt::Display for HashParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HashParseError::WrongLength { got } => {
                write!(f, "Invalid hash length: expected 64 chars, got {}", got)
            }
            HashParseError::InvalidChar { position } => {
                write!(f, "Invalid hex character at position {}", position)
            }
        }
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl Serialize for Hash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Hash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Hash::from_str(&s).map_err(de::Error::custom)
    }
}

impl FromStr for Hash {
    type Err = HashParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some((index, _ch)) = s.char_indices().find(|(_, c)| !c.is_ascii_hexdigit()) {
            return Err(HashParseError::InvalidChar {position: index});
        }
        if s.len() != 64 {
            return Err(HashParseError::WrongLength {got: s.len()});
        }
        let mut out = [0u8; 32];
        for (i, pair) in s.as_bytes().chunks_exact(2).enumerate() {
            let hi = hex_value(pair[0])
                .ok_or(HashParseError::InvalidChar {position: 2 * i})?;
            let lo = hex_value(pair[1])
                .ok_or(HashParseError::InvalidChar {position: 2 * i + 1})?;
            out[i] = (hi << 4) | lo;
        }
        Ok(Hash(out))
    }
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

pub fn from_hash_to_bytes(hash: &str) -> Result<Hash, HashParseError> {
    if hash.len() != 64 {
        return Err(HashParseError::WrongLength { got: hash.len() });
    }

    let mut bytes = [0u8; 32];

    for (i, chunk) in hash.as_bytes().chunks(2).enumerate() {
        if !chunk.iter().all(|&b| b.is_ascii_hexdigit()) {
            let invalid_pos = if !chunk[0].is_ascii_hexdigit() {
                i * 2
            } else {
                i * 2 + 1
            };
            return Err(HashParseError::InvalidChar {
                position: invalid_pos,
            });
        }

        let hex_str = std::str::from_utf8(chunk).unwrap();
        bytes[i] = u8::from_str_radix(hex_str, 16).unwrap();
    }

    Ok(Hash(bytes))
}

pub fn to_hex(hash: &Hash) -> String {
    hash.0
        .iter()
        .map(|byte| format!("{:02x}", byte))
        .collect::<Vec<String>>()
        .join("")
}

pub fn sha256d(data: &[u8]) -> Hash {
    let hash_after_first_round = sha256(data);
    let hash_after_second_round = sha256(&hash_after_first_round);
    Hash(hash_after_second_round)
}

fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_hash_to_bytes_correct_input_correct_output() {
        let hash_str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let expected_hash = Hash([
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
            0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67,
            0x89, 0xab, 0xcd, 0xef,
        ]);
        let result = from_hash_to_bytes(hash_str).unwrap();
        assert_eq!(result, expected_hash);
    }

    #[test]
    fn test_from_hash_to_bytes_correct_input_edge_cases() {
        let test_cases = vec![
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            "0000000000000000000000000000000000000000000000000000000000000000",
        ];

        for hash in test_cases {
            let result = from_hash_to_bytes(hash);
            assert!(result.is_ok());
        }
    }

    #[test]
    fn test_from_hash_to_bytes_incorrect_input() {
        let test_cases = vec![
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde", // < 64
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdeff", // > 64
            "",                                                                // empty
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdefg", // invalid 'g'
            "0",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde!", // invalid '!'
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde+", // invalid '+'
            "+123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef", // invalid '+' first
        ];

        for hash in test_cases {
            let result = from_hash_to_bytes(hash);
            assert!(result.is_err(), "Expected error for hash: {}", hash);
        }
    }

    #[test]
    fn test_from_bytes_to_hash_correct_input_correct_output() {
        let bytes = Hash([
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
            0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67,
            0x89, 0xab, 0xcd, 0xef,
        ]);
        let expected_hash_str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

        let result = to_hex(&bytes);
        assert_eq!(result, expected_hash_str);
    }

    #[test]
    fn test_cycle_of_transformations() {
        let input_hash = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

        // Строка -> Hash -> Строка
        let hash = from_hash_to_bytes(input_hash).unwrap();
        let hash_back = to_hex(&hash);

        assert_eq!(hash_back, input_hash);
    }
}
