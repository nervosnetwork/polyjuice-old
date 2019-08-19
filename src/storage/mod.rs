mod indexer;
mod loader;
mod runner;

use crate::{Error, CODE_HASH_CONTRACT_LOCK, SECP256K1};
use bincode::{deserialize, serialize};
use bytes::{BufMut, Bytes, BytesMut};
use ckb_core::transaction::Witness;
use ckb_jsonrpc_types::{Capacity, CellOutPoint, CellOutput, JsonBytes, TransactionView};
use ckb_occupied_capacity::AsCapacity;
use ethereum_types::Address as ParityAddress;
use faster_hex::hex_decode;
use numext_fixed_hash::H256;
use numext_fixed_uint::{u256, U256};
use rlp::{encode_list, Rlp};
use rocksdb::DB;
use secp256k1::{Message, RecoverableSignature, RecoveryId};
use serde_derive::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::TryFrom;
use std::sync::Arc;
use tiny_keccak::keccak256;

pub use indexer::Indexer;
pub use loader::Loader;
pub use runner::Runner;

pub const CHAIN_ID: u64 = 1;
pub const BLOCK_KEY: &str = "block";
pub const LOCK_CODE_DEP_KEY: &str = "lock_dep";
pub const CONTRACT_LOCK_CODE_DEP_KEY: &str = "contract_lock_dep";

pub const SHANNON_TO_WEI: U256 = u256!("10_000_000_000");

#[derive(PartialEq, Eq, Hash, Clone, Debug)]
pub enum CellType {
    NormalMainCell = 1,
    ContractMainCell = 2,
}

impl TryFrom<u8> for CellType {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(CellType::NormalMainCell),
            2 => Ok(CellType::ContractMainCell),
            _ => Err(Error::MalformedData(
                format!("Invalid cell type: {}", value).to_string(),
            )),
        }
    }
}

pub fn build_eth_key(eth_address: &EthAddress, block_number: Option<u64>) -> Bytes {
    let mut key = BytesMut::from("e:");
    key.extend_from_slice(&eth_address.0);
    key.extend_from_slice(b":");
    if let Some(block_number) = block_number {
        key.reserve(8);
        key.put_u64_le(block_number);
    }
    key.freeze()
}

pub fn build_out_point_key(out_point: &CellOutPoint) -> Result<Bytes, Error> {
    let mut key = BytesMut::from("o:");
    key.extend_from_slice(&serialize(out_point)?);
    Ok(key.freeze())
}

pub fn build_block_hash_key(block_number: u64) -> Bytes {
    let mut key = BytesMut::from("b:");
    key.reserve(8);
    key.put_u64_le(block_number);
    key.extend_from_slice(b":h");
    key.freeze()
}

pub fn build_receipt_key(tx_hash: &H256) -> Bytes {
    let mut key = BytesMut::from("r:");
    key.extend_from_slice(tx_hash.as_bytes());
    key.freeze()
}

pub fn build_block_receipt_hashes_key(block_number: u64) -> Bytes {
    let mut key = BytesMut::from("b:");
    key.reserve(8);
    key.put_u64_le(block_number);
    key.extend_from_slice(b":r");
    key.freeze()
}

pub fn build_block_spent_out_points_key(block_number: u64) -> Bytes {
    let mut key = BytesMut::from("b:");
    key.reserve(8);
    key.put_u64_le(block_number);
    key.extend_from_slice(b":s");
    key.freeze()
}

pub fn build_block_added_out_points_key(block_number: u64) -> Bytes {
    let mut key = BytesMut::from("b:");
    key.reserve(8);
    key.put_u64_le(block_number);
    key.extend_from_slice(b":a");
    key.freeze()
}

pub fn load_latest_out_points(
    db: &Arc<DB>,
    eth_address: &EthAddress,
    block_number: u64,
) -> Result<Vec<CellOutPoint>, Error> {
    let last_key = build_eth_key(&eth_address, Some(block_number));
    let prefix_key = build_eth_key(&eth_address, None);

    let mut iter = db.raw_iterator();
    iter.seek_for_prev(&last_key);

    if iter.valid() {
        if let Some(key) = iter.key() {
            if key.starts_with(&prefix_key) {
                if let Some(value) = iter.value() {
                    return Ok(deserialize(&value)?);
                }
            }
        }
    }
    Ok(vec![])
}

