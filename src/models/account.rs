use std::collections::BTreeMap;

use crate::models::block::Block;
use crate::models::transaction::Transaction;
use crate::models::transaction::COINBASE_REWARD;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Account {
    pub balance: u64,
    pub next_nonce: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateError {
    ZeroAmount,
    SelfTransfer,
    UnknownSender,
    NonceMismatch { expected: u64, got: u64 },
    InsufficientBalance { balance: u64, amount: u64},
    Overflow,
    FirstTransactionNotCoinbase,
    UnexpectedCoinbase,
    InvalidCoinbase,
    EmptyBlock,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct State {
    pub accounts: BTreeMap<String, Account>
}

impl State {
    pub fn new() -> State {
        State { accounts: BTreeMap::new()}
    }

    pub fn balance(&self, addr: &str) -> u64 {
        self.accounts.get(addr).map(|acc| acc.balance).unwrap_or(0)
    }

    pub fn next_nonce(&self, addr: &str) -> u64 {
        self.accounts.get(addr).map(|acc| acc.next_nonce).unwrap_or(0)
    }

    pub fn apply_transaction(&mut self, tx: &Transaction) -> Result<(), StateError> {
        if tx.amount == 0 {
            return Err(StateError::ZeroAmount);
        }
        if tx.from == tx.to {
            return Err(StateError::SelfTransfer);
        }
        let (sender_balance, sender_nonce) = {
            let sender = self.accounts.get(&tx.from).ok_or(StateError::UnknownSender)?;
            (sender.balance, sender.next_nonce)
        };
        if tx.nonce != sender_nonce {
            return Err(StateError::NonceMismatch { expected: sender_nonce, got: tx.nonce });
        }
        if sender_balance < tx.amount {
            return Err(StateError::InsufficientBalance { balance: sender_balance, amount: tx.amount });
        }

        let new_sender_balance = sender_balance.checked_sub(tx.amount).ok_or(StateError::Overflow)?;
        let new_sender_nonce = sender_nonce.checked_add(1).ok_or(StateError::Overflow)?;
        let new_recipient_balance = self.balance(&tx.to).checked_add(tx.amount).ok_or(StateError::Overflow)?;

        let sender = self.accounts.get_mut(&tx.from).ok_or(StateError::UnknownSender)?;
        sender.balance = new_sender_balance;
        sender.next_nonce = new_sender_nonce;

        self.accounts.entry(tx.to.clone()).or_default().balance = new_recipient_balance;
        Ok(())
    }

    pub fn apply_coinbase(&mut self, tx: &Transaction, height: u64) -> Result<(), StateError> {
        if !tx.from.is_empty() || tx.to.is_empty() || tx.amount != COINBASE_REWARD || tx.nonce != height {
            return Err(StateError::InvalidCoinbase);
        }
        let new_balance = self.balance(&tx.to).checked_add(tx.amount).ok_or(StateError::Overflow)?;
        self.accounts.entry(tx.to.clone()).or_default().balance = new_balance;
        Ok(())
    }

    pub fn apply_block(&mut self, block: &Block) -> Result<(), StateError> {
        let (first, rest) = block.transactions.split_first().ok_or(StateError::EmptyBlock)?;
        if !first.from.is_empty() {
            return Err(StateError::FirstTransactionNotCoinbase);
        }
        let mut working = self.clone();
        working.apply_coinbase(first, block.header.height)?;
        for tx in rest {
            if tx.from.is_empty() {
                return Err(StateError::UnexpectedCoinbase);
            }
            working.apply_transaction(tx)?;
        }
        *self = working;
        Ok(())
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::hash::Hash;

    fn tx(from: &str, to: &str, amount: u64, nonce: u64) -> Transaction {
        Transaction { from: from.to_string(), to: to.to_string(), amount, nonce }
    }

    fn coinbase(to: &str, height: u64) -> Transaction {
        tx("", to, COINBASE_REWARD, height)
    }

    fn make_block(height: u64, txs: Vec<Transaction>) -> Block {
        Block::new(Hash([0; 32]), height, txs, 0, 0).unwrap()
    }

    /// Состояние, где у A есть 50 монет (после coinbase в блоке 0).
    fn funded_state() -> State {
        let mut state = State::new();
        state.apply_block(&make_block(0, vec![coinbase("A", 0)])).unwrap();
        state
    }

    fn total_supply(state: &State) -> u128 {
        state.accounts.values().map(|a| u128::from(a.balance)).sum()
    }

    // ---------- пустое состояние ----------

    #[test]
    fn empty_state_has_zero_balance_and_nonce() {
        let state = State::new();
        assert_eq!(state.balance("anyone"), 0);
        assert_eq!(state.next_nonce("anyone"), 0);
        assert!(state.accounts.is_empty());
    }

    // ---------- coinbase ----------

    #[test]
    fn coinbase_credits_reward_and_accumulates() {
        let mut state = State::new();
        state.apply_coinbase(&coinbase("miner", 0), 0).unwrap();
        assert_eq!(state.balance("miner"), COINBASE_REWARD);
        state.apply_coinbase(&coinbase("miner", 1), 1).unwrap();
        assert_eq!(state.balance("miner"), 2 * COINBASE_REWARD);
    }

    #[test]
    fn coinbase_does_not_change_next_nonce() {
        let mut state = State::new();
        state.apply_coinbase(&coinbase("miner", 5), 5).unwrap();
        assert_eq!(state.next_nonce("miner"), 0);
    }

    #[test]
    fn coinbase_rejects_invalid_fields_without_changing_state() {
        let cases = [
            ("from not empty", tx("A", "miner", COINBASE_REWARD, 3), 3),
            ("empty recipient", tx("", "", COINBASE_REWARD, 3), 3),
            ("too small reward", tx("", "miner", COINBASE_REWARD - 1, 3), 3),
            ("too large reward", tx("", "miner", COINBASE_REWARD + 1, 3), 3),
            ("nonce != height", tx("", "miner", COINBASE_REWARD, 2), 3),
        ];
        for (name, transaction, height) in cases {
            let mut state = funded_state();
            let before = state.clone();
            assert_eq!(
                state.apply_coinbase(&transaction, height),
                Err(StateError::InvalidCoinbase),
                "case: {name}"
            );
            assert_eq!(state, before, "case: {name}");
        }
    }

    #[test]
    fn coinbase_overflow_leaves_state_unchanged() {
        let mut state = State::new();
        state.accounts.insert(
            "miner".to_string(),
            Account { balance: u64::MAX - 10, next_nonce: 0 },
        );
        let before = state.clone();
        assert_eq!(state.apply_coinbase(&coinbase("miner", 1), 1), Err(StateError::Overflow));
        assert_eq!(state, before);
    }

    // ---------- обычный перевод ----------

    #[test]
    fn transfer_moves_funds_and_bumps_sender_nonce() {
        let mut state = funded_state();
        state.apply_transaction(&tx("A", "B", 20, 0)).unwrap();
        assert_eq!(state.balance("A"), 30);
        assert_eq!(state.balance("B"), 20);
        assert_eq!(state.next_nonce("A"), 1);
        assert_eq!(state.next_nonce("B"), 0);
    }

    #[test]
    fn transfer_of_entire_balance_is_allowed() {
        let mut state = funded_state();
        state.apply_transaction(&tx("A", "B", COINBASE_REWARD, 0)).unwrap();
        assert_eq!(state.balance("A"), 0);
        assert_eq!(state.balance("B"), COINBASE_REWARD);
        assert_eq!(state.next_nonce("A"), 1);
    }

    #[test]
    fn recipient_can_spend_received_funds() {
        let mut state = funded_state();
        state.apply_transaction(&tx("A", "B", 20, 0)).unwrap();
        state.apply_transaction(&tx("B", "C", 5, 0)).unwrap();
        assert_eq!(state.balance("B"), 15);
        assert_eq!(state.balance("C"), 5);
        assert_eq!(state.next_nonce("B"), 1);
    }

    #[test]
    fn consecutive_nonces_work() {
        let mut state = funded_state();
        state.apply_transaction(&tx("A", "B", 1, 0)).unwrap();
        state.apply_transaction(&tx("A", "B", 1, 1)).unwrap();
        state.apply_transaction(&tx("A", "B", 1, 2)).unwrap();
        assert_eq!(state.next_nonce("A"), 3);
        assert_eq!(state.balance("A"), COINBASE_REWARD - 3);
    }

    #[test]
    fn invalid_transfers_return_specific_error_and_keep_state() {
        let cases = [
            ("zero amount", tx("A", "B", 0, 0), StateError::ZeroAmount),
            ("self transfer", tx("A", "A", 1, 0), StateError::SelfTransfer),
            ("unknown sender", tx("Z", "B", 1, 0), StateError::UnknownSender),
            ("nonce too high", tx("A", "B", 1, 1), StateError::NonceMismatch { expected: 0, got: 1 }),
            (
                "insufficient balance",
                tx("A", "B", COINBASE_REWARD + 1, 0),
                StateError::InsufficientBalance { balance: COINBASE_REWARD, amount: COINBASE_REWARD + 1 },
            ),
        ];
        for (name, transaction, expected) in cases {
            let mut state = funded_state();
            let before = state.clone();
            assert_eq!(state.apply_transaction(&transaction), Err(expected), "case: {name}");
            assert_eq!(state, before, "case: {name}");
        }
    }

    #[test]
    fn replayed_transaction_is_rejected() {
        let mut state = funded_state();
        let transaction = tx("A", "B", 10, 0);
        state.apply_transaction(&transaction).unwrap();
        let before = state.clone();
        assert_eq!(
            state.apply_transaction(&transaction),
            Err(StateError::NonceMismatch { expected: 1, got: 0 })
        );
        assert_eq!(state, before);
    }

    #[test]
    fn nonce_check_precedes_balance_check() {
        // и nonce неверный, и денег не хватает: по ADR 0005 первой срабатывает проверка nonce
        let mut state = funded_state();
        assert_eq!(
            state.apply_transaction(&tx("A", "B", COINBASE_REWARD + 100, 7)),
            Err(StateError::NonceMismatch { expected: 0, got: 7 })
        );
    }

    #[test]
    fn recipient_balance_overflow_keeps_state() {
        let mut state = funded_state();
        state.accounts.insert(
            "B".to_string(),
            Account { balance: u64::MAX - 5, next_nonce: 0 },
        );
        let before = state.clone();
        assert_eq!(state.apply_transaction(&tx("A", "B", 10, 0)), Err(StateError::Overflow));
        assert_eq!(state, before);
    }

    #[test]
    fn sender_nonce_overflow_keeps_state() {
        let mut state = State::new();
        state.accounts.insert(
            "A".to_string(),
            Account { balance: 100, next_nonce: u64::MAX },
        );
        let before = state.clone();
        assert_eq!(state.apply_transaction(&tx("A", "B", 1, u64::MAX)), Err(StateError::Overflow));
        assert_eq!(state, before);
    }

    // ---------- блок ----------

    #[test]
    fn block_applies_coinbase_and_transfers_in_order() {
        let mut state = funded_state();
        let block = make_block(
            1,
            vec![coinbase("A", 1), tx("A", "B", 30, 0), tx("B", "C", 10, 0), tx("A", "B", 5, 1)],
        );
        state.apply_block(&block).unwrap();
        assert_eq!(state.balance("A"), 50 + 50 - 30 - 5);
        assert_eq!(state.balance("B"), 30 - 10 + 5);
        assert_eq!(state.balance("C"), 10);
        assert_eq!(state.next_nonce("A"), 2);
        assert_eq!(state.next_nonce("B"), 1);
        assert_eq!(state.next_nonce("C"), 0);
    }

    #[test]
    fn block_allows_spending_funds_received_in_same_block() {
        let mut state = State::new();
        // у X нет денег до блока: coinbase начисляет ему 50, затем он тратит их же
        let block = make_block(0, vec![coinbase("X", 0), tx("X", "Y", 50, 0)]);
        state.apply_block(&block).unwrap();
        assert_eq!(state.balance("X"), 0);
        assert_eq!(state.balance("Y"), 50);
    }

    #[test]
    fn block_with_same_nonce_twice_is_rejected_atomically() {
        let mut state = funded_state();
        let before = state.clone();
        let block = make_block(1, vec![coinbase("A", 1), tx("A", "B", 1, 0), tx("A", "B", 1, 0)]);
        assert_eq!(
            state.apply_block(&block),
            Err(StateError::NonceMismatch { expected: 1, got: 0 })
        );
        assert_eq!(state, before);
    }

    #[test]
    fn block_must_start_with_coinbase() {
        let mut state = funded_state();
        let before = state.clone();
        let block = make_block(1, vec![tx("A", "B", 1, 0), coinbase("A", 1)]);
        assert_eq!(state.apply_block(&block), Err(StateError::FirstTransactionNotCoinbase));
        assert_eq!(state, before);
    }

    #[test]
    fn block_with_second_coinbase_is_rejected() {
        let mut state = funded_state();
        let before = state.clone();
        let block = make_block(1, vec![coinbase("A", 1), coinbase("A", 1)]);
        assert_eq!(state.apply_block(&block), Err(StateError::UnexpectedCoinbase));
        assert_eq!(state, before);
    }

    #[test]
    fn block_with_wrong_coinbase_reward_or_nonce_is_rejected() {
        let wrong_reward = tx("", "A", COINBASE_REWARD + 1, 1);
        let wrong_nonce = tx("", "A", COINBASE_REWARD, 2);
        for (name, bad) in [("reward", wrong_reward), ("nonce", wrong_nonce)] {
            let mut state = funded_state();
            let before = state.clone();
            let block = make_block(1, vec![bad]);
            assert_eq!(state.apply_block(&block), Err(StateError::InvalidCoinbase), "case: {name}");
            assert_eq!(state, before, "case: {name}");
        }
    }

    #[test]
    fn empty_block_is_rejected() {
        let mut state = funded_state();
        let before = state.clone();
        let mut block = make_block(1, vec![coinbase("A", 1)]);
        block.transactions.clear(); // Block::new пустой список не принимает, очищаем вручную
        assert_eq!(state.apply_block(&block), Err(StateError::EmptyBlock));
        assert_eq!(state, before);
    }

    #[test]
    fn failed_block_does_not_apply_earlier_transactions() {
        // атомарность: coinbase и первый перевод валидны, третья транзакция нет
        let mut state = funded_state();
        let before = state.clone();
        let block = make_block(
            1,
            vec![coinbase("A", 1), tx("A", "B", 10, 0), tx("A", "B", 1_000_000, 1)],
        );
        assert!(matches!(
            state.apply_block(&block),
            Err(StateError::InsufficientBalance { .. })
        ));
        assert_eq!(state, before);
        assert_eq!(state.balance("B"), 0);
    }

    #[test]
    fn transfers_preserve_total_supply_and_blocks_add_only_reward() {
        let mut state = State::new();
        let mut expected_supply: u128 = 0;
        let blocks = [
            make_block(0, vec![coinbase("A", 0)]),
            make_block(1, vec![coinbase("B", 1), tx("A", "B", 20, 0), tx("A", "C", 5, 1)]),
            make_block(2, vec![coinbase("A", 2), tx("B", "A", 40, 0), tx("C", "D", 5, 0)]),
        ];
        for block in &blocks {
            state.apply_block(block).unwrap();
            expected_supply += u128::from(COINBASE_REWARD);
            assert_eq!(total_supply(&state), expected_supply);
        }
    }
}

