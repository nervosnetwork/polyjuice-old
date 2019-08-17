use ckb_jsonrpc_types::JsonBytes;
use jsonrpc_core::Result;
use jsonrpc_derive::rpc;
use tiny_keccak::keccak256;

#[rpc]
pub trait Web3Rpc {
    #[rpc(name = "web3_clientVersion")]
    fn client_version(&self) -> Result<String>;

    #[rpc(name = "web3_sha3")]
    fn sha3(&self, data: JsonBytes) -> Result<JsonBytes>;
}

pub struct Web3RpcImpl {}

impl Web3Rpc for Web3RpcImpl {
    fn client_version(&self) -> Result<String> {
        Ok(format!("Nervos Polyjuice/v{}", env!("CARGO_PKG_VERSION")))
    }

    fn sha3(&self, data: JsonBytes) -> Result<JsonBytes> {
        let result = keccak256(data.as_bytes()).to_vec();
        Ok(JsonBytes::from_vec(result))
    }
}