// TODO: some of the following functions are better implemented as serde
// trait implementations. But serde takes some trouble to get right, so we
// are sticking with simple solution now and make the change later.

#[derive(Debug, Clone)]
pub struct EthCell(pub CellOutput, pub CellOutPoint);

#[derive(Debug, Clone)]
pub struct EthAccount {
    pub main_cell: Option<EthCell>,
    pub fund_cells: Vec<EthCell>,
}

impl EthAccount {
    pub fn contract_account(&self) -> Result<bool, Error> {
        if let Some(main_cell) = &self.main_cell {
            if main_cell.0.data.len() > 0 {
                return Ok(CellType::try_from(main_cell.0.data.as_bytes()[0])?
                    == CellType::ContractMainCell);
            }
        }
        Ok(false)
    }

    pub fn contract_data(&self) -> Result<EthContractData, Error> {
        if let Some(main_cell) = &self.main_cell {
            Ok(deserialize(&main_cell.0.data.as_bytes()[1..])?)
        } else {
            Err(Error::MalformedData(
                "Contract must have main cell!".to_string(),
            ))
        }
    }

    pub fn next_nonce(&self) -> Result<U256, Error> {
        if let Some(main_cell) = &self.main_cell {
            let mut data = [0u8; 32];
            if main_cell.0.data.len() < 8 {
                return Err(Error::MalformedData("Invalid main cell".to_string()));
            }
            data[..8].copy_from_slice(&main_cell.0.data.as_bytes()[1..9]);
            let nonce = U256::from_le_bytes(&data);
            let next_nonce = nonce
                .checked_add(&U256::one())
                .ok_or(Error::MalformedData("Nonce addition overflow!".to_string()))?;
            Ok(next_nonce)
        } else {
            Ok(U256::zero())
        }
    }

    pub fn total_capacities(&self) -> Result<Capacity, Error> {
        let main_capacity = self
            .main_cell
            .clone()
            .map(|cell| cell.0.capacity)
            .unwrap_or(Capacity(0u64.as_capacity()));
        self.fund_cells
            .iter()
            .try_fold(main_capacity, |sum, cell| {
                sum.0.safe_add(cell.0.capacity.0).map(|c| Capacity(c))
            })
            .map_err(|_| Error::MalformedData("Capacity overflow".to_string()))
    }

    pub fn total_capacities_in_wei(&self) -> Result<U256, Error> {
        let capacities: U256 = self.total_capacities()?.0.as_u64().into();
        let wei = capacities
            .checked_mul(&SHANNON_TO_WEI)
            .ok_or(Error::MalformedData(
                "Shannon cannot be expressed in wei!".to_string(),
            ))?;
        Ok(wei)
    }
}

pub enum BlockNumber {
    Latest,
    Number(u64),
}

impl BlockNumber {
    pub fn parse_with_default(s: &Option<String>) -> Result<BlockNumber, Error> {
        match s {
            Some(s) => BlockNumber::parse(&s),
            None => Ok(BlockNumber::Latest),
        }
    }

    pub fn parse(s: &str) -> Result<BlockNumber, Error> {
        match s {
            "latest" => Ok(BlockNumber::Latest),
            n if n.starts_with("0x") && (!n.starts_with("0x0")) => Ok(BlockNumber::Number(
                u64::from_str_radix(&s[2..], 16)
                    .map_err(|e| Error::MalformedData(e.to_string()))?,
            )),
            _ => Err(Error::MalformedData(
                format!("Invalid block number: {}", s).to_string(),
            )),
        }
    }
}

#[derive(PartialEq, Eq, Hash, Clone, Debug)]
pub struct EthAddress(pub Bytes);

impl Default for EthAddress {
    fn default() -> Self {
        EthAddress(Bytes::from(&[0u8; 20][..]))
    }
}

impl EthAddress {
    pub fn parse(s: &str) -> Result<EthAddress, Error> {
        if s.len() != 42 || (!s.starts_with("0x")) {
            return Err(Error::MalformedData(
                format!("Invalid ETH address: {}", s).to_string(),
            ));
        }
        let mut b = [0u8; 20];
        hex_decode(&s.as_bytes()[2..], &mut b[..])
            .map_err(|e| Error::MalformedData(e.to_string()))?;
        Ok(EthAddress(Bytes::from(&b[..])))
    }
}

impl<'a> From<&'a [u8]> for EthAddress {
    fn from(src: &'a [u8]) -> EthAddress {
        assert!(src.len() == 20);
        let b: Bytes = src.into();
        EthAddress(b)
    }
}

