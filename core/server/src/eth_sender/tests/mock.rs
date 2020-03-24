//! Mocking utilities for tests.

// Built-in deps
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
// External uses
use futures::channel::mpsc;
use web3::contract::{tokens::Tokenize, Options};
use web3::types::{H256, U256};
// Workspace uses
use eth_client::SignedCallResult;
use models::{ethereum::ETHOperation, Operation};
// Local uses
use super::ETHSender;
use crate::eth_sender::database::DatabaseAccess;
use crate::eth_sender::ethereum_interface::EthereumInterface;
use crate::eth_sender::transactions::{
    ETHStats, ExecutedTxStatus, OperationETHState, OperationType, TransactionETHState,
};

const CHANNEL_CAPACITY: usize = 16;

/// Mock database is capable of recording all the incoming requests for the further analysis.
#[derive(Debug, Default)]
pub(super) struct MockDatabase {
    restore_state: VecDeque<OperationETHState>,
    unconfirmed_operations: RefCell<HashMap<H256, TransactionETHState>>,
    confirmed_operations: RefCell<HashMap<H256, TransactionETHState>>,
    nonce: Cell<i64>,
    stats: RefCell<ETHStats>,
}

impl MockDatabase {
    /// Creates a database with emulation of previously stored uncommitted requests.
    pub fn with_restorable_state(
        restore_state: impl IntoIterator<Item = OperationETHState>,
        stats: ETHStats,
    ) -> Self {
        let restore_state: VecDeque<_> = restore_state.into_iter().collect();
        let nonce = restore_state.iter().fold(0, |acc, op| acc + op.txs.len());
        Self {
            restore_state,
            nonce: Cell::new(nonce as i64),
            stats: RefCell::new(stats),
            ..Default::default()
        }
    }

    /// Ensures that the provided transaction is stored in the database and not confirmed yet.
    pub fn assert_stored(&self, tx: &TransactionETHState) {
        assert_eq!(
            self.unconfirmed_operations.borrow().get(&tx.signed_tx.hash),
            Some(tx)
        );

        assert!(self
            .confirmed_operations
            .borrow()
            .get(&tx.signed_tx.hash)
            .is_none());
    }

    /// Ensures that the provided transaction is not stored in the database.
    pub fn assert_not_stored(&self, tx: &TransactionETHState) {
        assert!(self
            .confirmed_operations
            .borrow()
            .get(&tx.signed_tx.hash)
            .is_none());

        assert!(self
            .unconfirmed_operations
            .borrow()
            .get(&tx.signed_tx.hash)
            .is_none());
    }

    /// Ensures that the provided transaction is stored as confirmed.
    pub fn assert_confirmed(&self, tx: &TransactionETHState) {
        assert_eq!(
            self.confirmed_operations.borrow().get(&tx.signed_tx.hash),
            Some(tx)
        );

        assert!(self
            .unconfirmed_operations
            .borrow()
            .get(&tx.signed_tx.hash)
            .is_none());
    }
}

impl DatabaseAccess for MockDatabase {
    fn restore_state(&self) -> Result<Vec<(ETHOperation, Option<Operation>)>, failure::Error> {
        Ok(self.restore_state.clone())
    }

    fn save_unconfirmed_operation(&self, tx: &TransactionETHState) -> Result<(), failure::Error> {
        self.unconfirmed_operations
            .borrow_mut()
            .insert(tx.signed_tx.hash, tx.clone());

        Ok(())
    }

    fn confirm_operation(&self, hash: &H256) -> Result<(), failure::Error> {
        let mut unconfirmed_operations = self.unconfirmed_operations.borrow_mut();
        assert!(
            unconfirmed_operations.contains_key(hash),
            "Request to confirm operation that was not stored"
        );

        let operation = unconfirmed_operations.remove(hash).unwrap();
        self.confirmed_operations
            .borrow_mut()
            .insert(*hash, operation);

        Ok(())
    }

    fn next_nonce(&self) -> Result<i64, failure::Error> {
        let old_value = self.nonce.get();
        let new_value = old_value + 1;
        self.nonce.set(new_value);

        Ok(old_value)
    }

    fn load_stats(&self) -> Result<ETHStats, failure::Error> {
        Ok(self.stats.borrow().clone())
    }

    fn report_created_operation(
        &self,
        operation_type: OperationType,
    ) -> Result<(), failure::Error> {
        let mut stats = self.stats.borrow_mut();

        match operation_type {
            OperationType::Commit => {
                stats.commit_ops += 1;
            }
            OperationType::Verify => {
                stats.verify_ops += 1;
            }
            OperationType::Withdraw => {
                stats.withdraw_ops += 1;
            }
        }

        Ok(())
    }
}

/// Mock Ethereum client is capable of recording all the incoming requests for the further analysis.
#[derive(Debug)]
pub(super) struct MockEthereum {
    pub block_number: u64,
    pub gas_price: U256,
    pub tx_statuses: RefCell<HashMap<H256, ExecutedTxStatus>>,
    pub sent_txs: RefCell<HashMap<H256, SignedCallResult>>,
}

impl Default for MockEthereum {
    fn default() -> Self {
        Self {
            block_number: 1,
            gas_price: 100.into(),
            tx_statuses: Default::default(),
            sent_txs: Default::default(),
        }
    }
}

impl MockEthereum {
    /// A fake `sha256` hasher, which calculates an `std::hash` instead.
    /// This is done for simplicity and it's also much faster.
    pub fn fake_sha256(data: &[u8]) -> H256 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::Hasher;

        let mut hasher = DefaultHasher::new();
        hasher.write(data);

        let result = hasher.finish();

