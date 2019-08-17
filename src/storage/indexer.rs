use super::{
    build_block_added_out_points_key, build_block_hash_key, build_block_receipt_hashes_key,
    build_block_spent_out_points_key, build_eth_key, build_out_point_key, build_receipt_key,
    load_latest_out_points, Error, EthAddress, EthBasicReceipt, EthTransaction, BLOCK_KEY,
};
use crate::{CODE_HASH_CONTRACT_LOCK, CODE_HASH_LOCK};
use bincode::{deserialize, serialize};
use bytes::Bytes;
use ckb_core::transaction::Witness;
use ckb_jsonrpc_types::{BlockNumber, CellOutPoint, Unsigned};
use ckb_sdk::HttpRpcClient;
use numext_fixed_hash::H256;
use numext_fixed_uint::U256;
use rocksdb::{WriteBatch, DB};
use std::collections::{HashMap, HashSet};
use std::iter::FromIterator;
use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;

pub struct Indexer {
    pub db: Arc<DB>,
    pub client: HttpRpcClient,
}

impl Indexer {
    pub fn from(db: Arc<DB>, ckb_uri: &str) -> Self {
        Indexer {
            db,
            client: HttpRpcClient::from_uri(ckb_uri),
        }
    }

    // Ideally this should never return. The caller is responsible for wrapping
    // it into a separate thread.
    pub fn index(&mut self) -> Result<(), Error> {
        loop {
            let last_processed: (u64, Bytes) = match self.db.get(BLOCK_KEY)? {
                Some(data) => deserialize(&data)?,
                None => (0, Bytes::new()),
            };
            let (block_number, block_hash) = last_processed;

            if block_number > 0 {
                if let Some(header) = self
                    .client
                    .get_header_by_number(BlockNumber(block_number))
                    .call()?
                    .0
                {
                    if header.hash.as_bytes() != block_hash.as_ref() {
                        info!(
                            "reverting block: {:x}({}) due to fork",
                            header.hash, block_number
                        );
                        // There is a fork, revert current block and start
                        // a new loop iteration.
                        let mut batch = WriteBatch::default();
                        let receipt_hashes_key = build_block_receipt_hashes_key(block_number);
                        let receipt_hashes: Vec<H256> = deserialize(
                            self.db
                                .get(&receipt_hashes_key)?
                                .ok_or(Error::MalformedData(
                                    "Receipt hash key does not exist!".to_string(),
                                ))?
                                .as_ref(),
                        )?;
                        batch.delete(&receipt_hashes_key)?;
                        for receipt_hash in &receipt_hashes {
                            let key = build_receipt_key(&receipt_hash);
                            batch.delete(&key)?;
                        }
                        let added_out_points_key = build_block_added_out_points_key(block_number);
                        let added_out_points: Vec<CellOutPoint> = deserialize(
                            self.db
                                .get(&added_out_points_key)?
                                .ok_or(Error::MalformedData(
                                    "Added out point key does not exist!".to_string(),
                                ))?
                                .as_ref(),
                        )?;
                        batch.delete(&added_out_points_key)?;
                        let spent_out_points_key = build_block_spent_out_points_key(block_number);
                        batch.delete(&spent_out_points_key)?;
                        let mut eth_addresses: HashSet<EthAddress> = HashSet::new();
                        for out_point in &added_out_points {
                            let key = build_out_point_key(&out_point)?;
                            let eth_address = self.db.get(&key)?.ok_or(Error::MalformedData(
                                "Out point key does not exist!".to_string(),
                            ))?;
                            eth_addresses.insert(eth_address.as_ref().into());
                            batch.delete(&key)?;
                        }
                        for eth_address in &eth_addresses {
                            let first_key = build_eth_key(eth_address, Some(block_number));
                            let last_key = build_eth_key(eth_address, Some(block_number + 1));
                            batch.delete_range(&first_key, &last_key)?;
                        }
                        if block_number > 1 {
                            let previous_block_number = block_number - 1;
                            let previous_block_hash_key =
                                build_block_hash_key(previous_block_number);
                            let previous_block_hash: Bytes = self
                                .db
                                .get(&previous_block_hash_key)?
                                .ok_or(Error::MalformedData(
                                    "Previous block hash key does not exist!".to_string(),
                                ))?
                                .as_ref()
                                .into();
                            batch.put(
                                BLOCK_KEY,
                                serialize(&(previous_block_number, previous_block_hash))?,
                            )?;
                        } else {
                            batch.delete(BLOCK_KEY)?;
                        }
                        self.db.write(batch)?;

                        continue;
                    }
                }
            }

            let next_block_number = block_number + 1;
            if let Some(next_block) = self
                .client
                .get_block_by_number(BlockNumber(next_block_number))
                .call()?
                .0
            {
                info!(
                    "indexing block: {:x}({})",
                    next_block.header.hash, next_block_number
                );
                let mut diff_cells: HashMap<
                    EthAddress,
                    (HashSet<CellOutPoint>, HashSet<CellOutPoint>),
                > = HashMap::default();
                let mut receipts: HashMap<H256, EthBasicReceipt> = HashMap::default();
                let mut current_transaction_index = 1;
                let mut current_cumulated_gas = U256::zero();
                // Process the block here.
                for transaction in next_block.transactions {
                    if transaction
                        .inner
                        .outputs
                        .iter()
                        .any(|o| o.lock.code_hash.as_bytes() == CODE_HASH_LOCK)
                    {
                        // Index Ethereum transactions for receipts
                        for (i, witness) in transaction.inner.witnesses.iter().enumerate() {
                            // TODO: when data is properly exposed, we don't need
                            // this.
                            let witness: Witness = witness.clone().into();
                            if witness.len() == 1 {
                                let tx = match EthTransaction::from_raw(witness[0].clone()) {
                                    Ok(tx) => tx,
                                    Err(e) => {
                                        warn!("Skipping witness at {:x} {} since we cannot parse it: {:?}", transaction.hash, i, e);
                                        continue;
                                    }
                                };
                                current_cumulated_gas =
                                    current_cumulated_gas.checked_add(&tx.fees()?).ok_or(
                                        Error::MalformedData("Wei addition overflow!".to_string()),
                                    )?;
                                receipts.insert(
                                    tx.hash(),
                                    EthBasicReceipt {
                                        transaction_index: current_transaction_index,
                                        cumulative_gas: current_cumulated_gas.clone(),
                                        witness_index: i as u64,
                                        ckb_transaction_hash: transaction.hash.clone(),
                                        block_number: next_block_number,
                                    },
                                );
                                current_transaction_index += 1;
                            }
                        }
                    }

                    // Purge spent cells in inputs
                    for input in transaction.inner.inputs {
                        if let Some(cell_out_point) = &input.previous_output.cell {
                            let cell_out_point_key = build_out_point_key(&cell_out_point)?;
;
                            if let Some(eth_address) = self.db.get(&cell_out_point_key)? {
                                diff_cells
                                    .entry(eth_address.as_ref().into())
                                    .and_modify(|e| {
                                        e.0.insert(cell_out_point.clone());
                                    })
                                    .or_insert_with(|| {
                                        let mut spent_cells = HashSet::new();
                                        spent_cells.insert(cell_out_point.clone());
                                        (spent_cells, HashSet::new())
                                    });
                            }
                        }
                    }

                    for (i, output) in transaction.inner.outputs.iter().enumerate() {
                        if (output.lock.code_hash.as_bytes() == CODE_HASH_LOCK
                            || output.lock.code_hash.as_bytes() == CODE_HASH_CONTRACT_LOCK)
                            && output.lock.args.len() == 1
                            && output.lock.args[0].len() == 20
                        {
                            // Index current cell
                            let cell_out_point = CellOutPoint {
                                tx_hash: transaction.hash.clone(),
                                index: Unsigned(i as u64),
                            };
                            let eth_address = output.lock.args[0].as_bytes().into();
                            diff_cells
                                .entry(eth_address)
                                .and_modify(|e| {
                                    e.1.insert(cell_out_point.clone());
                                })
                                .or_insert_with(|| {
                                    let mut added_cells = HashSet::new();
                                    added_cells.insert(cell_out_point.clone());
                                    (HashSet::new(), added_cells)
                                });
                        }
                    }
                }

                let mut batch = WriteBatch::default();
                batch.put(
                    BLOCK_KEY,
                    serialize(&(
                        next_block_number,
                        Bytes::from(next_block.header.hash.as_bytes()),
                    ))?,
                )?;
                batch.put(
                    &build_block_hash_key(next_block_number),
                    next_block.header.hash.clone(),
                )?;

                let mut all_spent_out_points = vec![];
                let mut all_added_out_points = vec![];
                for (eth_address, (spent_out_points, added_out_points)) in diff_cells {
                    let last_out_points =
                        load_latest_out_points(&self.db, &eth_address, block_number)?;
                    let new_out_points: Vec<CellOutPoint> =
                        HashSet::from_iter(last_out_points.into_iter())
                            .difference(&spent_out_points)
                            .cloned()
                            .collect::<HashSet<CellOutPoint>>()
                            .union(&added_out_points)
                            .cloned()
                            .collect();
                    let new_key = build_eth_key(&eth_address, Some(next_block_number));
                    batch.put(&new_key, serialize(&new_out_points)?)?;

                    for out_point in &spent_out_points {
                        all_spent_out_points.push(out_point.clone());
                    }

                    for out_point in &added_out_points {
                        all_added_out_points.push(out_point.clone());
                        batch.put(&build_out_point_key(&out_point)?, &eth_address)?;
                    }
                }
                batch.put(
                    &build_block_spent_out_points_key(next_block_number),
                    serialize(&all_spent_out_points)?,
                )?;
                batch.put(
                    &build_block_added_out_points_key(next_block_number),
                    serialize(&all_added_out_points)?,
                )?;

                for (tx_hash, receipt) in &receipts {
                    batch.put(&build_receipt_key(&tx_hash), serialize(&receipt)?)?;
                }
                let receipt_hashes: Vec<H256> = receipts.keys().cloned().collect();
                batch.put(
                    &build_block_receipt_hashes_key(next_block_number),
                    serialize(&receipt_hashes)?,
                )?;

                self.db.write(batch)?;
            } else {
                // No new block yet.
                // TODO: purge old blocks
                debug!("no new block available, sleeping ...");
                sleep(Duration::from_secs(3));
            }
        }
    }
}