impl<'a> From<&'a ParityAddress> for EthAddress {
    fn from(address: &'a ParityAddress) -> EthAddress {
        address.as_ref().into()
    }
}

impl<'a> From<&'a EthAddress> for ParityAddress {
    fn from(address: &'a EthAddress) -> ParityAddress {
        ParityAddress::from_slice(address.as_ref())
    }
}

impl AsRef<[u8]> for EthAddress {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

#[inline]
pub fn serialize_u64(n: u64) -> String {
    format!("0x{:x}", n).to_string()
}

#[derive(Debug)]
pub struct EthTransaction {
    pub nonce: u64,
    pub gas_price: U256,
    pub gas_limit: U256,
    pub to: Option<EthAddress>,
    pub value: U256,
    pub data: Option<Bytes>,
    pub v: u64,
    pub r: U256,
    pub s: U256,

    pub from: EthAddress,
    pub raw: Bytes,
}

impl EthTransaction {
    pub fn value_in_capacity(&self) -> Result<Capacity, Error> {
        wei_to_capacity(&self.value)
    }

    pub fn fees(&self) -> Result<U256, Error> {
        self.gas_price
            .checked_mul(&self.gas_limit)
            .ok_or(Error::MalformedData(
                "Wei multiplication overflow!".to_string(),
            ))
    }

    pub fn fees_in_capacity(&self) -> Result<Capacity, Error> {
        wei_to_capacity(&self.fees()?)
    }

    pub fn hash(&self) -> H256 {
        keccak256(&self.raw).into()
    }

    pub fn from_raw(raw: Bytes) -> Result<EthTransaction, Error> {
        let bytes: Vec<Vec<u8>> = Rlp::new(&raw).as_list()?;
        if bytes.len() != 9 {
            return Err(Error::MalformedData(
                format!("Invalid data length: {}", bytes.len()).to_string(),
            ));
        }
        if bytes[7].len() != 32 {
            return Err(Error::MalformedData(
                format!("Invalid r length: {}", bytes[7].len()).to_string(),
            ));
        }
        if bytes[8].len() != 32 {
            return Err(Error::MalformedData(
                format!("Invalid s length: {}", bytes[8].len()).to_string(),
            ));
        }
        let tx = EthTransaction {
            nonce: bytes_to_u64(&bytes[0])?,
            gas_price: bytes_to_u256(&bytes[1])?,
            gas_limit: bytes_to_u256(&bytes[2])?,
            to: if bytes[3].len() > 0 {
                Some(EthAddress(Bytes::from(&bytes[3][..])))
            } else {
                None
            },
            value: bytes_to_u256(&bytes[4])?,
            data: if bytes[5].len() > 0 {
                Some(Bytes::from(&bytes[5][..]))
            } else {
                None
            },
            v: bytes_to_u64(&bytes[6])?,
            r: bytes_to_u256(&bytes[7])?,
            s: bytes_to_u256(&bytes[8])?,
            from: extract_from_address(&bytes)?,
            raw,
        };
        Ok(tx)
    }
}

fn wei_to_capacity(w: &U256) -> Result<Capacity, Error> {
    let bytes = w.overflowing_div(&SHANNON_TO_WEI).0.to_le_bytes();
    for b in &bytes[8..] {
        if *b != 0 {
            return Err(Error::MalformedData(
                "Exceeds maximum range of capacity!".to_string(),
            ));
        }
    }
    let mut capacity_bytes = [0u8; 8];
    capacity_bytes.copy_from_slice(&bytes[0..8]);
    Ok(Capacity(u64::from_le_bytes(capacity_bytes).as_capacity()))
}

fn extract_from_address(bytes: &[Vec<u8>]) -> Result<EthAddress, Error> {
    let recovery = calculate_sig_recovery(bytes_to_u64(&bytes[6])?)?;
    let recovery_id = RecoveryId::from_i32(recovery as i32)?;
    let mut unsigned_tx = bytes.to_vec();
    // TODO: fix this later
    assert!(CHAIN_ID <= 0xFF);
    unsigned_tx[6] = vec![CHAIN_ID as u8];
    unsigned_tx[7] = vec![];
    unsigned_tx[8] = vec![];
    let serialized_unsigned_tx = encode_list::<Vec<u8>, _>(&unsigned_tx);
    let serialized_unsigned_tx_hash = keccak256(&serialized_unsigned_tx).to_vec();
    let message = Message::from_slice(&serialized_unsigned_tx_hash[..])?;
    let mut signature_data = [0u8; 64];
    signature_data[..32].copy_from_slice(&bytes[7]);
    signature_data[32..].copy_from_slice(&bytes[8]);
    let signature = RecoverableSignature::from_compact(&signature_data, recovery_id)?;
    let public_key = SECP256K1.recover(&message, &signature)?;
    let serialized_public_key = public_key.serialize_uncompressed();
    let public_key_hash = keccak256(&serialized_public_key[1..]);
    Ok(EthAddress(Bytes::from(&public_key_hash[12..])))
}

fn bytes_to_u64(bytes: &[u8]) -> Result<u64, Error> {
    if bytes.len() > 8 {
        return Err(Error::MalformedData(
            format!("Invalid field length: {}", bytes.len()).to_string(),
        ));
    }
    let mut v: u64 = 0;
    for b in bytes {
        v = (v << 8) | u64::from(*b);
    }
    Ok(v)
}

fn bytes_to_u256(bytes: &[u8]) -> Result<U256, Error> {
    if bytes.len() > 32 {
        return Err(Error::MalformedData(
            format!("Invalid field length: {}", bytes.len()).to_string(),
        ));
    }
    let mut data = [0u8; 32];
    // Big endian
    data[(32 - bytes.len())..].copy_from_slice(bytes);
    Ok(U256::from_be_bytes(&data))
}

fn calculate_sig_recovery(v: u64) -> Result<u8, Error> {
    let v = v - (2 * CHAIN_ID + 35);
    if v != 0 && v != 1 {
        return Err(Error::MalformedData(
            format!("Invalid recovery: {}", v).to_string(),
        ));
    }
    Ok(v as u8)
}

#[derive(Serialize, Deserialize)]
pub struct EthBasicReceipt {
    pub transaction_index: u64,
    pub cumulative_gas: U256,

