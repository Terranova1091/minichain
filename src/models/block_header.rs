use serde::{Deserialize, Serialize};

use crate::models::block::BlockError;
use crate::models::hash::Hash;
use crate::models::transaction::Transaction;
use crate::models::block::Block;

pub const MAX_BITS: u32 = 256;
pub const RETARGET_INTERVAL: u64 = 10;
pub const TARGET_BLOCK_TIME_SECS: u64 = 10;
pub const MIN_BITS: u32 = 0;
pub const MAX_RETARGET_BITS: u32 = 255;
pub const GENESIS_TIMESTAMP: u64 = 1_700_000_000;
pub const GENESIS_ADDRESS: &str = "genesis";
const COINBASE_REWARD: u64 = 50;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockHeader {
    pub version: u32,
    pub height: u64,
    pub prev_hash: Hash,
    pub merkle_root: Hash,
    pub timestamp: u64,
    pub bits: u32,
    pub nonce: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MineError {
    InvalidBits,
    NonceExhausted,
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenesisError {
    InvalidBits,
    Block(BlockError),
    Mine(MineError),
}

impl From<BlockError> for GenesisError {
    fn from(e: BlockError) -> Self {
        GenesisError::Block(e)
    }
}

impl From<MineError> for GenesisError {
    fn from(e: MineError) -> Self {
        GenesisError::Mine(e)
    }
}

impl BlockHeader {
    pub fn to_bytes(&self) -> [u8; 96] {
        let mut buffer: [u8; 96] = [0; 96];
        let header_elements = [
            self.version.to_be_bytes().to_vec(),
            self.height.to_be_bytes().to_vec(),
            self.prev_hash.0.to_vec(),
            self.merkle_root.0.to_vec(),
            self.timestamp.to_be_bytes().to_vec(),
            self.bits.to_be_bytes().to_vec(),
            self.nonce.to_be_bytes().to_vec(),
        ];
        let mut pos = 0;
        for part in &header_elements {
            buffer[pos..pos + part.len()].copy_from_slice(part);
            pos += part.len();
        }
        buffer
    }

    pub fn hash(&self) -> Hash {
        crate::models::hash::sha256d(&self.to_bytes())
    }

    pub fn meets_target(&self) -> bool {
        meets_target(&self.hash(), self.bits)
    }

    pub fn mine(&mut self) -> Result<(), MineError> {
        if self.bits > MAX_BITS {
            return Err(MineError::InvalidBits)
        }
        for nonce in 0..=u64::MAX {
            self.nonce = nonce;
            if self.meets_target() {
                return Ok(());
            }
        }
        Err(MineError::NonceExhausted)
    }
}

fn leading_zero_bits(hash: &Hash) -> u32 {
    let mut count = 0;
    for byte in &hash.0 {
        if *byte == 0 {
            count += 8;
        } else {
            count += byte.leading_zeros();
            break;
        }
    }
    count
}

pub fn meets_target(hash: &Hash, bits: u32) -> bool {
    if bits > MAX_BITS {
        return false;
    }
    leading_zero_bits(hash) >= bits
}

pub fn next_bits(prev_bits: u32, actual_secs: u64, expected_secs: u64) -> u32 {
    let e2 = u128::from(expected_secs) * u128::from(expected_secs);
    let a2 = u128::from(actual_secs) * u128::from(actual_secs);
    let next = if e2 >= 8 * a2 {
        prev_bits.saturating_add(2)
    } else if e2 >= 2 * a2 {
        prev_bits.saturating_add(1)
    } else if 2 * e2 > a2 {
        prev_bits
    } else if 8 * e2 > a2 {
        prev_bits.saturating_sub(1)
    } else {
        prev_bits.saturating_sub(2)
    };
    next.clamp(MIN_BITS, MAX_RETARGET_BITS)
}

pub fn is_retarget_height(height: u64) -> bool {
    height > 0 && height % RETARGET_INTERVAL == 0
}

pub fn genesis(bits: u32) -> Result<Block, GenesisError> {
    if !(MIN_BITS..=MAX_RETARGET_BITS).contains(&bits) {
        return Err(GenesisError::InvalidBits);
    }
    let coinbase = Transaction {
        from: String::new(),
        to: GENESIS_ADDRESS.to_string(),
        amount: COINBASE_REWARD,
        nonce: 0,
    };
    let mut block = Block::new(Hash([0; 32]), 0, vec![coinbase], GENESIS_TIMESTAMP, bits)?;
    block.mine()?;
    Ok(block)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zero_header() -> BlockHeader {
        BlockHeader {
            version: 0,
            height: 0,
            prev_hash: Hash([0; 32]),
            merkle_root: Hash([0; 32]),
            timestamp: 0,
            bits: 0,
            nonce: 0,
        }
    }

    fn sample_header() -> BlockHeader {
        BlockHeader {
            version: 1,
            height: 2,
            prev_hash: Hash([0x11; 32]),
            merkle_root: Hash([0x22; 32]),
            timestamp: 1_700_000_000,
            bits: 16,
            nonce: 0x0A0B0C0D0E0F1011,
        }
    }

    fn sample_header_flexible(bits: u32) -> BlockHeader {
        BlockHeader {
            version: 1,
            height: 7,
            prev_hash: Hash([0x11; 32]),
            merkle_root: Hash([0x22; 32]),
            timestamp: 1_700_000_000,
            bits,
            nonce: 999,
        }
    }

    #[test]
    fn hash_matches_known_values() {
        let cases = [
            (
                "all zero fields",
                zero_header(),
                "d328800c7f3ccafe648d3eb43b47eb416f48103f1cd81ddd7a0c41431e4e463a",
            ),
            (
                "distinct values",
                sample_header(),
                "6350309088e6bdf0bf2bd48e47ace92025535c442c151c8217dd2f12d27320f2",
            ),
            (
                "bitcoin-like bits",
                BlockHeader {
                    version: 1,
                    height: 1,
                    prev_hash: Hash([1; 32]),
                    merkle_root: Hash([2; 32]),
                    timestamp: 1_234_567_890,
                    bits: 0x1d00ffff,
                    nonce: 2_083_236_893,
                },
                "9140cbe42e03081cffa5e794f1f1061fd3f77f2729ae7af8e6028f6e60bf500e",
            ),
        ];

        for (name, header, expected) in cases {
            assert_eq!(header.hash().to_string(), expected, "case: {name}");
        }
    }

    #[test]
    fn to_bytes_layout_is_big_endian_in_field_order() {
        let expected = concat!(
            "00000001",
            "0000000000000002",
            "1111111111111111111111111111111111111111111111111111111111111111",
            "2222222222222222222222222222222222222222222222222222222222222222",
            "000000006553f100",
            "00000010",
            "0a0b0c0d0e0f1011",
        );
        let actual: String = sample_header()
            .to_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn changing_any_field_changes_hash() {
        let base = sample_header();
        let base_hash = base.hash();

        let mut variants = Vec::new();
        let mut h = base.clone();
        h.version += 1;
        variants.push(("version", h));
        let mut h = base.clone();
        h.height += 1;
        variants.push(("height", h));
        let mut h = base.clone();
        h.prev_hash.0[0] ^= 1;
        variants.push(("prev_hash", h));
        let mut h = base.clone();
        h.merkle_root.0[31] ^= 1;
        variants.push(("merkle_root", h));
        let mut h = base.clone();
        h.timestamp += 1;
        variants.push(("timestamp", h));
        let mut h = base.clone();
        h.bits += 1;
        variants.push(("bits", h));
        let mut h = base.clone();
        h.nonce += 1;
        variants.push(("nonce", h));

        for (field, header) in variants {
            assert_ne!(
                header.hash(),
                base_hash,
                "field {field} does not affect the hash"
            );
        }
    }

        fn hash_with_prefix(prefix: &[u8]) -> Hash {
        let mut bytes = [0xFFu8; 32];
        bytes[..prefix.len()].copy_from_slice(prefix);
        Hash(bytes)
    }

    fn base_header() -> BlockHeader {
        BlockHeader {
            version: 1,
            height: 7,
            prev_hash: Hash([0x11; 32]),
            merkle_root: Hash([0x22; 32]),
            timestamp: 1_700_000_000,
            bits: 12,
            nonce: 42,
        }
    }

    #[test]
    fn leading_zero_bits_known_cases() {
        let cases: [(&str, Hash, u32); 7] = [
            ("all zeros", Hash([0u8; 32]), 256),
            ("0x80 first", hash_with_prefix(&[0x80]), 0),
            ("0xff first", hash_with_prefix(&[0xFF]), 0),
            ("0x01 first", hash_with_prefix(&[0x01]), 7),
            ("00 00 10", hash_with_prefix(&[0x00, 0x00, 0x10]), 19),
            ("00 ff", hash_with_prefix(&[0x00, 0xFF]), 8),
            ("00 0f", hash_with_prefix(&[0x00, 0x0F]), 12),
        ];
        for (name, hash, expected) in cases {
            assert_eq!(leading_zero_bits(&hash), expected, "case: {name}");
        }
    }

    #[test]
    fn leading_zero_bits_only_last_bit_set() {
        let mut bytes = [0u8; 32];
        bytes[31] = 1;
        assert_eq!(leading_zero_bits(&Hash(bytes)), 255);
    }

    #[test]
    fn leading_zero_bits_ignores_zeros_after_first_one() {
        // 0x80 0x00 0x00 ...: после единицы нули не считаются
        let mut bytes = [0u8; 32];
        bytes[0] = 0x80;
        assert_eq!(leading_zero_bits(&Hash(bytes)), 0);
    }

    #[test]
    fn meets_target_boundary_12_vs_13() {
        let hash = hash_with_prefix(&[0x00, 0x0F]); // ровно 12 нулевых бит
        assert!(meets_target(&hash, 11));
        assert!(meets_target(&hash, 12));
        assert!(!meets_target(&hash, 13));
    }

    #[test]
    fn meets_target_zero_bits_accepts_everything() {
        assert!(meets_target(&hash_with_prefix(&[0xFF]), 0));
        assert!(meets_target(&Hash([0u8; 32]), 0));
    }

    #[test]
    fn meets_target_above_max_bits_rejects_everything() {
        let zero = Hash([0u8; 32]);
        assert!(meets_target(&zero, 256));
        assert!(!meets_target(&zero, 257));
        assert!(!meets_target(&zero, u32::MAX));
    }

    #[test]
    fn meets_target_256_needs_all_zero_hash() {
        let mut bytes = [0u8; 32];
        bytes[31] = 1;
        assert!(!meets_target(&Hash(bytes), 256));
    }

    #[test]
    fn header_hash_is_deterministic() {
        assert_eq!(base_header().hash(), base_header().hash());
    }

    #[test]
    fn header_hash_equals_sha256d_of_encoding() {
        let header = base_header();
        assert_eq!(header.hash(), crate::models::hash::sha256d(&header.to_bytes()));
    }

    #[test]
    fn header_hash_changes_with_every_field() {
        let base = base_header();
        let mutants: Vec<(&str, BlockHeader)> = vec![
            ("version", BlockHeader { version: 2, ..base.clone() }),
            ("height", BlockHeader { height: 8, ..base.clone() }),
            ("prev_hash", BlockHeader { prev_hash: Hash([0x12; 32]), ..base.clone() }),
            ("merkle_root", BlockHeader { merkle_root: Hash([0x23; 32]), ..base.clone() }),
            ("timestamp", BlockHeader { timestamp: 1_700_000_001, ..base.clone() }),
            ("bits", BlockHeader { bits: 13, ..base.clone() }),
            ("nonce", BlockHeader { nonce: 43, ..base.clone() }),
        ];
        for (name, mutant) in mutants {
            assert_ne!(mutant.hash(), base.hash(), "field: {name}");
        }
    }

    #[test]
    fn header_meets_target_agrees_with_function() {
        let mut header = base_header();
        header.bits = 0;
        assert!(header.meets_target());
        header.bits = 257;
        assert!(!header.meets_target());
        header.bits = 8;
        assert_eq!(header.meets_target(), leading_zero_bits(&header.hash()) >= 8);
    }

    #[test]
    fn brute_force_finds_valid_nonce_for_small_bits() {
        let mut header = base_header();
        header.bits = 8;
        let found = (0..1_000_000u64).any(|nonce| {
            header.nonce = nonce;
            header.meets_target()
        });
        assert!(found);
    }

        fn tx(from: &str, to: &str, amount: u64, nonce: u64) -> Transaction {
        Transaction { from: from.to_string(), to: to.to_string(), amount, nonce }
    }

    fn sample_block(bits: u32) -> Block {
        let txs = vec![tx("", "miner", 50, 0), tx("A", "B", 5, 0), tx("A", "C", 7, 1)];
        Block::new(Hash([0xAB; 32]), 5, txs, 1_700_000_000, bits).unwrap()
    }

    #[test]
    fn mine_zero_bits_stops_at_nonce_zero() {
        let mut header = sample_header_flexible(0);
        assert_eq!(header.mine(), Ok(()));
        assert_eq!(header.nonce, 0);
    }

    #[test]
    fn mine_finds_valid_nonce_for_small_bits() {
        for bits in [1, 4, 8, 12] {
            let mut header = sample_header_flexible(bits);
            assert_eq!(header.mine(), Ok(()), "bits={bits}");
            assert!(header.meets_target(), "bits={bits}");
            assert!(leading_zero_bits(&header.hash()) >= bits, "bits={bits}");
        }
    }

    #[test]
    fn mine_finds_the_smallest_nonce() {
        let mut header = sample_header_flexible(10);
        header.mine().unwrap();
        let found = header.nonce;
        let mut probe = header.clone();
        for nonce in 0..found {
            probe.nonce = nonce;
            assert!(!probe.meets_target(), "nonce {nonce} уже подходил");
        }
    }

    #[test]
    fn mine_ignores_initial_nonce() {
        let mut a = sample_header_flexible(10);
        let mut b = sample_header_flexible(10);
        a.nonce = 0;
        b.nonce = 123_456;
        a.mine().unwrap();
        b.mine().unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn mine_rejects_bits_above_max_without_hanging() {
        for bits in [MAX_BITS + 1, 1000, u32::MAX] {
            let mut header = sample_header_flexible(bits);
            assert_eq!(header.mine(), Err(MineError::InvalidBits), "bits={bits}");
        }
    }

    #[test]
    fn mine_changes_only_nonce() {
        let before = sample_header_flexible(8);
        let mut after = before.clone();
        after.mine().unwrap();
        assert_eq!(after.version, before.version);
        assert_eq!(after.height, before.height);
        assert_eq!(after.prev_hash, before.prev_hash);
        assert_eq!(after.merkle_root, before.merkle_root);
        assert_eq!(after.timestamp, before.timestamp);
        assert_eq!(after.bits, before.bits);
    }

    #[test]
    fn block_mine_delegates_to_header() {
        let mut block = sample_block(8);
        let mut header = block.header.clone();
        block.mine().unwrap();
        header.mine().unwrap();
        assert_eq!(block.header, header);
        assert!(block.header.meets_target());
        assert_eq!(block.hash(), header.hash());
    }

    #[test]
    fn mined_block_keeps_valid_merkle_root_and_transactions() {
        let mut block = sample_block(10);
        let txs_before = block.transactions.clone();
        block.mine().unwrap();
        assert_eq!(block.verify_merkle_root(), Ok(()));
        assert_eq!(block.transactions, txs_before);
    }

    #[test]
    fn block_mine_propagates_invalid_bits() {
        let mut block = sample_block(300);
        assert_eq!(block.mine(), Err(MineError::InvalidBits));
    }

    #[test]
    fn tampering_after_mining_breaks_proof_of_work() {
        let mut block = sample_block(16);
        block.mine().unwrap();
        assert!(block.header.meets_target());
        block.header.timestamp += 1;
        assert!(!block.header.meets_target());
    }

        const EXPECTED: u64 = 90; // 9 интервалов по 10 секунд

    /// Сдвиг bits относительно 16 для заданного actual.
    fn delta(actual: u64) -> i64 {
        i64::from(next_bits(16, actual, EXPECTED)) - 16
    }

    #[test]
    fn on_target_keeps_bits() {
        assert_eq!(next_bits(16, 90, 90), 16);
    }

    #[test]
    fn twice_as_fast_adds_one_bit() {
        assert_eq!(delta(45), 1);
    }

    #[test]
    fn four_times_faster_adds_two_bits() {
        assert_eq!(delta(22), 2);
    }

    #[test]
    fn much_faster_is_clamped_to_two_bits() {
        assert_eq!(delta(1), 2);
        assert_eq!(delta(0), 2); // нулевой интервал: без деления на ноль
    }

    #[test]
    fn twice_as_slow_removes_one_bit() {
        assert_eq!(delta(180), -1);
    }

    #[test]
    fn four_times_slower_removes_two_bits() {
        assert_eq!(delta(360), -2);
    }

    #[test]
    fn much_slower_is_clamped_to_two_bits() {
        assert_eq!(delta(9_000), -2);
        assert_eq!(delta(1_000_000), -2);
    }

    #[test]
    fn rounding_boundaries_for_expected_90() {
        // (actual, ожидаемый сдвиг): границы 31/32, 63/64, 127/128, 254/255
        let cases = [
            (31, 2), (32, 1),
            (63, 1), (64, 0),
            (127, 0), (128, -1),
            (254, -1), (255, -2),
        ];
        for (actual, expected_delta) in cases {
            assert_eq!(delta(actual), expected_delta, "actual={actual}");
        }
    }

    #[test]
    fn delta_is_monotonic_in_actual() {
        // чем дольше шли блоки, тем меньше (или столько же) сложность
        let mut prev = i64::MAX;
        for actual in 0..=400 {
            let d = delta(actual);
            assert!(d <= prev, "actual={actual}: {d} > {prev}");
            prev = d;
        }
    }

    #[test]
    fn lower_bound_is_min_bits() {
        assert_eq!(next_bits(MIN_BITS, 1_000_000, EXPECTED), MIN_BITS);
        assert_eq!(next_bits(2, 1_000_000, EXPECTED), MIN_BITS);
    }

    #[test]
    fn upper_bound_is_max_retarget_bits() {
        assert_eq!(next_bits(MAX_RETARGET_BITS, 1, EXPECTED), MAX_RETARGET_BITS);
        assert_eq!(next_bits(MAX_RETARGET_BITS - 1, 1, EXPECTED), MAX_RETARGET_BITS);
    }

    #[test]
    fn out_of_range_prev_bits_are_pulled_into_range() {
        assert_eq!(next_bits(0, 90, EXPECTED), MIN_BITS);
        assert_eq!(next_bits(300, 90, EXPECTED), MAX_RETARGET_BITS);
        assert_eq!(next_bits(u32::MAX, 1, EXPECTED), MAX_RETARGET_BITS);
    }

    #[test]
    fn huge_actual_does_not_panic() {
        assert_eq!(next_bits(16, u64::MAX, EXPECTED), 14);
        assert_eq!(next_bits(16, u64::MAX, u64::MAX), 16);
        assert_eq!(next_bits(16, 0, u64::MAX), 18);
        assert_eq!(next_bits(16, 2_000_000_000, EXPECTED), 14);
    }

    #[test]
    fn symmetric_up_and_down() {
        let up = next_bits(16, 45, EXPECTED);
        assert_eq!(next_bits(up, 180, EXPECTED), 16);
        let down = next_bits(16, 360, EXPECTED);
        assert_eq!(next_bits(down, 22, EXPECTED), 16);
    }

    #[test]
    fn works_for_other_expected_time() {
        // то же правило, другое окно: expected = 100
        assert_eq!(next_bits(16, 100, 100), 16);
        assert_eq!(next_bits(16, 50, 100), 17);
        assert_eq!(next_bits(16, 200, 100), 15);
    }

    #[test]
    fn retarget_heights() {
        assert!(!is_retarget_height(0));
        assert!(!is_retarget_height(1));
        assert!(!is_retarget_height(9));
        assert!(is_retarget_height(10));
        assert!(!is_retarget_height(11));
        assert!(is_retarget_height(20));
        assert!(is_retarget_height(100));
        assert!(!is_retarget_height(u64::MAX)); // 2^64 − 1 не кратно 10
    }

        const BITS: u32 = 8;

    fn hex(hash: &Hash) -> String {
        hash.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn genesis_is_deterministic() {
        assert_eq!(genesis(BITS).unwrap(), genesis(BITS).unwrap());
        assert_eq!(genesis(BITS).unwrap().hash(), genesis(BITS).unwrap().hash());
    }

    #[test]
    fn genesis_contains_single_coinbase() {
        let block = genesis(BITS).unwrap();
        assert_eq!(block.transactions.len(), 1);
        let coinbase = &block.transactions[0];
        assert!(coinbase.from.is_empty());
        assert_eq!(coinbase.to, GENESIS_ADDRESS);
        assert_eq!(coinbase.amount, COINBASE_REWARD);
        assert_eq!(coinbase.nonce, 0);
    }

    #[test]
    fn genesis_is_valid_block() {
        let block = genesis(BITS).unwrap();
        assert_eq!(block.verify_merkle_root(), Ok(()));
        assert!(block.header.meets_target());
        assert!(leading_zero_bits(&block.hash()) >= BITS);
    }

    #[test]
    fn genesis_nonce_is_the_smallest_valid() {
        let block = genesis(BITS).unwrap();
        let mut probe = block.header.clone();
        for nonce in 0..block.header.nonce {
            probe.nonce = nonce;
            assert!(!probe.meets_target(), "nonce {nonce} уже подходил");
        }
    }

    #[test]
    fn genesis_differs_for_different_bits() {
        let a = genesis(8).unwrap();
        let b = genesis(9).unwrap();
        assert_ne!(a.hash(), b.hash());
        assert_ne!(a.header.bits, b.header.bits);
    }

    #[test]
    fn genesis_rejects_invalid_bits() {
        assert_eq!(genesis(0), Err(GenesisError::InvalidBits));
        assert_eq!(genesis(MIN_BITS.wrapping_sub(1)), Err(GenesisError::InvalidBits));
        assert_eq!(genesis(MAX_RETARGET_BITS + 1), Err(GenesisError::InvalidBits));
        assert_eq!(genesis(256), Err(GenesisError::InvalidBits));
        assert_eq!(genesis(u32::MAX.wrapping_add(1)), Err(GenesisError::InvalidBits));
    }

    #[test]
    fn genesis_accepts_min_bits() {
        let block = genesis(MIN_BITS).unwrap();
        assert!(block.header.meets_target());
    }

    #[test]
    fn genesis_known_vectors() {
        let cases = [
            (1u32, 1u64, "5d5f5f6f4fc1117a2ea18d840577110e679faeeac55f0892b60aa260f21d278c"),
            (8, 358, "00e57a57e4cc4e9975b5809916e24bf29f24a0aa59a1647d8a07d8cb31963821"),
            (12, 1072, "000fb6a49ea05c7b1713723eeff81573a94fce818def504e0429b27dd0c135ae"),
        ];
        for (bits, nonce, hash) in cases {
            let block = genesis(bits).unwrap();
            assert_eq!(block.header.nonce, nonce, "bits={bits}");
            assert_eq!(hex(&block.hash()), hash, "bits={bits}");
            assert_eq!(
                hex(&block.header.merkle_root),
                "e002ff0e1b07c3fcd210c015906ed64b71318ecb75ef271cc6eb326ef9e38534",
                "bits={bits}"
            );
        }
    }
}
