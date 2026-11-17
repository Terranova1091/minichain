use crate::models::{
    transaction::Transaction, chain::StorageError, account::State, chain::atomic_write_json, chain::VersionOnly
};

use thiserror::Error;
use std::{fs, io, path::Path};
use serde::{Deserialize, Serialize};

const MEMPOOL_FORMAT_VERSION: u32 = 1;

pub struct Mempool { txs: Vec<Transaction> }

#[derive(Debug, Error)]
pub enum MempoolError {
    #[error("coinbase transactions cannot be submitted")]
    CoinbaseNotAllowed,
    #[error("unknown sender or insufficient balance: available {available}, need {needed}")]
    InsufficientBalance { available: u64, needed: u64 },
    #[error("bad nonce: expected {expected}, got {got}")]
    BadNonce { expected: u64, got: u64 },
    #[error("amount overflow")]                          
    Overflow,
    #[error(transparent)]
    Storage(#[from] StorageError), 
}

#[derive(Serialize)]
struct MempoolFileRef<'a> {
    format_version: u32,
    transactions: &'a [Transaction],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MempoolFile {
    //format_version: u32,
    transactions: Vec<Transaction>,
}

impl Mempool {
    pub fn new() -> Self {
        Mempool { txs: vec![] }
    }

    pub fn txs(&self) -> &[Transaction] {
        &self.txs
    }

    // pub fn len(&self) -> usize {
    //     self.txs.len()
    // }

    // pub fn is_empty(&self) -> bool {
    //     self.txs.is_empty()
    // }

    // проверяет транзакцию с учётом уже стоящих в очереди
    pub fn add(&mut self, tx: Transaction, state: &State) -> Result<(), MempoolError> {
        if tx.from.is_empty() {
            return Err(MempoolError::CoinbaseNotAllowed);
        }
        let pending_count = self.txs.iter()
            .filter(|x| x.from == tx.from)
            .count() as u64;
        let pending_sum: u64 = self.txs.iter()
            .filter(|x| x.from == tx.from)
            .try_fold(0u64, |acc, x| {
                acc.checked_add(x.amount)
                    .ok_or(MempoolError::Overflow)
            })?;
            
        let expected_nonce = state.next_nonce(&tx.from).checked_add(pending_count).ok_or(MempoolError::Overflow)?;
        if tx.nonce != expected_nonce {
            return Err(MempoolError::BadNonce { expected: expected_nonce, got: tx.nonce })
        }
        let current_txs_sum = tx.amount.checked_add(pending_sum).ok_or(MempoolError::Overflow)?;
        if state.balance(&tx.from) < current_txs_sum {
            return Err(MempoolError::InsufficientBalance { available: state.balance(&tx.from).saturating_sub(pending_sum), needed: tx.amount });
        }
        self.txs.push(tx);
        Ok(())
    }

    pub fn revalidate(&mut self, state: &State) -> Vec<Transaction> {
        let old = std::mem::take(&mut self.txs);  
        let mut dropped = Vec::new();
        for tx in old {
            if self.add(tx.clone(), state).is_err() {
                dropped.push(tx);
            }
        }
        dropped
    }

    pub fn load(path: &Path) -> Result<Mempool, StorageError> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Mempool::new()),
            Err(e) => return Err(e.into()),
        };

        let version: VersionOnly = serde_json::from_str(&text)?;
        if version.format_version != MEMPOOL_FORMAT_VERSION {
            return Err(StorageError::UnsupportedVersion {
                found: version.format_version,
                expected: MEMPOOL_FORMAT_VERSION,
            });
        }

        let file: MempoolFile = serde_json::from_str(&text)?;
        Ok(Mempool { txs: file.transactions })
    }

    /// Атомарно перезаписывает файл. Пустая очередь пишется как пустой список
    pub fn save(&self, path: &Path) -> Result<(), StorageError> {
        let file = MempoolFileRef {
            format_version: MEMPOOL_FORMAT_VERSION,
            transactions: &self.txs,
        };
        atomic_write_json(&file, path)
    }
}