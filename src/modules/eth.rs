use crate::{
    modules::TransactionCall,
    storage::{
        serialize_u64, BlockNumber, EthAddress, EthTransaction, Loader, Runner, TransactionReceipt,
    },
    Error as CrateError,
};
use ckb_jsonrpc_types::JsonBytes;
use jsonrpc_core::{Error, Result};
use jsonrpc_derive::rpc;
use numext_fixed_hash::H256;
use numext_fixed_uint::U256;
use std::convert::TryFrom;
use std::sync::Arc;

#[rpc]
pub trait EthRpc {
    #[rpc(name = "eth_blockNumber")]
    fn block_number(&self) -> Result<String>;

    #[rpc(name = "eth_getBalance")]
    fn get_balance(&self, eth_address: String, block_number: Option<String>) -> Result<U256>;

    #[rpc(name = "eth_sendRawTransaction")]
    fn send_raw_transaction(&self, raw: JsonBytes) -> Result<H256>;

    #[rpc(name = "eth_getTransactionReceipt")]
    fn get_transaction_receipt(&self, hash: H256) -> Result<Option<TransactionReceipt>>;

    #[rpc(name = "eth_getTransactionCount")]
    fn get_transaction_count(
        &self,
        eth_address: String,
        block_number: Option<String>,
    ) -> Result<U256>;

    #[rpc(name = "eth_getStorageAt")]
    fn get_storage_at(
        &self,
        eth_address: String,
        position: U256,
        block_number: Option<String>,
    ) -> Result<H256>;

    #[rpc(name = "eth_call")]
    fn eth_call(&self, call: TransactionCall, block_number: Option<String>) -> Result<JsonBytes>;
}

pub struct EthRpcImpl {
    pub loader: Arc<Loader>,
}

impl EthRpc for EthRpcImpl {
    fn block_number(&self) -> Result<String> {
        Ok(serialize_u64(
            self.loader
                .tip_block_number()
                .map_err(|_| Error::internal_error())?,
        ))
    }

    fn get_balance(&self, eth_address: String, block_number: Option<String>) -> Result<U256> {
        let eth_address = EthAddress::parse(&eth_address)?;
        let block_number = self
            .loader
            .resolve_block_number(BlockNumber::parse_with_default(&block_number)?)?;
        let account = self.loader.load_account(&eth_address, block_number, true)?;
        let wei = match account {
            Some(account) => account.total_capacities_in_wei()?,
            None => U256::zero(),
        };
        Ok(wei)
    }

    fn get_transaction_count(
        &self,
        eth_address: String,
        block_number: Option<String>,
    ) -> Result<U256> {
        let eth_address = EthAddress::parse(&eth_address)?;
        let block_number = self
            .loader
            .resolve_block_number(BlockNumber::parse_with_default(&block_number)?)?;
        let account = self.loader.load_account(&eth_address, block_number, true)?;
        let wei = match account {
            Some(account) => account.next_nonce()?,
            None => U256::zero(),
        };
        Ok(wei)
    }

    fn send_raw_transaction(&self, raw: JsonBytes) -> Result<H256> {
        let tx = EthTransaction::from_raw(raw.into_bytes())?;
        let block_number = self.loader.tip_block_number()?;
        let ckb_transaction = Runner {
            loader: &self.loader,
            tx: &tx,
            block_number,
        }
        .run()?;
        let tx_hash = self
            .loader
            .ckb_client()
            .send_transaction(ckb_transaction)
            .call()
            .map_err(|e| CrateError::Rpc(e.to_string()))?;
        debug!("Sent CKB transaction: {:x}", tx_hash);
        Ok(tx.hash())
    }

    fn get_transaction_receipt(&self, hash: H256) -> Result<Option<TransactionReceipt>> {
        let receipt = self.loader.load_receipt(&hash)?;
        Ok(receipt)
    }

    fn get_storage_at(
        &self,
        eth_address: String,
        position: U256,
        block_number: Option<String>,
    ) -> Result<H256> {
        let eth_address = EthAddress::parse(&eth_address)?;
        let block_number = self
            .loader
            .resolve_block_number(BlockNumber::parse_with_default(&block_number)?)?;
        let account = self
            .loader
            .load_account(&eth_address, block_number, true)?
            .ok_or(CrateError::MalformedData(
                "Contract does not exist!".to_string(),
            ))?;
        if !account.contract_account()? {
            return Err(CrateError::MalformedData(
                "Specified account is not a contract!".to_string(),
            )
            .into());
        }
        let contract_data = account.contract_data()?;
        let value = contract_data
            .storage
            .get(&position)
            .cloned()
            .unwrap_or(U256::zero());
        Ok(value.to_be_bytes().into())
    }

    fn eth_call(&self, call: TransactionCall, block_number: Option<String>) -> Result<JsonBytes> {
        let tx = EthTransaction::try_from(call)?;
        let block_number = self
            .loader
            .resolve_block_number(BlockNumber::parse_with_default(&block_number)?)?;
        let result = Runner {
            loader: &self.loader,
            tx: &tx,
            block_number,
        }
        .call()?;
        Ok(JsonBytes::from_bytes(result))
    }
}
