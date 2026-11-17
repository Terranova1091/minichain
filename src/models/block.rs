use serde::{Deserialize, Serialize};

use crate::models::block_header::BlockHeader;
use crate::models::transaction::Transaction;
use crate::models::block_header::MineError;
use crate::models::hash::Hash;
use crate::models::merkle::{self, MerkleError};

pub const BLOCK_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockError {
    EmptyTransactionList,
    Merkle(MerkleError),
    MerkleRootMismatch,
}

impl Block {
    pub fn new(prev_hash: Hash, height: u64, transactions: Vec<Transaction>,
        timestamp: u64, bits: u32) -> Result<Block, BlockError> {

        if transactions.is_empty() {
            return Err(BlockError::EmptyTransactionList);
        }
        let merkle_root = merkle::merkle_root(&transactions).map_err(BlockError::Merkle)?;
        let header = BlockHeader {
            version: BLOCK_VERSION,
            height,
            prev_hash,
            merkle_root,
            timestamp,
            bits,
            nonce: 0,
        };
        Ok(Block { header, transactions })
    }

    pub fn hash(&self) -> Hash {
        self.header.hash()
    }

    pub fn verify_merkle_root(&self) -> Result<(), BlockError> {
        if self.transactions.is_empty() {
            return Err(BlockError::EmptyTransactionList);
        }
        let merkle_root = merkle::merkle_root(&self.transactions).map_err(BlockError::Merkle)?;
        if merkle_root != self.header.merkle_root {
            return Err(BlockError::MerkleRootMismatch);
        }
        Ok(())
    }

    pub fn mine(&mut self) -> Result<(), MineError> {
        self.header.mine()
    }
}

// pub fn is_genesis(block: &Block) -> bool {
//     block.header.height == 0 && block.header.prev_hash == Hash([0; 32])
// }

#[cfg(test)]
mod tests {
    use super::*;

    fn tx(from: &str, to: &str, amount: u64, nonce: u64) -> Transaction {
        Transaction { from: from.to_string(), to: to.to_string(), amount, nonce }
    }

    fn sample_txs(n: usize) -> Vec<Transaction> {
        (0..n)
            .map(|i| {
                if i == 0 {
                    tx("", "miner", 50, 0)
                } else {
                    tx(&format!("from{i}"), &format!("to{i}"), i as u64, i as u64)
                }
            })
            .collect()
    }

    fn prev() -> Hash {
        Hash([0xAB; 32])
    }

    fn sample_block(n: usize) -> Block {
        Block::new(prev(), 5, sample_txs(n), 1_700_000_000, 12).unwrap()
    }

    #[test]
    fn new_fills_header_for_1_2_3_transactions() {
        for n in 1..=3 {
            let txs = sample_txs(n);
            let block = Block::new(prev(), 5, txs.clone(), 1_700_000_000, 12).unwrap();
            assert_eq!(block.header.merkle_root, merkle::merkle_root(&txs).unwrap(), "n={n}");
            assert_eq!(block.header.version, BLOCK_VERSION);
            assert_eq!(block.header.height, 5);
            assert_eq!(block.header.prev_hash, prev());
            assert_eq!(block.header.timestamp, 1_700_000_000);
            assert_eq!(block.header.bits, 12);
            assert_eq!(block.header.nonce, 0);
            assert_eq!(block.transactions, txs);
        }
    }

    #[test]
    fn new_rejects_empty_transaction_list() {
        assert_eq!(
            Block::new(prev(), 0, Vec::new(), 0, 0),
            Err(BlockError::EmptyTransactionList)
        );
    }

    #[test]
    fn fresh_block_passes_merkle_verification() {
        for n in 1..=9 {
            assert_eq!(sample_block(n).verify_merkle_root(), Ok(()), "n={n}");
        }
    }

    #[test]
    fn verify_rejects_empty_body() {
        let mut block = sample_block(3);
        block.transactions.clear();
        assert_eq!(block.verify_merkle_root(), Err(BlockError::EmptyTransactionList));
    }

    #[test]
    fn verify_detects_modified_transaction() {
        let mut block = sample_block(3);
        block.transactions[1].amount += 1;
        assert_eq!(block.verify_merkle_root(), Err(BlockError::MerkleRootMismatch));
    }

    #[test]
    fn verify_detects_modified_header_root() {
        let mut block = sample_block(3);
        block.header.merkle_root.0[0] ^= 1;
        assert_eq!(block.verify_merkle_root(), Err(BlockError::MerkleRootMismatch));
    }

    #[test]
    fn verify_detects_added_transaction() {
        let mut block = sample_block(3);
        block.transactions.push(tx("x", "y", 1, 99));
        assert_eq!(block.verify_merkle_root(), Err(BlockError::MerkleRootMismatch));
    }

    #[test]
    fn verify_detects_removed_transaction() {
        let mut block = sample_block(3);
        block.transactions.pop();
        assert_eq!(block.verify_merkle_root(), Err(BlockError::MerkleRootMismatch));
    }

    #[test]
    fn verify_detects_reordered_transactions() {
        let mut block = sample_block(3);
        block.transactions.swap(1, 2);
        assert_eq!(block.verify_merkle_root(), Err(BlockError::MerkleRootMismatch));
    }

    #[test]
    fn verify_detects_duplicated_last_transaction() {
        // [a,b,c] и [a,b,c,c] — разные блоки (ADR 0001)
        let mut block = sample_block(3);
        let last = block.transactions[2].clone();
        block.transactions.push(last);
        assert_eq!(block.verify_merkle_root(), Err(BlockError::MerkleRootMismatch));
    }

    #[test]
    fn verify_does_not_depend_on_nonce_or_timestamp() {
        let mut block = sample_block(3);
        block.header.nonce = 12345;
        block.header.timestamp += 100;
        assert_eq!(block.verify_merkle_root(), Ok(()));
    }

    #[test]
    fn block_hash_equals_header_hash() {
        let block = sample_block(2);
        assert_eq!(block.hash(), block.header.hash());
    }

    #[test]
    fn block_hash_changes_with_nonce() {
        let mut block = sample_block(2);
        let before = block.hash();
        block.header.nonce += 1;
        assert_ne!(block.hash(), before);
    }

    #[test]
    fn block_hash_changes_with_transactions_via_new() {
        let a = Block::new(prev(), 5, sample_txs(2), 1_700_000_000, 12).unwrap();
        let b = Block::new(prev(), 5, sample_txs(3), 1_700_000_000, 12).unwrap();
        assert_ne!(a.hash(), b.hash());
    }

    #[test]
    fn same_input_gives_same_block() {
        assert_eq!(sample_block(4), sample_block(4));
    }
}