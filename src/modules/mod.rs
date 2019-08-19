mod eth;
mod web3;

use crate::{
    storage::{EthAddress, EthTransaction},
    Error,
};
use bytes::Bytes;
use ckb_jsonrpc_types::JsonBytes;
use numext_fixed_uint::U256;
use serde_derive::{Deserialize, Serialize};
use std::convert::TryFrom;

pub use eth::{EthRpc, EthRpcImpl};
pub use web3::{Web3Rpc, Web3RpcImpl};

#[derive(Serialize, Deserialize)]
pub struct TransactionCall {
    pub from: Option<String>,
    pub to: String,
    pub gas: Option<U256>,
    pub gas_price: Option<U256>,
    pub value: Option<U256>,
    pub data: Option<JsonBytes>,
}

impl TryFrom<TransactionCall> for EthTransaction {
    type Error = Error;

    fn try_from(call: TransactionCall) -> Result<Self, Self::Error> {
        Ok(EthTransaction {
            nonce: 0,
            gas_price: call.gas_price.unwrap_or(U256::one()),
            gas_limit: call.gas.unwrap_or(U256::max_value()),
            to: Some(EthAddress::parse(&call.to)?),
            value: call.value.unwrap_or(U256::zero()),
            data: call.data.map(|data| data.into_bytes()),
            v: 0,
            r: U256::zero(),
            s: U256::zero(),
            from: match call.from {
                Some(from) => EthAddress::parse(&from)?,
                None => EthAddress::default(),
            },
            raw: Bytes::default(),
        })
    }
}