        H256::from_low_u64_ne(result)
    }

    /// Checks that there was a request to send the provided transaction.
    pub fn assert_sent(&self, tx: &TransactionETHState) {
        assert_eq!(
            self.sent_txs.borrow().get(&tx.signed_tx.hash),
            Some(&tx.signed_tx)
        );
    }

    /// Checks that there was a request to send a transaction with the provided hash.
    pub fn assert_sent_by_hash(&self, hash: &H256) {
        assert!(
            self.sent_txs.borrow().get(hash).is_some(),
            format!("Transaction with hash {:?} was not sent", hash),
        );
    }

    /// Adds an response for the sent transaction for `ETHSender` to receive.
    pub fn add_execution(&mut self, tx: &TransactionETHState, status: &ExecutedTxStatus) {
        self.tx_statuses
            .borrow_mut()
            .insert(tx.signed_tx.hash, status.clone());
    }

    /// Increments the blocks by a provided `confirmations` and marks the sent transaction
    /// as a success.
    pub fn add_successfull_execution(&mut self, tx_hash: H256, confirmations: u64) {
        self.block_number += confirmations;

        let status = ExecutedTxStatus {
            confirmations,
            success: true,
            receipt: None,
        };
        self.tx_statuses.borrow_mut().insert(tx_hash, status);
    }

    /// Same as `add_successfull_execution`, but marks the transaction as a failure.
    pub fn add_failed_execution(&mut self, tx: &TransactionETHState, confirmations: u64) {
        self.block_number += confirmations;

        let status = ExecutedTxStatus {
            confirmations,
            success: false,
            receipt: Some(Default::default()),
        };
        self.tx_statuses
            .borrow_mut()
            .insert(tx.signed_tx.hash, status);
    }
}

impl EthereumInterface for MockEthereum {
    fn get_tx_status(&self, hash: &H256) -> Result<Option<ExecutedTxStatus>, failure::Error> {
        Ok(self.tx_statuses.borrow().get(hash).cloned())
    }

    fn block_number(&self) -> Result<u64, failure::Error> {
        Ok(self.block_number)
    }

    fn gas_price(&self) -> Result<U256, failure::Error> {
        Ok(self.gas_price)
    }

    fn send_tx(&self, signed_tx: &SignedCallResult) -> Result<(), failure::Error> {
        self.sent_txs
            .borrow_mut()
            .insert(signed_tx.hash, signed_tx.clone());

        Ok(())
    }

    fn encode_tx_data<P: Tokenize>(&self, _func: &str, params: P) -> Vec<u8> {
        ethabi::encode(params.into_tokens().as_ref())
    }

    fn sign_prepared_tx(
        &self,
        raw_tx: Vec<u8>,
        options: Options,
    ) -> Result<SignedCallResult, failure::Error> {
        let gas_price = options.gas_price.unwrap_or(self.gas_price);
        let nonce = options.nonce.expect("Nonce must be set for every tx");

        // Nonce and gas_price are appended to distinguish the same transactions
        // with different gas by their hash in tests.
        let mut data_for_hash = raw_tx.clone();
        data_for_hash.append(&mut ethabi::encode(gas_price.into_tokens().as_ref()));
        data_for_hash.append(&mut ethabi::encode(nonce.into_tokens().as_ref()));
        let hash = Self::fake_sha256(data_for_hash.as_ref()); // Okay for test purposes.

        Ok(SignedCallResult {
            raw_tx,
            gas_price,
            nonce,
            hash,
        })
    }
}

/// Creates a default `ETHSender` with mock Ethereum connection/database and no operations in DB.
/// Returns the `ETHSender` itself along with communication channels to interact with it.
pub(super) fn default_eth_sender() -> (
    ETHSender<MockEthereum, MockDatabase>,
    mpsc::Sender<Operation>,
    mpsc::Receiver<Operation>,
) {
    restored_eth_sender(Vec::new(), Default::default())
}

/// Creates an `ETHSender` with mock Ethereum connection/database and restores its state "from DB".
/// Returns the `ETHSender` itself along with communication channels to interact with it.
pub(super) fn restored_eth_sender(
    restore_state: impl IntoIterator<Item = OperationETHState>,
    stats: ETHStats,
) -> (
    ETHSender<MockEthereum, MockDatabase>,
    mpsc::Sender<Operation>,
    mpsc::Receiver<Operation>,
) {
    let ethereum = MockEthereum::default();
    let db = MockDatabase::with_restorable_state(restore_state, stats);

    let (operation_sender, operation_receiver) = mpsc::channel(CHANNEL_CAPACITY);
    let (notify_sender, notify_receiver) = mpsc::channel(CHANNEL_CAPACITY);

    (
        ETHSender::new(db, ethereum, operation_receiver, notify_sender),
        operation_sender,
        notify_receiver,
    )
}

/// Behaves the same as `ETHSender::sign_new_tx`, but does not affect nonce.
/// This method should be used to create expected tx copies which won't affect
/// the internal `ETHSender` state.
pub(super) fn create_signed_tx(
    eth_sender: &ETHSender<MockEthereum, MockDatabase>,
    operation: &Operation,
    deadline_block: u64,
    nonce: i64,
) -> TransactionETHState {
    let mut options = Options::default();
    options.nonce = Some(nonce.into());

    let raw_tx = eth_sender.operation_to_raw_tx(&operation);
    let signed_tx = eth_sender
        .ethereum
        .sign_prepared_tx(raw_tx, options)
        .unwrap();

    TransactionETHState {
        op_id: operation.id.unwrap(),
        deadline_block,
        signed_tx,
    }
}
