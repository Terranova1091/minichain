use crate::models::common_errors::CodecError;
use crate::models::hash::Hash;
use crate::models::hash::sha256d;
use crate::models::transaction::Transaction;

const LEAF_PREFIX: u8 = 0x00;
const NODE_PREFIX: u8 = 0x01;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}
pub type MerkleProof = Vec<(Side, Hash)>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MerkleError {
    EmptyTransactionList,
    Codec(CodecError),
    IndexOutOfRange { index: usize, len: usize },
}

impl From<CodecError> for MerkleError {
    fn from(error: CodecError) -> Self {
        MerkleError::Codec(error)
    }
}

pub fn merkle_root(transactions: &[Transaction]) -> Result<Hash, MerkleError> {
    let levels = build_levels(transactions)?;
    levels
        .last()
        .and_then(|level| level.first())
        .copied()
        .ok_or(MerkleError::EmptyTransactionList)
}

pub fn build_levels(transactions: &[Transaction]) -> Result<Vec<Vec<Hash>>, MerkleError> {
    if transactions.is_empty() {
        return Err(MerkleError::EmptyTransactionList);
    }

    let leaves = transactions
        .iter()
        .map(leaf_hash)
        .collect::<Result<Vec<Hash>, CodecError>>()?;

    let mut levels = vec![leaves];
    while let Some(current) = levels.last() {
        if current.len() <= 1 {
            break;
        }
        let next_level: Vec<Hash> = current
            .chunks(2)
            .map(|pair| match pair {
                [left, right] => node_hash(*left, *right),
                // Непарный последний узел переходит наверх без изменений (ADR 0001).
                // chunks(2) никогда не отдаёт пустой кусок, поэтому pair[0] безопасен.
                _ => pair[0],
            })
            .collect();
        levels.push(next_level);
    }
    Ok(levels)
}

pub fn verify_proof(root: Hash, tx: &Transaction, proof: &MerkleProof) -> bool {
    let Ok(mut acc) = leaf_hash(tx) else {
        return false;
    };   
    for (side, hash) in proof {
        acc = match side {
            Side::Left => node_hash(*hash, acc),
            Side::Right => node_hash(acc, *hash),
        };
    }
    acc == root
}

pub fn merkle_proof(transactions: &[Transaction], index: usize) -> Result<MerkleProof, MerkleError> {
    if index >= transactions.len() {
        return Err(MerkleError::IndexOutOfRange {
            index,
            len: transactions.len(),
        });
    }
    
    let levels = build_levels(transactions)?;
    let mut proof: MerkleProof = MerkleProof::new();
    let mut pos = index;
    for level in &levels[..levels.len() - 1] {
        if pos % 2 != 0 {
            proof.push((Side::Left, level[pos-1]));
        } else if level.len() > pos + 1 {
            proof.push((Side::Right, level[pos+1]));
        }
        pos /= 2;
    }
    Ok(proof)
}

pub fn leaf_hash(transaction: &Transaction) -> Result<Hash, CodecError> {
    let bytes = transaction.to_bytes()?;
    let mut buffer = Vec::with_capacity(1 + bytes.len());
    buffer.push(LEAF_PREFIX);
    buffer.extend_from_slice(&bytes);
    let hash = sha256d(&buffer);
    Ok(hash)
}

pub fn node_hash(left: Hash, right: Hash) -> Hash {
    let mut buffer: [u8; 65] = [0; 65];
    buffer[0] = NODE_PREFIX;
    buffer[1..33].copy_from_slice(&left.0);
    buffer[33..65].copy_from_slice(&right.0);
    sha256d(&buffer)
}

#[cfg(test)]
mod tests {

use super::*;

    fn tx(from: &str, to: &str, amount: u64, nonce: u64) -> Transaction {
        Transaction {
            from: from.to_string(),
            to: to.to_string(),
            amount,
            nonce,
        }
    }

    fn t1() -> Transaction {
        tx("", "M", 50, 0)
    }
    fn t2() -> Transaction {
        tx("A", "B", 5, 0)
    }
    fn t3() -> Transaction {
        tx("A", "C", 7, 1)
    }
    fn t4() -> Transaction {
        tx("B", "A", 1, 0)
    }

    #[test]
    fn leaf_hash_matches_known_values() {
        let cases = [
            (
                "t1 (coinbase)",
                t1(),
                "bb7abc3e12dfd44a9483e3525d59bee8d6f84b650f79bc9ab41b8f6c8f72ee15",
            ),
            (
                "t2",
                t2(),
                "cc3428031a15889f1de5506bbced6bd24a1ed045d76836a1ce984b511c8ce7d5",
            ),
            (
                "t3",
                t3(),
                "5b7418739be801461d10abeaf2e9fe537e5f038083ff2e99361cdd5ebb207e78",
            ),
            (
                "t4",
                t4(),
                "af1e8dfd68278331f1a9c7922c8b66372d4d923cccefb2c96745d654ac70221f",
            ),
        ];
        for (name, transaction, expected) in cases {
            let actual = leaf_hash(&transaction).unwrap().to_string();
            assert_eq!(actual, expected, "case: {name}");
        }
    }

