use std::path::{Path, PathBuf};

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::models::{
    account::{State, StateError},
    block::{Block, BlockError, BLOCK_VERSION},
    transaction::Transaction,
    transaction::COINBASE_REWARD,
    block_header::{genesis, is_retarget_height, next_bits, RETARGET_INTERVAL, TARGET_BLOCK_TIME_SECS},
};

const MEDIAN_WINDOW: usize = 11;
const MAX_FUTURE_SECS: u64 = 120;
const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BlockValidationError {
    #[error("Empty chain")]
    EmptyChain,

    #[error("Wrong block version: expected {expected}, got {got}")]
    WrongVersion { expected: u32, got: u32 },

    #[error("Wrong block height: expected {expected}, got {got}")]
    WrongHeight { expected: u64, got: u64 },

    #[error("Previous hash mismatch")]
    PrevHashMismatch,

    #[error("Wrong bits (difficulty): expected {expected}, got {got}")]
    WrongBits { expected: u32, got: u32 },

    #[error("Invalid block: {0:?}")]
    Block(BlockError),

    #[error("Insufficient proof of work")]
    InsufficientWork,

    #[error("Timestamp is too old: median is {median}, but got {got}")]
    TimestampTooOld { median: u64, got: u64 },

    #[error("Timestamp is in the future: limit is {limit}, but got {got}")]
    TimestampInFuture { limit: u64, got: u64 },

    #[error("Invalid transactions: {0:?}")]
    State(StateError),
}

impl From<BlockError> for BlockValidationError {
    fn from(e: BlockError) -> Self {
        BlockValidationError::Block(e)
    }
}

