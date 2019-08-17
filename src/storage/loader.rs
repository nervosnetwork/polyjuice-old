use super::{
    build_receipt_key, load_latest_out_points, BlockNumber, Error, EthAccount, EthAddress,
    EthBasicReceipt, EthCell, TransactionReceipt, BLOCK_KEY, CONTRACT_LOCK_CODE_DEP_KEY,
    LOCK_CODE_DEP_KEY,
};
use crate::{CODE_HASH_CONTRACT_LOCK, CODE_HASH_LOCK};
use bincode::deserialize;
use bytes::Bytes;
use ckb_hash::blake2b_256;
use ckb_jsonrpc_types::{CellOutPoint, OutPoint, TxStatus};
use ckb_sdk::HttpRpcClient;
use numext_fixed_hash::H256;
use rocksdb::DB;
use std::sync::Arc;

pub struct Loader {
    pub db: Arc<DB>,
    ckb_uri: String,
}

impl Loader {
    pub fn new(db: Arc<DB>, ckb_uri: &str) -> Result<Self, Error> {
        let loader = Loader {
            db,
            ckb_uri: ckb_uri.to_string(),
        };
        {
            let lock_out_point = loader.load_lock_out_point()?;
            let lock_cell = loader
                .ckb_client()
                .get_live_cell(OutPoint {
                    cell: Some(lock_out_point),
                    block_hash: None,
                })
                .call()?;
            if lock_cell.cell.is_none() {
                return Err(Error::MalformedData("Lock cell is missing!".to_string()));
            }
            let lock_cell = lock_cell.cell.unwrap();
            if blake2b_256(lock_cell.data.as_bytes()) != CODE_HASH_LOCK {
                return Err(Error::MalformedData(
                    "Lock data hash does not match!".to_string(),
                ));
            }
        }
        {
            let lock_out_point = loader.load_contract_lock_out_point()?;
            let lock_cell = loader
                .ckb_client()
                .get_live_cell(OutPoint {
                    cell: Some(lock_out_point),
                    block_hash: None,
                })
                .call()?;
            if lock_cell.cell.is_none() {
                return Err(Error::MalformedData(
                    "Contract lock cell is missing!".to_string(),
                ));
            }
            let lock_cell = lock_cell.cell.unwrap();
            if blake2b_256(lock_cell.data.as_bytes()) != CODE_HASH_CONTRACT_LOCK {
                return Err(Error::MalformedData(
                    "Contract lock data hash does not match!".to_string(),
                ));
            }
        }
        Ok(loader)
    }

    pub fn load_lock_out_point(&self) -> Result<CellOutPoint, Error> {
        let out_point = deserialize(&self.db.get(LOCK_CODE_DEP_KEY)?.ok_or(
            Error::MalformedData("Lock code is not on chain!".to_string()),
        )?)?;
        Ok(out_point)
    }

    pub fn load_contract_lock_out_point(&self) -> Result<CellOutPoint, Error> {
        let out_point = deserialize(&self.db.get(CONTRACT_LOCK_CODE_DEP_KEY)?.ok_or(
            Error::MalformedData("Contract lock code is not on chain!".to_string()),
        )?)?;
        Ok(out_point)
    }

    pub fn load_account(
        &self,
        eth_address: &EthAddress,
        block_number: u64,
        load_spent: bool,
    ) -> Result<Option<EthAccount>, Error> {
        let out_points = load_latest_out_points(&self.db, eth_address, block_number)?;
        let cells = self.load_cells(&out_points, load_spent)?;
        let (main_cells, fund_cells): (Vec<EthCell>, Vec<EthCell>) =
            cells.into_iter().partition(|cell| cell.0.data.len() > 0);
        if main_cells.len() > 1 {
            return Err(Error::MalformedData("Invalid account cells".to_string()));
        }
        Ok(Some(EthAccount {
            main_cell: main_cells.get(0).cloned(),
            fund_cells,
        }))
    }

    pub fn load_receipt(&self, hash: &H256) -> Result<Option<TransactionReceipt>, Error> {
        let basic_receipt: EthBasicReceipt = match self.db.get(&build_receipt_key(hash))? {
            Some(data) => deserialize(&data)?,
            None => return Ok(None),
        };
        let transaction = match self
            .ckb_client()
            .get_transaction(basic_receipt.ckb_transaction_hash.clone())
            .call()?
            .0
        {
            Some(tx) => tx,
            None => return Ok(None),
        };
        // Ideally we should test for status, but Status is not exposed in this
        // version yet.
        if transaction.tx_status.block_hash.is_none() {
            return Ok(None);
        }
        Ok(Some(TransactionReceipt::from(
            &basic_receipt,
            &transaction.transaction,
            &transaction.tx_status.block_hash.unwrap(),
        )?))
    }

    pub fn resolve_block_number(&self, block_number: BlockNumber) -> Result<u64, Error> {
        match block_number {
            BlockNumber::Latest => self.tip_block_number(),
            BlockNumber::Number(n) => Ok(n),
        }
    }

    pub fn tip_block_number(&self) -> Result<u64, Error> {
        let last_processed: (u64, Bytes) = match self.db.get(BLOCK_KEY)? {
            Some(data) => deserialize(&data)?,
            None => (0, Bytes::new()),
        };
        Ok(last_processed.0)
    }

    pub fn ckb_client(&self) -> HttpRpcClient {
        HttpRpcClient::from_uri(&self.ckb_uri)
    }

    fn load_cells(
        &self,
        out_points: &[CellOutPoint],
        load_spent: bool,
    ) -> Result<Vec<EthCell>, Error> {
        let mut client = self.ckb_client();
        out_points.iter().try_fold(vec![], |mut cells, out_point| {
            let cell_with_status = client
                .get_live_cell(OutPoint {
                    cell: Some(out_point.clone()),
                    block_hash: None,
                })
                .call()?;
            if cell_with_status.status == "live" {
                cells.push(EthCell(
                    cell_with_status.cell.expect("this cannot be empty!"),
                    out_point.clone(),
                ));
                return Ok(cells);
            } else if cell_with_status.status == "dead" && load_spent {
                if let Some(transaction_with_status) =
                    client.get_transaction(out_point.tx_hash.clone()).call()?.0
                {
                    // This is a fallback solution since Status is not exposed now
                    let dummy_tx_status = TxStatus::committed(out_point.tx_hash.clone());
                    if transaction_with_status.tx_status.status == dummy_tx_status.status {
                        let cell = transaction_with_status.transaction.inner.outputs
                            [out_point.index.0 as usize]
                            .clone();
                        cells.push(EthCell(cell, out_point.clone()));
                        return Ok(cells);
                    }
                }
            }
            Err(Error::InvalidOutPoint)
        })
    }
}