    #[test]
    fn leaf_hash_differs_from_plain_transaction_hash() {
        let transaction = t2();
        let plain = sha256d(&transaction.to_bytes().unwrap());
        assert_eq!(
            plain.to_string(),
            "90738b79a3113e120b7178fc332bd97ad3885946fd7d555ced337d9560b8b035"
        );
        assert_ne!(leaf_hash(&transaction).unwrap(), plain);
    }

    #[test]
    fn node_hash_matches_known_value() {
        let left = leaf_hash(&t1()).unwrap();
        let right = leaf_hash(&t2()).unwrap();
        assert_eq!(
            node_hash(left, right).to_string(),
            "4d15b683d6aa4bf43a8209a56e21bc24b8193afaeb6d17628a445202f7a44fca"
        );
        let left = leaf_hash(&t3()).unwrap();
        let right = leaf_hash(&t4()).unwrap();
        assert_eq!(
            node_hash(left, right).to_string(),
            "922f36750f304e6d8568f34671476ddffec68887caf3ca8d43b4ccdc736d9238"
        );
    }

    #[test]
    fn node_hash_depends_on_order() {
        let a = leaf_hash(&t1()).unwrap();
        let b = leaf_hash(&t2()).unwrap();
        assert_ne!(node_hash(a, b), node_hash(b, a));
    }

    #[test]
    fn leaf_hash_reports_encoding_error() {
        let long = tx(&"x".repeat(70_000), "B", 1, 0);
        assert_eq!(
            leaf_hash(&long).unwrap_err(),
            CodecError::StringTooLong { len: 70_000 }
        );
    }


    #[test]
    fn test_merkle_root_with_correct_transactions_list() {
        let cases = [
            (
                "t1",
                vec![t1()],
                "bb7abc3e12dfd44a9483e3525d59bee8d6f84b650f79bc9ab41b8f6c8f72ee15",
            ),
            (
                "t1,t2",
                vec![t1(), t2()],
                "4d15b683d6aa4bf43a8209a56e21bc24b8193afaeb6d17628a445202f7a44fca",
            ),
            (
                "t1,t2,t3",
                vec![t1(), t2(), t3()],
                "2c8e8f9cc5146f7f3246b10f0b6d0c61a91afec5fe6135f727796063ef899baf",
            ),
            (
                "t1,t2,t3,t4",
                vec![t1(), t2(), t3(), t4()],
                "abde42811c56c4cd22c2f8939aeeb2b40286c713e6462582e8678d0db222263b",
            ),
        ];
        for (name, transactions, expected) in cases {
            let root = merkle_root(&transactions).unwrap().to_string();
            assert_eq!(root, expected, "case: {name}");
        }
    }

    #[test]
    fn test_merkle_root_duplicate_transactions() {
        let double_transactions = vec![t1(), t2(), t3(), t3()];
        let default_transactions = vec![t1(), t2(), t3()];
        let root_of_double_transactions = merkle_root(&double_transactions).unwrap().to_string();
        let root_of_default_transactions = merkle_root(&default_transactions).unwrap().to_string();
        assert_ne!(root_of_double_transactions, root_of_default_transactions);
    }

    #[test]
    fn test_merkle_root_empty_list() {
        let double_transactions = vec![];
        let result = merkle_root(&double_transactions);
        assert!(matches!(
            result, 
            Err(MerkleError::EmptyTransactionList {})
        ));
    }

    #[test]
    fn test_merkle_root_encoding_error() {
        let long_transaction = tx(&"x".repeat(70_000), "B", 1, 0);
        let transactions = vec![long_transaction];
        let result = merkle_root(&transactions);
        assert!(matches!(
            result,
            Err(MerkleError::Codec(CodecError::StringTooLong { len: 70_000 }))
        ));
    }


    #[test]
    fn test_merkle_root_rearrangement_transactions() {
        let transactions1 = vec![t1(), t2()];
        let transactions2 = vec![t2(), t1()];
        let root1 = merkle_root(&transactions1).unwrap().to_string();
        let root2 = merkle_root(&transactions2).unwrap().to_string();
        assert_ne!(root1, root2);
    }

    // ---------- verify_proof ----------
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

    #[test]
    fn proof_roundtrip_for_every_index_and_size() {
        for n in 1..=17 {
            let txs = sample_txs(n);
            let root = merkle_root(&txs).unwrap();
            for (i, transaction) in txs.iter().enumerate() {
                let proof = merkle_proof(&txs, i).unwrap();
                assert!(verify_proof(root, transaction, &proof), "n={n}, i={i}");
            }
        }
    }

