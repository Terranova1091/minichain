use serde::{Deserialize, Serialize};

use crate::models::common_errors::CodecError;

pub const COINBASE_REWARD: u64 = 50;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transaction {
    pub from: String,
    pub to: String,
    pub amount: u64,
    pub nonce: u64,
}

impl Transaction {
    pub fn to_bytes(&self) -> Result<Vec<u8>, CodecError> {
        let mut buffer = Vec::new();
        let from_len = u16::try_from(self.from.len())
            .map_err(|_| CodecError::StringTooLong {
                len: self.from.len(),
            })?
            .to_be_bytes();
        let to_len = u16::try_from(self.to.len())
            .map_err(|_| CodecError::StringTooLong { len: self.to.len() })?
            .to_be_bytes();

        buffer.extend_from_slice(&from_len);
        buffer.extend_from_slice(self.from.as_bytes());
        buffer.extend_from_slice(&to_len);
        buffer.extend_from_slice(self.to.as_bytes());
        buffer.extend_from_slice(&self.amount.to_be_bytes());
        buffer.extend_from_slice(&self.nonce.to_be_bytes());

        Ok(buffer)
    }

    // pub fn hash(&self) -> Hash {
    //     crate::models::hash::sha256d(&self.to_bytes().unwrap())
    // }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transaction_encode_correct_transaction() {
        let input_transactions = vec![
            Transaction {
                from: "A".to_string(),
                to: "B".to_string(),
                amount: 5,
                nonce: 0,
            },
            Transaction {
                from: "".to_string(),
                to: "M".to_string(),
                amount: 50,
                nonce: 0,
            },
            Transaction {
                from: "A".to_string(),
                to: "B".to_string(),
                amount: 0x0102030405060708,
                nonce: 1,
            },
        ];

        let expected_bytes = vec![
            vec![
                0, 1, 65, 0, 1, 66, 0, 0, 0, 0, 0, 0, 0, 5, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
            vec![
                0, 0, 0, 1, 77, 0, 0, 0, 0, 0, 0, 0, 50, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
            vec![
                0, 1, 65, 0, 1, 66, 1, 2, 3, 4, 5, 6, 7, 8, 0, 0, 0, 0, 0, 0, 0, 1,
            ],
        ];

        for (input_transaction, expected_byte) in input_transactions.into_iter().zip(expected_bytes)
        {
            assert_eq!(input_transaction.to_bytes().unwrap(), expected_byte);
        }
    }

    #[test]
    fn test_to_bytes_key_properties() {
        let unequals_bytes = vec![
            Transaction {
                from: "AB".to_string(),
                to: "C".to_string(),
                amount: 0,
                nonce: 0,
            },
            Transaction {
                from: "C".to_string(),
                to: "AB".to_string(),
                amount: 0,
                nonce: 0,
            },
        ];

        let equels_bytes = Transaction {
            from: "AB".to_string(),
            to: "C".to_string(),
            amount: 0,
            nonce: 0,
        };

        assert_ne!(
            unequals_bytes[0].to_bytes().unwrap(),
            unequals_bytes[1].to_bytes().unwrap()
        );
        assert_eq!(
            equels_bytes.to_bytes().unwrap(),
            equels_bytes.to_bytes().unwrap()
        );
    }
}