impl From<StateError> for BlockValidationError {
    fn from(e: StateError) -> Self {
        BlockValidationError::State(e)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ChainError {
    #[error("Empty chain")]
    EmptyChain,

    #[error("Invalid genesis block")]
    InvalidGenesis,

    #[error("Invalid block at height {height}: {reason}")]
    InvalidBlock { height: u64, reason: BlockValidationError },

    #[error("Chain state does not match the state replayed from blocks")]
    StateMismatch,
}

/// Медиана меток времени последних MEDIAN_WINDOW блоков (верхняя медиана).
pub fn median_time_past(blocks: &[Block]) -> u64 {
    let start = blocks.len().saturating_sub(MEDIAN_WINDOW);
    let mut times: Vec<u64> = blocks[start..].iter().map(|b| b.header.timestamp).collect();
    times.sort_unstable();
    times.get(times.len() / 2).copied().unwrap_or(0)
}

/// `bits`, которые обязан иметь блок, идущий после `blocks`.
pub fn expected_bits(blocks: &[Block]) -> Option<u32> {
    let prev = blocks.last()?;
    let height = u64::try_from(blocks.len()).ok()?;
    if !is_retarget_height(height) {
        return Some(prev.header.bits);
    }
    let last_height = height.checked_sub(1)?;
    let start = height.saturating_sub(RETARGET_INTERVAL).max(1);
    let first = blocks.get(usize::try_from(start).ok()?)?;
    let last = blocks.get(usize::try_from(last_height).ok()?)?;
    let actual = last.header.timestamp.saturating_sub(first.header.timestamp);
    let intervals = last_height.checked_sub(start)?;
    let expected = intervals.saturating_mul(TARGET_BLOCK_TIME_SECS);
    Some(next_bits(prev.header.bits, actual, expected))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chain {
    pub blocks: Vec<Block>,
    pub state: State,
}

pub struct BuiltBlock { pub block: Block, pub skipped: Vec<Transaction> }

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BuildError {
    #[error("Chain has no tip")]
    EmptyChain,

    #[error("Clock is behind the chain: now is {now}, but the block timestamp must be at least {min}")]
    ClockBehind { now: u64, min: u64 },

    #[error("Cannot assemble block: {0:?}")]
    Block(BlockError),

    #[error("Invalid coinbase (miner address rejected): {0:?}")]
    Coinbase(StateError),
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp");
    PathBuf::from(name)
}

/// Атомарная запись: tmp → sync_all → rename; при ошибке tmp удаляется.
pub(crate) fn atomic_write_json<T: Serialize + ?Sized>(
    value: &T,
    path: &Path,
) -> Result<(), StorageError> {
    let tmp = tmp_path(path);
    let result = write_then_rename(value, &tmp, path);
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn write_then_rename<T: Serialize + ?Sized>(
    value: &T,
    tmp: &Path,
    target: &Path,
) -> Result<(), StorageError> {
    let mut writer = BufWriter::new(File::create(tmp)?);
    serde_json::to_writer_pretty(&mut writer, value)?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    fs::rename(tmp, target)?;
    Ok(())
}

impl Chain {
    pub fn new(bits: u32) -> Result<Chain, ChainError> {
        let genesis_block = genesis(bits).map_err(|_| ChainError::InvalidGenesis)?;
        let mut state = State::new();
        state.apply_block(&genesis_block).map_err(|e| ChainError::InvalidBlock {
            height: 0,
            reason: BlockValidationError::State(e),
        })?;
        Ok(Chain { blocks: vec![genesis_block], state })
    }

    pub fn tip(&self) -> Option<&Block> {
        self.blocks.last()
    }

    pub fn height(&self) -> u64 {
        self.blocks.len().saturating_sub(1) as u64
    }

    pub fn validate_block(&self, block: &Block, now: u64) -> Result<State, BlockValidationError> {
        let tip = self.tip().ok_or(BlockValidationError::EmptyChain)?;
        let header = &block.header;

        if header.version != BLOCK_VERSION {
            return Err(BlockValidationError::WrongVersion {
                expected: BLOCK_VERSION,
                got: header.version,
            });
        }

        let expected_height = tip.header.height.saturating_add(1);
        if header.height != expected_height {
            return Err(BlockValidationError::WrongHeight {
                expected: expected_height,
                got: header.height,
            });
        }

        if header.prev_hash != tip.hash() {
            return Err(BlockValidationError::PrevHashMismatch);
        }

        let expected = expected_bits(&self.blocks).ok_or(BlockValidationError::EmptyChain)?;
        if header.bits != expected {
            return Err(BlockValidationError::WrongBits { expected, got: header.bits });
        }

        block.verify_merkle_root()?;

        if !header.meets_target() {
            return Err(BlockValidationError::InsufficientWork);
        }

        let median = median_time_past(&self.blocks);
        if header.timestamp <= median {
            return Err(BlockValidationError::TimestampTooOld { median, got: header.timestamp });
        }

        let limit = now.saturating_add(MAX_FUTURE_SECS);
        if header.timestamp > limit {
            return Err(BlockValidationError::TimestampInFuture { limit, got: header.timestamp });
        }

        let mut new_state = self.state.clone();
        new_state.apply_block(block)?;
        Ok(new_state)
    }

    pub fn add_block(&mut self, block: Block, now: u64) -> Result<(), ChainError> {
        let new_state = self.validate_block(&block, now).map_err(|reason| ChainError::InvalidBlock {
            height: u64::try_from(self.blocks.len()).unwrap_or(u64::MAX),
            reason,
        })?;
        self.blocks.push(block);
        self.state = new_state;
        Ok(())
    }

    pub fn from_blocks(blocks: Vec<Block>, now: u64) -> Result<Chain, ChainError> {
        let mut iter = blocks.into_iter();
        let first = iter.next().ok_or(ChainError::EmptyChain)?;
        let mut chain = Chain::new(first.header.bits)?;
        if chain.blocks.first() != Some(&first) {
            return Err(ChainError::InvalidGenesis);
        }
        for block in iter {
            chain.add_block(block, now)?;
        }
        Ok(chain)
    }

    pub fn validate(&self, now: u64) -> Result<(), ChainError> {
        let rebuilt = Chain::from_blocks(self.blocks.clone(), now)?;
        if rebuilt.state != self.state {
            return Err(ChainError::StateMismatch);
        }
        Ok(())
    }
        
    pub fn save(&self, path: &Path) -> Result<(), StorageError> {
        let data = ChainFileRef { format_version: FORMAT_VERSION, blocks: &self.blocks };
        atomic_write_json(&data, path)
    }

    pub fn load(path: &Path, now: u64) -> Result<Chain, StorageError> {
        let text = fs::read_to_string(path)?;

        let version: VersionOnly = serde_json::from_str(&text)?;
        if version.format_version != FORMAT_VERSION {
            return Err(StorageError::UnsupportedVersion {
                found: version.format_version,
                expected: FORMAT_VERSION,
            });
        }

        let file: ChainFile = serde_json::from_str(&text)?;
        Ok(Chain::from_blocks(file.blocks, now)?)
    }

    // собирает блок-кандидата
    pub fn build_block(
        &self,
        miner: &str,
        pending: &[Transaction],
        now: u64,
    ) -> Result<BuiltBlock, BuildError> {
        let tip = self.tip().ok_or(BuildError::EmptyChain)?;
        let bits = expected_bits(&self.blocks).ok_or(BuildError::EmptyChain)?;
        let height = tip.header.height.saturating_add(1);

        let min_timestamp = median_time_past(&self.blocks).saturating_add(1);
        let timestamp = now.max(min_timestamp);
        if timestamp > now.saturating_add(MAX_FUTURE_SECS) {
            return Err(BuildError::ClockBehind { now, min: min_timestamp });
        }

        let make = |txs: Vec<Transaction>| Block::new(tip.hash(), height, txs, timestamp, bits);

        let coinbase = Transaction {
            from: String::new(),
            to: miner.to_string(),
            amount: COINBASE_REWARD,
            nonce: height,
        };
        let mut included = vec![coinbase];

        // Блок только с наградой должен применяться: иначе виноват адрес майнера.
        let base = make(included.clone()).map_err(BuildError::Block)?;
        self.state.clone().apply_block(&base).map_err(BuildError::Coinbase)?;

        let mut skipped = Vec::new();
        for tx in pending {
            let mut candidate = included.clone();
            candidate.push(tx.clone());
            let applicable = match make(candidate) {
                Ok(block) => self.state.clone().apply_block(&block).is_ok(),
                Err(_) => false,
            };
            if applicable {
                included.push(tx.clone());
            } else {
                skipped.push(tx.clone());
            }
        }

        let block = make(included).map_err(BuildError::Block)?;
        Ok(BuiltBlock { block, skipped })
    }
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Unsupported file format version: found {found}, expected {expected}")]
    UnsupportedVersion { found: u32, expected: u32 },

    #[error(transparent)]
    Chain(#[from] ChainError),
}

#[derive(Serialize)]
struct ChainFileRef<'a> {
    format_version: u32,
    blocks: &'a [Block],
}

#[derive(Deserialize)]
pub struct VersionOnly {
    pub format_version: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainFile {
    format_version: u32,
    blocks: Vec<Block>
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use serde_json::{json, Value};
    use crate::models::{
        block_header::BlockHeader, hash::HashParseError
    };

    use crate::models::{
        block_header::{GENESIS_ADDRESS, GENESIS_TIMESTAMP},
        hash::Hash,
        transaction::{Transaction, COINBASE_REWARD},
    };

    const BITS: u32 = 4;
    const NOW: u64 = 4_000_000_000; 

    fn ts(step: u64, i: u64) -> u64 {
        GENESIS_TIMESTAMP + step * i
    }

    fn coinbase(to: &str, height: u64) -> Transaction {
        Transaction { from: String::new(), to: to.to_string(), amount: COINBASE_REWARD, nonce: height }
    }

    fn next_block(chain: &Chain, txs: Vec<Transaction>, timestamp: u64) -> Block {
        let tip = chain.tip().unwrap();
        let bits = expected_bits(&chain.blocks).unwrap();
        let mut block = Block::new(tip.hash(), tip.header.height + 1, txs, timestamp, bits).unwrap();
        block.mine().unwrap();
        block
    }

    /// Цепь из n блоков поверх генезиса; метки идут с шагом `step` секунд.
    fn build_chain(n: u64, step: u64) -> Chain {
        let mut chain = Chain::new(BITS).unwrap();
        for i in 1..=n {
            let block = next_block(&chain, vec![coinbase("miner", i)], ts(step, i));
            chain.add_block(block, NOW).unwrap();
        }
        chain
    }

    fn height_of_error(err: &ChainError) -> Option<u64> {
        match err {
            ChainError::InvalidBlock { height, .. } => Some(*height),
            _ => None,
        }
    }

    fn reason(err: ChainError) -> BlockValidationError {
        match err {
            ChainError::InvalidBlock { reason, .. } => reason,
            other => panic!("unexpected error: {other:?}"),
        }
    }

    fn block_with_ts(timestamp: u64) -> Block {
        Block::new(Hash([0; 32]), 0, vec![coinbase("m", 0)], timestamp, 0).unwrap()
    }

    #[test]
    fn median_cases() {
        let blocks = |v: &[u64]| v.iter().map(|t| block_with_ts(*t)).collect::<Vec<_>>();
        assert_eq!(median_time_past(&[]), 0);
        assert_eq!(median_time_past(&blocks(&[7])), 7);
        assert_eq!(median_time_past(&blocks(&[10, 20])), 20);
        assert_eq!(median_time_past(&blocks(&[5, 1, 3])), 3);
        assert_eq!(median_time_past(&blocks(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11])), 6);
        // 12 блоков: первый (1000) не учитывается
        assert_eq!(median_time_past(&blocks(&[1000, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11])), 6);
    }

    #[test]
    fn expected_bits_basic() {
        assert_eq!(expected_bits(&[]), None);
        let chain = build_chain(8, 10);
        // следующий блок имеет высоту 9: пересчёта нет
        assert_eq!(expected_bits(&chain.blocks), Some(BITS));
    }

    #[test]
    fn retarget_on_target_pace_keeps_bits() {
        let chain = build_chain(10, 10);
        assert_eq!(chain.blocks[10].header.bits, BITS);
    }

    #[test]
    fn retarget_fast_blocks_raise_bits_by_two() {
        let chain = build_chain(11, 1);
        assert_eq!(chain.blocks[9].header.bits, BITS);
        assert_eq!(chain.blocks[10].header.bits, BITS + 2);
        assert_eq!(chain.blocks[11].header.bits, BITS + 2);
    }

    #[test]
    fn retarget_slow_blocks_lower_bits_by_two() {
        let chain = build_chain(10, 100);
        assert_eq!(chain.blocks[10].header.bits, BITS - 2);
    }

    #[test]
    fn first_retarget_ignores_genesis_timestamp() {
        // блок 1 далеко от генезиса, но блоки 1..9 идут быстро: должно быть +2, а не -2
        let mut chain = Chain::new(BITS).unwrap();
        let far = GENESIS_TIMESTAMP + 100_000_000;
        for i in 1..=10u64 {
            let t = far + i; // шаг 1 секунда между блоками
            let block = next_block(&chain, vec![coinbase("miner", i)], t);
            chain.add_block(block, NOW).unwrap();
        }
        assert_eq!(chain.blocks[10].header.bits, BITS + 2);
    }

    #[test]
    fn second_retarget_uses_nine_intervals() {
        // окно блока 20: блоки 10..19, 9 интервалов по 10 с = 90: ровно в цель
        let chain = build_chain(20, 10);
        assert_eq!(chain.blocks[20].header.bits, BITS);
    }

    #[test]
    fn valid_chain_of_25_blocks_passes_validate() {
        let chain = build_chain(25, 10);
        assert_eq!(chain.height(), 25);
        assert_eq!(chain.validate(NOW), Ok(()));
        assert_eq!(chain.state.balance("miner"), 25 * COINBASE_REWARD);
        assert_eq!(chain.state.balance(GENESIS_ADDRESS), COINBASE_REWARD);
    }

    #[test]
    fn new_chain_has_only_genesis() {
        let chain = Chain::new(BITS).unwrap();
        assert_eq!(chain.height(), 0);
        assert_eq!(chain.blocks.len(), 1);
        assert_eq!(chain.blocks[0], genesis(BITS).unwrap());
        assert_eq!(chain.state.balance(GENESIS_ADDRESS), COINBASE_REWARD);
        assert_eq!(chain.validate(NOW), Ok(()));
    }

    #[test]
    fn new_rejects_invalid_bits() {
        assert_eq!(Chain::new(0), Err(ChainError::InvalidGenesis));
        assert_eq!(Chain::new(256), Err(ChainError::InvalidGenesis));
    }

    /// Цепь из 3 блоков и корректный блок 4 поверх неё (не добавленный).
    fn fresh() -> (Chain, Block) {
        let chain = build_chain(3, 10);
        let block = next_block(&chain, vec![coinbase("miner", 4)], ts(10, 4));
        (chain, block)
    }

    #[test]
    fn rejects_wrong_version() {
        let (chain, mut block) = fresh();
        block.header.version += 1;
        assert_eq!(
            chain.validate_block(&block, NOW),
            Err(BlockValidationError::WrongVersion { expected: BLOCK_VERSION, got: BLOCK_VERSION + 1 })
        );
    }

    #[test]
    fn rejects_wrong_height() {
        let (chain, mut block) = fresh();
        block.header.height = 9;
        assert_eq!(
            chain.validate_block(&block, NOW),
            Err(BlockValidationError::WrongHeight { expected: 4, got: 9 })
        );
    }

    #[test]
    fn rejects_wrong_prev_hash() {
        let (chain, mut block) = fresh();
        block.header.prev_hash = Hash([9; 32]);
        assert_eq!(chain.validate_block(&block, NOW), Err(BlockValidationError::PrevHashMismatch));
    }

    #[test]
    fn rejects_wrong_bits() {
        let (chain, mut block) = fresh();
        block.header.bits = BITS + 1;
        assert_eq!(
            chain.validate_block(&block, NOW),
            Err(BlockValidationError::WrongBits { expected: BITS, got: BITS + 1 })
        );
    }

    #[test]
    fn rejects_bad_merkle_root() {
        let (chain, mut block) = fresh();
        block.transactions[0].to = "thief".to_string();
        assert_eq!(
            chain.validate_block(&block, NOW),
            Err(BlockValidationError::Block(BlockError::MerkleRootMismatch))
        );
    }

    #[test]
    fn rejects_insufficient_work() {
        let (chain, mut block) = fresh();
        // подбираем nonce, при котором PoW не выполнен
        let mut nonce = block.header.nonce;
        loop {
            nonce += 1;
            block.header.nonce = nonce;
            if !block.header.meets_target() {
                break;
            }
        }
        assert_eq!(chain.validate_block(&block, NOW), Err(BlockValidationError::InsufficientWork));
    }

    #[test]
    fn rejects_invalid_transactions() {
        let chain = build_chain(3, 10);
        let bad = Transaction { from: "nobody".into(), to: "x".into(), amount: 1, nonce: 0 };
        let block = next_block(&chain, vec![coinbase("miner", 4), bad], ts(10, 4));
        assert_eq!(
            chain.validate_block(&block, NOW),
            Err(BlockValidationError::State(StateError::UnknownSender))
        );
    }

    #[test]
    fn timestamp_must_be_strictly_greater_than_median() {
        let chain = build_chain(3, 10);
        let median = median_time_past(&chain.blocks);
        let equal = next_block(&chain, vec![coinbase("m", 4)], median);
        assert_eq!(
            chain.validate_block(&equal, NOW),
            Err(BlockValidationError::TimestampTooOld { median, got: median })
        );
        let ok = next_block(&chain, vec![coinbase("m", 4)], median + 1);
        assert!(chain.validate_block(&ok, NOW).is_ok());
    }

    #[test]
    fn timestamp_future_limit() {
        let chain = build_chain(3, 10);
        let now = ts(10, 3) + 50;
        let at_limit = next_block(&chain, vec![coinbase("m", 4)], now + MAX_FUTURE_SECS);
        assert!(chain.validate_block(&at_limit, now).is_ok());
        let over = next_block(&chain, vec![coinbase("m", 4)], now + MAX_FUTURE_SECS + 1);
        assert_eq!(
            chain.validate_block(&over, now),
            Err(BlockValidationError::TimestampInFuture {
                limit: now + MAX_FUTURE_SECS,
                got: now + MAX_FUTURE_SECS + 1
            })
        );
    }

    // ---------- add_block ----------

    #[test]
    fn failed_add_block_leaves_chain_unchanged() {
        let (mut chain, mut block) = fresh();
        block.header.prev_hash = Hash([1; 32]);
        let before = chain.clone();
        let err = chain.add_block(block, NOW).unwrap_err();
        assert_eq!(height_of_error(&err), Some(4));
        assert_eq!(reason(err), BlockValidationError::PrevHashMismatch);
        assert_eq!(chain, before);
    }

    #[test]
    fn add_block_applies_transfers_to_state() {
        let mut chain = build_chain(2, 10);
        let txs = vec![
            coinbase("miner", 3),
            Transaction { from: "miner".into(), to: "bob".into(), amount: 30, nonce: 0 },
        ];
        let block = next_block(&chain, txs, ts(10, 3));
        chain.add_block(block, NOW).unwrap();
        assert_eq!(chain.state.balance("bob"), 30);
        assert_eq!(chain.state.balance("miner"), 3 * COINBASE_REWARD - 30);
    }

    #[test]
    fn tampered_transaction_in_old_block_is_detected_with_height() {
        let mut chain = build_chain(6, 10);
        chain.blocks[3].transactions[0].to = "thief".to_string();
        let err = chain.validate(NOW).unwrap_err();
        assert_eq!(height_of_error(&err), Some(3));
        assert_eq!(reason(err), BlockValidationError::Block(BlockError::MerkleRootMismatch));
    }

    #[test]
    fn tampered_header_in_old_block_is_detected() {
        let mut chain = build_chain(6, 10);
        // timestamp не входит в Merkle, но входит в хэш заголовка
        chain.blocks[3].header.timestamp += 1;
        let err = chain.validate(NOW).unwrap_err();
        let h = height_of_error(&err).unwrap();
        assert!(h == 3 || h == 4, "height {h}");
    }

    #[test]
    fn tampered_nonce_in_old_block_is_detected() {
        let mut chain = build_chain(6, 10);
        chain.blocks[2].header.nonce += 1;
        let err = chain.validate(NOW).unwrap_err();
        let h = height_of_error(&err).unwrap();
        assert!(h == 2 || h == 3, "height {h}");
    }

    #[test]
    fn replaced_genesis_is_detected() {
        let mut chain = build_chain(3, 10);
        chain.blocks[0].transactions[0].to = "attacker".to_string();
        assert_eq!(chain.validate(NOW), Err(ChainError::InvalidGenesis));
    }

    #[test]
    fn inconsistent_state_is_detected() {
        let mut chain = build_chain(3, 10);
        chain.state.accounts.get_mut("miner").unwrap().balance += 1;
        assert_eq!(chain.validate(NOW), Err(ChainError::StateMismatch));
    }

    #[test]
    fn from_blocks_rejects_empty_list() {
        assert_eq!(Chain::from_blocks(Vec::new(), NOW), Err(ChainError::EmptyChain));
    }

    #[test]
    fn from_blocks_roundtrip() {
        let chain = build_chain(12, 10);
        let rebuilt = Chain::from_blocks(chain.blocks.clone(), NOW).unwrap();
        assert_eq!(rebuilt, chain);
    }

    /// Сохраняет цепь из n блоков во временный каталог.
    fn saved(n: u64, step: u64) -> (tempfile::TempDir, PathBuf, Chain) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chain.json");
        let chain = build_chain(n, step);
        chain.save(&path).unwrap();
        (dir, path, chain)
    }

    fn read_value(path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    /// Читает JSON-файл, применяет правку и записывает обратно.
    fn tamper(path: &Path, edit: impl FnOnce(&mut Value)) {
        let mut value = read_value(path);
        edit(&mut value);
        fs::write(path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
    }

    fn write_text(dir: &tempfile::TempDir, text: &str) -> PathBuf {
        let path = dir.path().join("input.json");
        fs::write(&path, text).unwrap();
        path
    }

    fn chain_error(err: StorageError) -> ChainError {
        match err {
            StorageError::Chain(e) => e,
            other => panic!("expected StorageError::Chain, got {other:?}"),
        }
    }

    fn invalid_block(err: StorageError) -> (u64, BlockValidationError) {
        match chain_error(err) {
            ChainError::InvalidBlock { height, reason } => (height, reason),
            other => panic!("expected InvalidBlock, got {other:?}"),
        }
    }

    fn blocks_mut(value: &mut Value) -> &mut Vec<Value> {
        value["blocks"].as_array_mut().unwrap()
    }

    // ===== Hash: FromStr =====

    #[test]
    fn hash_from_str_roundtrip_and_both_cases() {
        let mut bytes = [0u8; 32];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = (i * 8 + 3) as u8;
        }
        let hash = Hash(bytes);
        assert_eq!(hash.to_string().parse::<Hash>().unwrap(), hash);
        assert_eq!("ab".repeat(32).parse::<Hash>().unwrap(), "AB".repeat(32).parse::<Hash>().unwrap());
    }

    #[test]
    fn hash_from_str_errors() {
        assert_eq!("a".repeat(63).parse::<Hash>(), Err(HashParseError::WrongLength {got: 63}));
        assert_eq!("a".repeat(65).parse::<Hash>(), Err(HashParseError::WrongLength{got: 65}));
        assert_eq!("".parse::<Hash>(), Err(HashParseError::WrongLength{got: 0}));
        assert_eq!(
            format!("{}g", "a".repeat(63)).parse::<Hash>(),
            Err(HashParseError::InvalidChar { position: 63})
        );
        assert_eq!(
            format!("+{}", "a".repeat(63)).parse::<Hash>(),
            Err(HashParseError::InvalidChar { position: 0})
        );
        assert_eq!(
            format!(" {}", "a".repeat(63)).parse::<Hash>(),
            Err(HashParseError::InvalidChar { position: 0 })
        );
        assert!("я".repeat(32).parse::<Hash>().is_err());
        assert!(format!("я{}", "a".repeat(62)).parse::<Hash>().is_err());
    }

    // ===== Hash: serde =====

    #[test]
    fn hash_serializes_as_lowercase_hex_string() {
        let json = serde_json::to_string(&Hash([0xab; 32])).unwrap();
        assert_eq!(json, format!("\"{}\"", "ab".repeat(32)));
    }

    #[test]
    fn hash_json_roundtrip() {
        let hash = Hash([0x12; 32]);
        let back: Hash = serde_json::from_str(&serde_json::to_string(&hash).unwrap()).unwrap();
        assert_eq!(back, hash);
    }

    #[test]
    fn hash_deserialize_accepts_uppercase_but_reserializes_lowercase() {
        let upper = format!("\"{}\"", "AB".repeat(32));
        let hash: Hash = serde_json::from_str(&upper).unwrap();
        assert_eq!(serde_json::to_string(&hash).unwrap(), format!("\"{}\"", "ab".repeat(32)));
    }

    #[test]
    fn hash_deserialize_rejects_bad_input() {
        let cases = [
            format!("\"{}\"", "a".repeat(63)),
            format!("\"{}\"", "a".repeat(65)),
            format!("\"{}g\"", "a".repeat(63)),
            "\"\"".to_string(),
            "123".to_string(),
            "null".to_string(),
            "true".to_string(),
            "[1,2,3]".to_string(),
            "{}".to_string(),
        ];
        for json in cases {
            assert!(serde_json::from_str::<Hash>(&json).is_err(), "input: {json}");
        }
    }

    // ===== Модели: Transaction, BlockHeader, Block =====

    #[test]
    fn transaction_json_roundtrip_including_extremes() {
        let cases = [
            Transaction { from: "A".into(), to: "B".into(), amount: 1, nonce: 0 },
            Transaction { from: String::new(), to: "miner".into(), amount: COINBASE_REWARD, nonce: 7 },
            Transaction { from: "Ёж".into(), to: "地址🙂".into(), amount: u64::MAX, nonce: u64::MAX },
        ];
        for original in cases {
            let json = serde_json::to_string(&original).unwrap();
            let back: Transaction = serde_json::from_str(&json).unwrap();
            assert_eq!(back, original, "json: {json}");
        }
    }

    #[test]
    fn block_header_json_roundtrip() {
        let header = BlockHeader {
            version: BLOCK_VERSION,
            height: u64::MAX,
            prev_hash: Hash([1; 32]),
            merkle_root: Hash([2; 32]),
            timestamp: u64::MAX,
            bits: u32::MAX,
            nonce: u64::MAX,
        };
        let back: BlockHeader = serde_json::from_str(&serde_json::to_string(&header).unwrap()).unwrap();
        assert_eq!(back, header);
    }

    #[test]
    fn block_json_roundtrip_preserves_hash_and_merkle_root() {
        let txs = vec![coinbase("m", 3), Transaction { from: "m".into(), to: "x".into(), amount: 5, nonce: 0 }];
        let mut block = Block::new(Hash([7; 32]), 3, txs, 1_700_000_100, 8).unwrap();
        block.mine().unwrap();
        let back: Block = serde_json::from_str(&serde_json::to_string(&block).unwrap()).unwrap();
        assert_eq!(back, block);
        assert_eq!(back.hash(), block.hash());
        assert_eq!(back.verify_merkle_root(), Ok(()));
    }

    #[test]
    fn block_json_shape() {
        let block = Block::new(Hash([7; 32]), 3, vec![coinbase("m", 3)], 1_700_000_100, 12).unwrap();
        let v = serde_json::to_value(&block).unwrap();
        assert_eq!(v["header"]["prev_hash"], json!("07".repeat(32)));
        assert_eq!(v["header"]["bits"], json!(12));
        assert_eq!(v["header"]["height"], json!(3));
        assert_eq!(v["transactions"][0]["amount"], json!(COINBASE_REWARD));
        assert_eq!(v["transactions"][0]["from"], json!(""));
    }

    #[test]
    fn models_reject_unknown_missing_and_wrongly_typed_fields() {
        let bad_transactions = [
            r#"{"from":"","to":"a","amount":1,"nonce":0,"extra":1}"#, // лишнее поле
            r#"{"from":"","to":"a","amount":1}"#,                       // нет nonce
            r#"{"to":"a","amount":1,"nonce":0}"#,                       // нет from
            r#"{"from":"","to":"a","amount":"1","nonce":0}"#,           // строка вместо числа
            r#"{"from":"","to":"a","amount":1.5,"nonce":0}"#,           // дробное
            r#"{"from":"","to":"a","amount":-1,"nonce":0}"#,            // отрицательное
            r#"{"from":"","to":"a","amount":18446744073709551616,"nonce":0}"#, // 2^64
            r#"{"from":"","to":"a","amount":null,"nonce":0}"#,
            r#"{"from":"","to":"a","amount":1,"amount":2,"nonce":0}"#,  // повторное поле
            r#"{"from":1,"to":"a","amount":1,"nonce":0}"#,              // число вместо строки
            r#"[]"#,
            r#"null"#,
        ];
        for json in bad_transactions {
            assert!(serde_json::from_str::<Transaction>(json).is_err(), "input: {json}");
        }
    }

    #[test]
    fn header_rejects_out_of_range_numbers() {
        let ok = serde_json::to_value(BlockHeader {
            version: 1, height: 1, prev_hash: Hash([0; 32]), merkle_root: Hash([0; 32]),
            timestamp: 1, bits: 1, nonce: 1,
        })
        .unwrap();
        for (field, bad) in [
            ("version", json!(4_294_967_296u64)), // больше u32
            ("bits", json!(4_294_967_296u64)),
            ("bits", json!(-1)),
            ("height", json!(-1)),
            ("nonce", json!(1.5)),
            ("timestamp", json!("1")),
        ] {
            let mut v = ok.clone();
            v[field] = bad.clone();
            assert!(serde_json::from_value::<BlockHeader>(v).is_err(), "field {field} = {bad}");
        }
    }

    // ===== save / load: основной путь =====

    #[test]
    fn save_then_load_returns_equal_chain() {
        let (_dir, path, chain) = saved(12, 10); // включает пересчёт на высоте 10
        let loaded = Chain::load(&path, NOW).unwrap();
        assert_eq!(loaded, chain);
        assert_eq!(loaded.validate(NOW), Ok(()));
        assert_eq!(loaded.state.balance("bob"), 30);
        assert_eq!(loaded.height(), 12);
    }

    #[test]
    fn genesis_only_chain_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chain.json");
        let chain = Chain::new(BITS).unwrap();
        chain.save(&path).unwrap();
        assert_eq!(Chain::load(&path, NOW).unwrap(), chain);
    }

    #[test]
    fn file_has_expected_shape() {
        let (_dir, path, chain) = saved(3, 10);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains('\n'), "ожидается JSON с отступами");
        let v = read_value(&path);
        assert_eq!(v["format_version"], json!(1));
        assert_eq!(v["blocks"].as_array().unwrap().len(), chain.blocks.len());
        assert!(v.get("state").is_none(), "состояние не должно храниться");
    }

    #[test]
    fn save_is_deterministic_and_load_save_is_idempotent() {
        let (dir, path1, chain) = saved(5, 10);
        let path2 = dir.path().join("second.json");
        chain.save(&path2).unwrap();
        assert_eq!(fs::read(&path1).unwrap(), fs::read(&path2).unwrap());

        let path3 = dir.path().join("third.json");
        Chain::load(&path1, NOW).unwrap().save(&path3).unwrap();
        assert_eq!(fs::read(&path1).unwrap(), fs::read(&path3).unwrap());
    }

    #[test]
    fn load_normalizes_uppercase_hex_on_next_save() {
        let (dir, path, chain) = saved(2, 10);
        let text = fs::read_to_string(&path).unwrap();
        let hash_hex = chain.blocks[1].header.prev_hash.to_string();
        let upper_text = text.replace(&hash_hex, &hash_hex.to_uppercase());
        assert_ne!(text, upper_text);
        let upper_path = write_text(&dir, &upper_text);
        let loaded = Chain::load(&upper_path, NOW).unwrap();
        assert_eq!(loaded, chain);
        let resaved = dir.path().join("resaved.json");
        loaded.save(&resaved).unwrap();
        assert_eq!(fs::read(&resaved).unwrap(), fs::read(&path).unwrap());
    }

    // ===== load: подмены содержимого =====

    #[test]
    fn tampered_transaction_recipient_in_block_3() {
        let (_dir, path, _) = saved(6, 10);
        tamper(&path, |v| v["blocks"][3]["transactions"][0]["to"] = json!("thief"));
        let (height, reason) = invalid_block(Chain::load(&path, NOW).unwrap_err());
        assert_eq!(height, 3);
        assert_eq!(reason, BlockValidationError::Block(BlockError::MerkleRootMismatch));
    }

    #[test]
    fn tampered_coinbase_amount_in_block_3() {
        let (_dir, path, _) = saved(6, 10);
        tamper(&path, |v| v["blocks"][3]["transactions"][0]["amount"] = json!(1_000_000u64));
        let (height, reason) = invalid_block(Chain::load(&path, NOW).unwrap_err());
        assert_eq!(height, 3);
        assert_eq!(reason, BlockValidationError::Block(BlockError::MerkleRootMismatch));
    }

    #[test]
    fn tampered_transfer_amount_in_block_2() {
        let (_dir, path, _) = saved(6, 10);
        tamper(&path, |v| v["blocks"][2]["transactions"][1]["amount"] = json!(31));
        let (height, reason) = invalid_block(Chain::load(&path, NOW).unwrap_err());
        assert_eq!(height, 2);
        assert_eq!(reason, BlockValidationError::Block(BlockError::MerkleRootMismatch));
    }

    #[test]
    fn tampered_header_nonce_is_detected_at_block_or_next() {
        let (_dir, path, _) = saved(6, 10);
        tamper(&path, |v| {
            let n = v["blocks"][2]["header"]["nonce"].as_u64().unwrap();
            v["blocks"][2]["header"]["nonce"] = json!(n + 1);
        });
        let (height, reason) = invalid_block(Chain::load(&path, NOW).unwrap_err());
        match height {
            2 => assert_eq!(reason, BlockValidationError::InsufficientWork),
            3 => assert_eq!(reason, BlockValidationError::PrevHashMismatch),
            other => panic!("unexpected height {other}"),
        }
    }

    #[test]
    fn tampered_header_timestamp_is_detected_at_block_or_next() {
        let (_dir, path, _) = saved(6, 10);
        tamper(&path, |v| {
            let t = v["blocks"][3]["header"]["timestamp"].as_u64().unwrap();
            v["blocks"][3]["header"]["timestamp"] = json!(t + 1);
        });
        let (height, reason) = invalid_block(Chain::load(&path, NOW).unwrap_err());
        match height {
            3 => assert_eq!(reason, BlockValidationError::InsufficientWork),
            4 => assert_eq!(reason, BlockValidationError::PrevHashMismatch),
            other => panic!("unexpected height {other}"),
        }
    }

    #[test]
    fn tampered_prev_hash_in_block_4() {
        let (_dir, path, _) = saved(6, 10);
        tamper(&path, |v| v["blocks"][4]["header"]["prev_hash"] = json!("11".repeat(32)));
        let (height, reason) = invalid_block(Chain::load(&path, NOW).unwrap_err());
        assert_eq!(height, 4);
        assert_eq!(reason, BlockValidationError::PrevHashMismatch);
    }

    #[test]
    fn tampered_merkle_root_in_block_3() {
        let (_dir, path, _) = saved(6, 10);
        tamper(&path, |v| v["blocks"][3]["header"]["merkle_root"] = json!("22".repeat(32)));
        let (height, reason) = invalid_block(Chain::load(&path, NOW).unwrap_err());
        assert_eq!(height, 3);
        assert_eq!(reason, BlockValidationError::Block(BlockError::MerkleRootMismatch));
    }

    #[test]
    fn tampered_bits_in_block_5() {
        let (_dir, path, _) = saved(6, 10);
        tamper(&path, |v| v["blocks"][5]["header"]["bits"] = json!(BITS + 1));
        let (height, reason) = invalid_block(Chain::load(&path, NOW).unwrap_err());
        assert_eq!(height, 5);
        assert_eq!(reason, BlockValidationError::WrongBits { expected: BITS, got: BITS + 1 });
    }

    #[test]
    fn tampered_version_in_block_2() {
        let (_dir, path, _) = saved(6, 10);
        tamper(&path, |v| v["blocks"][2]["header"]["version"] = json!(BLOCK_VERSION + 1));
        let (height, reason) = invalid_block(Chain::load(&path, NOW).unwrap_err());
        assert_eq!(height, 2);
        assert_eq!(
            reason,
            BlockValidationError::WrongVersion { expected: BLOCK_VERSION, got: BLOCK_VERSION + 1 }
        );
    }

    #[test]
    fn tampered_height_in_block_3() {
        let (_dir, path, _) = saved(6, 10);
        tamper(&path, |v| v["blocks"][3]["header"]["height"] = json!(99));
        let (height, reason) = invalid_block(Chain::load(&path, NOW).unwrap_err());
        assert_eq!(height, 3);
        assert_eq!(reason, BlockValidationError::WrongHeight { expected: 3, got: 99 });
    }

    #[test]
    fn tampered_genesis_is_detected() {
        let (_dir, path, _) = saved(4, 10);
        tamper(&path, |v| v["blocks"][0]["transactions"][0]["to"] = json!("attacker"));
        assert_eq!(chain_error(Chain::load(&path, NOW).unwrap_err()), ChainError::InvalidGenesis);
    }

    #[test]
    fn genesis_with_other_bits_or_invalid_bits_is_rejected() {
        for bits in [0u32, BITS + 1, 256, 1000] {
            let (_dir, path, _) = saved(3, 10);
            tamper(&path, |v| v["blocks"][0]["header"]["bits"] = json!(bits));
            assert_eq!(
                chain_error(Chain::load(&path, NOW).unwrap_err()),
                ChainError::InvalidGenesis,
                "bits = {bits}"
            );
        }
    }

    #[test]
    fn removed_middle_block_is_detected() {
        let (_dir, path, _) = saved(6, 10);
        tamper(&path, |v| {
            blocks_mut(v).remove(2);
        });
        let (height, reason) = invalid_block(Chain::load(&path, NOW).unwrap_err());
        assert_eq!(height, 2);
        assert_eq!(reason, BlockValidationError::WrongHeight { expected: 2, got: 3 });
    }

    #[test]
    fn swapped_blocks_are_detected() {
        let (_dir, path, _) = saved(6, 10);
        tamper(&path, |v| blocks_mut(v).swap(2, 3));
        let (height, reason) = invalid_block(Chain::load(&path, NOW).unwrap_err());
        assert_eq!(height, 2);
        assert_eq!(reason, BlockValidationError::WrongHeight { expected: 2, got: 3 });
    }

    #[test]
    fn duplicated_block_is_detected() {
        let (_dir, path, _) = saved(6, 10);
        tamper(&path, |v| {
            let copy = v["blocks"][3].clone();
            blocks_mut(v).insert(4, copy);
        });
        let (height, reason) = invalid_block(Chain::load(&path, NOW).unwrap_err());
        assert_eq!(height, 4);
        assert_eq!(reason, BlockValidationError::WrongHeight { expected: 4, got: 3 });
    }

    #[test]
    fn forged_block_appended_with_inflated_reward_is_detected() {
        let (_dir, path, chain) = saved(4, 10);
        // блок собран и намайнен честно, но награда завышена: проходит всё, кроме правил состояния
        let mut forged_cb = coinbase("miner", 5);
        forged_cb.amount = COINBASE_REWARD * 100;
        let forged = next_block(&chain, vec![forged_cb], ts(10, 5));
        tamper(&path, |v| blocks_mut(v).push(serde_json::to_value(&forged).unwrap()));
        let (height, reason) = invalid_block(Chain::load(&path, NOW).unwrap_err());
        assert_eq!(height, 5);
        assert_eq!(reason, BlockValidationError::State(StateError::InvalidCoinbase));
    }

    #[test]
    fn truncated_chain_loads_as_shorter_valid_chain() {
        // известное ограничение (ADR 0007): отрезанный хвост файла нельзя отличить от более короткой цепи
        let (_dir, path, chain) = saved(6, 10);
        tamper(&path, |v| {
            blocks_mut(v).pop();
        });
        let loaded = Chain::load(&path, NOW).unwrap();
        assert_eq!(loaded.blocks.len(), chain.blocks.len() - 1);
        assert_eq!(loaded.blocks[..], chain.blocks[..chain.blocks.len() - 1]);
    }

    #[test]
    fn blocks_from_the_future_are_rejected_relative_to_now() {
        // метки блоков идут с шагом 10 с; при now = GENESIS_TIMESTAMP допуск 120 с
        // блок 12 (G+120) проходит, блок 13 (G+130) нет
        let (_dir, path, _) = saved(15, 10);
        let (height, reason) = invalid_block(Chain::load(&path, GENESIS_TIMESTAMP).unwrap_err());
        assert_eq!(height, 13);
        assert_eq!(
            reason,
            BlockValidationError::TimestampInFuture {
                limit: GENESIS_TIMESTAMP + 120,
                got: GENESIS_TIMESTAMP + 130
            }
        );
    }

    #[test]
    fn error_message_names_the_height() {
        let (_dir, path, _) = saved(6, 10);
        tamper(&path, |v| v["blocks"][3]["transactions"][0]["to"] = json!("thief"));
        let message = Chain::load(&path, NOW).unwrap_err().to_string();
        assert!(message.contains("height 3"), "message: {message}");
    }

    #[test]
    fn every_single_field_mutation_is_detected() {
        let n = 12;
        let (_dir, path, _) = saved(n, 10);
        let original = fs::read_to_string(&path).unwrap();
        let dir2 = tempfile::tempdir().unwrap();
        let work = dir2.path().join("work.json");

        // (описание, нужно ли пропускать последний блок, правка)
        type Edit = fn(&mut Value, usize);
        let always: Vec<(&str, Edit)> = vec![
            ("coinbase amount", |v, i| {
                let a = v["blocks"][i]["transactions"][0]["amount"].as_u64().unwrap();
                v["blocks"][i]["transactions"][0]["amount"] = json!(a + 1);
            }),
            ("coinbase to", |v, i| v["blocks"][i]["transactions"][0]["to"] = json!("thief")),
            ("prev_hash", |v, i| v["blocks"][i]["header"]["prev_hash"] = json!("33".repeat(32))),
            ("merkle_root", |v, i| v["blocks"][i]["header"]["merkle_root"] = json!("44".repeat(32))),
            ("bits", |v, i| {
                let b = v["blocks"][i]["header"]["bits"].as_u64().unwrap();
                v["blocks"][i]["header"]["bits"] = json!(b + 1);
            }),
            ("version", |v, i| v["blocks"][i]["header"]["version"] = json!(BLOCK_VERSION + 1)),
            ("height", |v, i| {
                let h = v["blocks"][i]["header"]["height"].as_u64().unwrap();
                v["blocks"][i]["header"]["height"] = json!(h + 1);
            }),
        ];
        // nonce и timestamp последнего блока могут случайно остаться валидными (PoW проходит с вер. 1/16),
        // поэтому для них проверяем только блоки, за которыми есть следующий
        let not_last: Vec<(&str, Edit)> = vec![
            ("nonce", |v, i| {
                let x = v["blocks"][i]["header"]["nonce"].as_u64().unwrap();
                v["blocks"][i]["header"]["nonce"] = json!(x + 1);
            }),
            ("timestamp", |v, i| {
                let t = v["blocks"][i]["header"]["timestamp"].as_u64().unwrap();
                v["blocks"][i]["header"]["timestamp"] = json!(t + 1);
            }),
        ];

        for i in 0..=n as usize {
            for (name, edit) in &always {
                let mut v: Value = serde_json::from_str(&original).unwrap();
                edit(&mut v, i);
                fs::write(&work, serde_json::to_string(&v).unwrap()).unwrap();
                assert!(Chain::load(&work, NOW).is_err(), "mutation `{name}` in block {i} not detected");
            }
            if i < n as usize {
                for (name, edit) in &not_last {
                    let mut v: Value = serde_json::from_str(&original).unwrap();
                    edit(&mut v, i);
                    fs::write(&work, serde_json::to_string(&v).unwrap()).unwrap();
                    assert!(Chain::load(&work, NOW).is_err(), "mutation `{name}` in block {i} not detected");
                }
            }
        }
    }

    // ===== load: повреждённый или чужой файл =====

    #[test]
    fn load_rejects_empty_truncated_and_non_json_files() {
        let (dir, good, _) = saved(3, 10);
        let text = fs::read_to_string(&good).unwrap();
        let cases = [
            String::new(),
            "   \n".to_string(),
            text[..text.len() / 2].to_string(),
            text[..text.len() - 2].to_string(), // нет закрывающей скобки
            "not json at all".to_string(),
            "{".to_string(),
            "[]".to_string(),
            "{}".to_string(),
            "null".to_string(),
            "42".to_string(),
            "\"text\"".to_string(),
            format!("\u{feff}{text}"), // BOM
            format!("{text}garbage"),  // мусор после JSON
            format!("{text}{{}}"),     // второй JSON-документ
        ];
        for (i, case) in cases.iter().enumerate() {
            let path = write_text(&dir, case);
            let err = Chain::load(&path, NOW).unwrap_err();
            assert!(matches!(err, StorageError::Json(_)), "case {i}: {err:?}");
        }
    }

    #[test]
    fn load_rejects_unsupported_or_malformed_version() {
        let dir = tempfile::tempdir().unwrap();
        let unsupported = [
            (r#"{"format_version":2,"blocks":[]}"#, 2),
            (r#"{"format_version":0,"blocks":[]}"#, 0),
            (r#"{"format_version":4294967295,"blocks":[]}"#, u32::MAX),
            // незнакомая версия с другой структурой тоже даёт UnsupportedVersion, а не ошибку полей
            (r#"{"format_version":2,"something":"else"}"#, 2),
        ];
        for (text, found) in unsupported {
            let path = write_text(&dir, text);
            let err = Chain::load(&path, NOW).unwrap_err();
            assert!(
                matches!(err, StorageError::UnsupportedVersion { found: f, expected: 1 } if f == found),
                "{text}: {err:?}"
            );
        }
        let malformed = [
            r#"{"blocks":[]}"#,
            r#"{"format_version":"1","blocks":[]}"#,
            r#"{"format_version":-1,"blocks":[]}"#,
            r#"{"format_version":4294967296,"blocks":[]}"#,
            r#"{"format_version":1.5,"blocks":[]}"#,
            r#"{"format_version":null,"blocks":[]}"#,
        ];
        for text in malformed {
            let path = write_text(&dir, text);
            let err = Chain::load(&path, NOW).unwrap_err();
            assert!(matches!(err, StorageError::Json(_)), "{text}: {err:?}");
        }
    }

    #[test]
    fn load_rejects_missing_blocks_and_unknown_root_fields() {
        let dir = tempfile::tempdir().unwrap();
        for text in [
            r#"{"format_version":1}"#,
            r#"{"format_version":1,"blocks":[],"extra":1}"#,
            r#"{"format_version":1,"blocks":{}}"#,
            r#"{"format_version":1,"blocks":null}"#,
            r#"{"format_version":1,"blocks":[1,2,3]}"#,
        ] {
            let path = write_text(&dir, text);
            let err = Chain::load(&path, NOW).unwrap_err();
            assert!(matches!(err, StorageError::Json(_)), "{text}: {err:?}");
        }
    }

    #[test]
    fn load_rejects_empty_block_list_with_chain_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_text(&dir, r#"{"format_version":1,"blocks":[]}"#);
        assert_eq!(chain_error(Chain::load(&path, NOW).unwrap_err()), ChainError::EmptyChain);
    }

    #[test]
    fn load_rejects_unknown_and_missing_fields_inside_blocks() {
        type Edit = fn(&mut Value);
        let edits: Vec<(&str, Edit)> = vec![
            ("extra field in block", |v| v["blocks"][1]["extra"] = json!(1)),
            ("extra field in header", |v| v["blocks"][1]["header"]["extra"] = json!(1)),
            ("extra field in transaction", |v| v["blocks"][1]["transactions"][0]["extra"] = json!(1)),
            ("missing header", |v| {
                v["blocks"][1].as_object_mut().unwrap().remove("header");
            }),
            ("missing nonce", |v| {
                v["blocks"][1]["header"].as_object_mut().unwrap().remove("nonce");
            }),
            ("missing transactions", |v| {
                v["blocks"][1].as_object_mut().unwrap().remove("transactions");
            }),
            ("missing tx field", |v| {
                v["blocks"][1]["transactions"][0].as_object_mut().unwrap().remove("amount");
            }),
            ("amount as string", |v| v["blocks"][1]["transactions"][0]["amount"] = json!("50")),
            ("amount as float", |v| v["blocks"][1]["transactions"][0]["amount"] = json!(50.5)),
            ("amount negative", |v| v["blocks"][1]["transactions"][0]["amount"] = json!(-1)),
            ("nonce negative", |v| v["blocks"][1]["header"]["nonce"] = json!(-1)),
            ("bits too large for u32", |v| v["blocks"][1]["header"]["bits"] = json!(4_294_967_296u64)),
            ("transactions not an array", |v| v["blocks"][1]["transactions"] = json!({})),
        ];
        for (name, edit) in edits {
            let (_dir, path, _) = saved(3, 10);
            tamper(&path, edit);
            let err = Chain::load(&path, NOW).unwrap_err();
            assert!(matches!(err, StorageError::Json(_)), "{name}: {err:?}");
        }
    }

    #[test]
    fn load_rejects_malformed_hashes() {
        let bad = [
            json!("11".repeat(31)),            // слишком короткий
            json!(format!("{}1", "11".repeat(32))), // слишком длинный
            json!(format!("zz{}", "11".repeat(31))), // не hex
            json!(""),
            json!(5),
            json!(null),
            json!([1, 2, 3]),
        ];
        for hash in bad {
            for field in ["prev_hash", "merkle_root"] {
                let (_dir, path, _) = saved(3, 10);
                tamper(&path, |v| v["blocks"][2]["header"][field] = hash.clone());
                let err = Chain::load(&path, NOW).unwrap_err();
                assert!(matches!(err, StorageError::Json(_)), "{field} = {hash}: {err:?}");
            }
        }
    }

    #[test]
    fn load_reports_io_errors() {
        let dir = tempfile::tempdir().unwrap();
        // нет файла
        let err = Chain::load(&dir.path().join("missing.json"), NOW).unwrap_err();
        assert!(matches!(err, StorageError::Io(ref e) if e.kind() == io::ErrorKind::NotFound), "{err:?}");
        // вместо файла каталог
        let err = Chain::load(dir.path(), NOW).unwrap_err();
        assert!(matches!(err, StorageError::Io(_)), "{err:?}");
        // не UTF-8
        let path = dir.path().join("binary.json");
        fs::write(&path, [0xff, 0xfe, 0x00, 0x80]).unwrap();
        assert!(matches!(Chain::load(&path, NOW).unwrap_err(), StorageError::Io(_)));
    }

    // ===== save: атомарная запись =====

    #[test]
    fn tmp_path_is_in_same_directory() {
        let p = Path::new("/a/b/chain.json");
        assert_eq!(tmp_path(p), PathBuf::from("/a/b/chain.json.tmp"));
        assert_eq!(tmp_path(p).parent(), p.parent());
        assert_eq!(tmp_path(Path::new("chain")), PathBuf::from("chain.tmp"));
    }

    #[test]
    fn save_leaves_no_tmp_file() {
        let (dir, path, _) = saved(3, 10);
        assert!(!tmp_path(&path).exists());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn save_replaces_existing_file_entirely_even_with_shorter_chain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chain.json");
        build_chain(8, 10).save(&path).unwrap();
        let long_len = fs::metadata(&path).unwrap().len();

        let short = Chain::new(5).unwrap();
        short.save(&path).unwrap();
        assert!(fs::metadata(&path).unwrap().len() < long_len);
        assert_eq!(Chain::load(&path, NOW).unwrap(), short); // без «хвоста» от старой версии
    }

    #[test]
    fn save_into_missing_directory_fails_and_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no_such_dir").join("chain.json");
        let err = Chain::new(4).unwrap().save(&path).unwrap_err();
        assert!(matches!(err, StorageError::Io(ref e) if e.kind() == io::ErrorKind::NotFound), "{err:?}");
        assert!(!dir.path().join("no_such_dir").exists());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn stale_tmp_file_is_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chain.json");
        fs::write(tmp_path(&path), b"garbage from a previous crash").unwrap();
        let chain = Chain::new(4).unwrap();
        chain.save(&path).unwrap();
        assert!(!tmp_path(&path).exists());
        assert_eq!(Chain::load(&path, NOW).unwrap(), chain);
    }

    #[test]
    fn failed_save_keeps_original_file_and_foreign_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chain.json");
        let original = Chain::new(4).unwrap();
        original.save(&path).unwrap();
        let before = fs::read(&path).unwrap();

        // на месте временного файла лежит каталог: создать файл невозможно
        fs::create_dir(tmp_path(&path)).unwrap();
        assert!(Chain::new(5).unwrap().save(&path).is_err());

        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(Chain::load(&path, NOW).unwrap(), original);
        assert!(tmp_path(&path).is_dir(), "чужой каталог не должен быть удалён");
    }

    #[test]
    fn failed_rename_removes_tmp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chain.json");
        fs::create_dir(&path).unwrap(); // цель каталог: rename файла на него не удастся
        assert!(Chain::new(4).unwrap().save(&path).is_err());
        assert!(!tmp_path(&path).exists());
        assert!(path.is_dir());
    }

    #[test]
    fn saving_does_not_modify_the_chain() {
        let (_dir, path, chain) = saved(4, 10);
        let snapshot = chain.clone();
        chain.save(&path).unwrap();
        assert_eq!(chain, snapshot);
    }
}