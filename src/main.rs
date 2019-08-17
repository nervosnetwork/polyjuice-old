#[macro_use]
extern crate log;

use jsonrpc_core::IoHandler;
use jsonrpc_http_server::ServerBuilder;
use jsonrpc_server_utils::cors::AccessControlAllowOrigin;
use jsonrpc_server_utils::hosts::DomainsValidation;
use polyjuice::{
    modules::{EthRpc, EthRpcImpl, Web3Rpc, Web3RpcImpl},
    storage::{Indexer, Loader},
};
use rocksdb::DB;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

fn main() {
    env_logger::init();

    info!("starting...");

    let db = Arc::new(DB::open_default("./data").expect("rocksdb"));
    let ckb_uri = "http://127.0.0.1:8114";
    let loader = Arc::new(Loader::new(Arc::clone(&db), ckb_uri).expect("loader failure"));

    let mut indexer = Indexer::from(Arc::clone(&db), ckb_uri);
    let _ = thread::spawn(move || indexer.index().expect("indexer faliure"));

    // RPC
    let mut io_handler = IoHandler::new();
    io_handler.extend_with(Web3RpcImpl {}.to_delegate());
    io_handler.extend_with(
        EthRpcImpl {
            loader: Arc::clone(&loader),
        }
        .to_delegate(),
    );

    let rpc_server = ServerBuilder::new(io_handler)
        .cors(DomainsValidation::AllowOnly(vec![
            AccessControlAllowOrigin::Null,
            AccessControlAllowOrigin::Any,
        ]))
        // TODO parameterize following if needed
        .threads(4)
        .max_request_body_size(10485760)
        .start_http(&"127.0.0.1:8214".parse().expect("parse listen address"))
        .expect("jsonrpc initialize");

    // Wait for exit
    let exit = Arc::new((Mutex::new(()), Condvar::new()));
    let e = Arc::clone(&exit);
    ctrlc::set_handler(move || {
        e.1.notify_all();
    })
    .expect("error setting Ctrl-C handler");
    let _guard = exit
        .1
        .wait(exit.0.lock().expect("locking"))
        .expect("waiting");
    rpc_server.close();
    info!("exiting...");
}