    #[test]
    fn proof_length_matches_tree_shape() {
        // (размер дерева, индекс, ожидаемая длина доказательства)
        // 3 листа: [a,b,c] -> [ab,c] -> [root]; у c на первом уровне пары нет
        let cases = [(1, 0, 0), (2, 0, 1), (2, 1, 1), (3, 0, 2), (3, 1, 2), (3, 2, 1), (4, 3, 2), (5, 4, 1)];
        for (n, i, expected) in cases {
            let txs = sample_txs(n);
            assert_eq!(merkle_proof(&txs, i).unwrap().len(), expected, "n={n}, i={i}");
        }
    }

    #[test]
    fn single_tx_has_empty_proof_and_root_equals_leaf() {
        let txs = sample_txs(1);
        let proof = merkle_proof(&txs, 0).unwrap();
        assert!(proof.is_empty());
        let root = merkle_root(&txs).unwrap();
        assert_eq!(root, leaf_hash(&txs[0]).unwrap());
        assert!(verify_proof(root, &txs[0], &proof));
    }

    #[test]
    fn proof_index_out_of_range() {
        let txs = sample_txs(3);
        assert_eq!(
            merkle_proof(&txs, 3),
            Err(MerkleError::IndexOutOfRange { index: 3, len: 3 })
        );
        assert_eq!(
            merkle_proof(&txs, usize::MAX),
            Err(MerkleError::IndexOutOfRange { index: usize::MAX, len: 3 })
        );
    }

    #[test]
    fn proof_empty_list_is_an_error() {
        assert!(merkle_proof(&[], 0).is_err());
    }

    #[test]
    fn verify_rejects_other_transaction() {
        let txs = sample_txs(5);
        let root = merkle_root(&txs).unwrap();
        let proof = merkle_proof(&txs, 2).unwrap();
        let forged = tx("from2", "to2", 1_000_000, 2);
        assert!(!verify_proof(root, &forged, &proof));
        // доказательство для индекса 2 не подходит к транзакции с индекса 3
        assert!(!verify_proof(root, &txs[3], &proof));
    }

    #[test]
    fn verify_rejects_wrong_root() {
        let txs = sample_txs(5);
        let proof = merkle_proof(&txs, 1).unwrap();
        let other_root = merkle_root(&sample_txs(6)).unwrap();
        assert!(!verify_proof(other_root, &txs[1], &proof));
        assert!(!verify_proof(Hash([0u8; 32]), &txs[1], &proof));
    }

    #[test]
    fn verify_rejects_tampered_sibling_hash() {
        let txs = sample_txs(6);
        let root = merkle_root(&txs).unwrap();
        let proof = merkle_proof(&txs, 4).unwrap();
        for k in 0..proof.len() {
            let mut bad = proof.clone();
            bad[k].1 .0[0] ^= 1;
            assert!(!verify_proof(root, &txs[4], &bad), "sibling {k}");
        }
    }

    #[test]
    fn verify_rejects_flipped_side() {
        let txs = sample_txs(6);
        let root = merkle_root(&txs).unwrap();
        let proof = merkle_proof(&txs, 1).unwrap();
        for k in 0..proof.len() {
            let mut bad = proof.clone();
            bad[k].0 = match bad[k].0 {
                Side::Left => Side::Right,
                Side::Right => Side::Left,
            };
            assert!(!verify_proof(root, &txs[1], &bad), "side {k}");
        }
    }

    #[test]
    fn verify_rejects_reordered_removed_and_extra_elements() {
        let txs = sample_txs(8);
        let root = merkle_root(&txs).unwrap();
        let proof = merkle_proof(&txs, 5).unwrap();
        assert_eq!(proof.len(), 3);

        let mut reordered = proof.clone();
        reordered.swap(0, 2);
        assert!(!verify_proof(root, &txs[5], &reordered));

        let mut shorter = proof.clone();
        shorter.pop();
        assert!(!verify_proof(root, &txs[5], &shorter));
        assert!(!verify_proof(root, &txs[5], &Vec::new()));

        let mut longer = proof.clone();
        longer.push(proof[0]);
        assert!(!verify_proof(root, &txs[5], &longer));
    }

    #[test]
    fn verify_returns_false_when_tx_cannot_be_encoded() {
        let txs = sample_txs(3);
        let root = merkle_root(&txs).unwrap();
        let proof = merkle_proof(&txs, 1).unwrap();
        let huge = tx(&"x".repeat(70_000), "b", 1, 0);
        assert!(!verify_proof(root, &huge, &proof));
    }

    #[test]
    fn odd_tree_is_not_ambiguous_with_duplicated_last_leaf() {
        // [a,b,c] и [a,b,c,c] должны иметь разные корни (см. ADR 0001)
        let abc = sample_txs(3);
        let mut abcc = abc.clone();
        abcc.push(abc[2].clone());
        assert_ne!(merkle_root(&abc).unwrap(), merkle_root(&abcc).unwrap());
        // доказательство для c из трёх листьев не подходит к корню четырёх
        let proof = merkle_proof(&abc, 2).unwrap();
        assert!(!verify_proof(merkle_root(&abcc).unwrap(), &abc[2], &proof));
    }
}