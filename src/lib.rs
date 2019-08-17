#![allow(clippy::unreadable_literal)]

include!(concat!(env!("OUT_DIR"), "/bundled.rs"));
include!(concat!(env!("OUT_DIR"), "/code_hashes.rs"));

pub mod modules;
pub mod storage;

#[macro_use]
extern crate log;

use bincode::Error as BincodeError;
use jsonrpc_client_core::Error as ClientRpcError;
use jsonrpc_core::{Error as ServerRpcError, ErrorCode as ServerRpcErrorCode};
use lazy_static::lazy_static;
use rlp::DecoderError;
use rocksdb::Error as DBError;
use secp256k1::Error as SecpError;
use std::fmt;
use vm::Error as EvmError;

lazy_static! {
    pub static ref SECP256K1: secp256k1::Secp256k1<secp256k1::All> = secp256k1::Secp256k1::new();
}

#[derive(Debug)]
pub enum Error {
    DB(String),
    Rpc(String),
    Data(String),
    Rlp(String),
    Secp(String),
    MalformedData(String),
    InvalidOutPoint,
    EVM(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl From<BincodeError> for Error {
    fn from(e: BincodeError) -> Error {
        Error::Data(e.to_string())
    }
}

impl From<DBError> for Error {
    fn from(e: DBError) -> Error {
        Error::DB(e.into_string())
    }
}

impl From<ClientRpcError> for Error {
    fn from(e: ClientRpcError) -> Error {
        Error::Rpc(e.to_string())
    }
}

impl From<DecoderError> for Error {
    fn from(e: DecoderError) -> Error {
        Error::Rlp(e.to_string())
    }
}

impl From<SecpError> for Error {
    fn from(e: SecpError) -> Error {
        Error::Secp(e.to_string())
    }
}

impl From<EvmError> for Error {
    fn from(e: EvmError) -> Error {
        Error::EVM(format!("{:?}", e).to_string())
    }
}

impl From<Error> for ServerRpcError {
    fn from(e: Error) -> ServerRpcError {
        ServerRpcError {
            code: ServerRpcErrorCode::InvalidRequest,
            message: e.to_string(),
            data: None,
        }
    }
}