    pub block_number: u64,
    pub ckb_transaction_hash: H256,
    pub witness_index: u64,
}

#[derive(Serialize, Deserialize)]
pub struct TransactionReceipt {
    #[serde(rename = "transactionHash")]
    pub transaction_hash: H256,
    #[serde(rename = "transactionIndex")]
    pub transaction_index: U256,
    #[serde(rename = "blockHash")]
    pub block_hash: H256,
    #[serde(rename = "blockNumber")]
    pub block_number: U256,
    pub from: JsonBytes,
    pub to: Option<JsonBytes>,
    #[serde(rename = "cumulativeGasUsed")]
    pub cumulative_gas_used: U256,
    #[serde(rename = "gasUsed")]
    pub gas_used: U256,
    #[serde(rename = "contractAddress")]
    pub contract_address: Option<JsonBytes>,
    pub logs: Vec<JsonBytes>,
    #[serde(rename = "logsBloom")]
    pub logs_bloom: H256,
    pub status: U256,
}

impl TransactionReceipt {
    pub fn from(
        basic_receipt: &EthBasicReceipt,
        transaction: &TransactionView,
        block_hash: &H256,
    ) -> Result<Self, Error> {
        let witness: Witness = transaction.inner.witnesses[basic_receipt.witness_index as usize]
            .clone()
            .into();
        let eth_transaction = EthTransaction::from_raw(witness[0].clone())?;
        let contract_address = transaction
            .inner
            .outputs
            .iter()
            .find(|output| output.lock.code_hash == CODE_HASH_CONTRACT_LOCK.into())
            .map(|output| output.lock.args[0].clone());
        Ok(TransactionReceipt {
            transaction_hash: transaction.hash.clone(),
            transaction_index: basic_receipt.transaction_index.into(),
            block_hash: block_hash.clone(),
            block_number: basic_receipt.block_number.into(),
            from: JsonBytes::from_bytes(eth_transaction.from.0.clone()),
            to: eth_transaction
                .to
                .clone()
                .map(|address| JsonBytes::from_bytes(address.0)),
            cumulative_gas_used: basic_receipt.cumulative_gas.clone(),
            gas_used: eth_transaction.fees()?,
            contract_address,
            logs: vec![],
            logs_bloom: H256::zero(),
            status: U256::one(),
        })
    }
}

#[derive(Serialize, Deserialize)]
pub struct EthContractData {
    pub code: Bytes,
    pub storage: HashMap<U256, U256>,
}
