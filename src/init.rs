use bincode::serialize;
use bytes::{BufMut, Bytes, BytesMut};
use ckb_core::{
    script::{Script as CoreScript, ScriptHashType as CoreScriptHashType},
    transaction::{
        CellInput as CoreCellInput, CellOutPoint as CoreCellOutPoint, CellOutput as CoreCellOutput,
        OutPoint as CoreOutPoint, TransactionBuilder as CoreTransactionBuilder,
    },
};
use ckb_hash::blake2b_256;
use ckb_jsonrpc_types::{BlockNumber, CellOutPoint, Unsigned};
use ckb_occupied_capacity::AsCapacity;
use ckb_sdk::HttpRpcClient;
use faster_hex::hex_decode;
use numext_fixed_hash::H256;
use polyjuice::{
    storage::{CONTRACT_LOCK_CODE_DEP_KEY, LOCK_CODE_DEP_KEY},
    BUNDLED_CELL, SECP256K1,
};
use rocksdb::DB;
use secp256k1::{Message, PublicKey, SecretKey};
use std::env;
use std::process::exit;
use std::sync::Arc;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() != 1 || args[0].len() != 64 {
        exit(1);
    }
    let mut secret_key_bytes = [0u8; 32];
    hex_decode(args[0].as_bytes(), &mut secret_key_bytes[..]).expect("hex decode");
    let secret_key = SecretKey::from_slice(&secret_key_bytes[..]).expect("secret key");
    let public_key = PublicKey::from_secret_key(&SECP256K1, &secret_key);
    let public_key_bytes = public_key.serialize();
    let public_key_hash = blake2b_256(&public_key_bytes[..]);
    let blake160_hash = Bytes::from(&public_key_hash[..20]);

    let ckb_uri = "http://127.0.0.1:8114";
    let mut client = HttpRpcClient::from_uri(ckb_uri);

    let genesis_block = client
        .get_block_by_number(BlockNumber(0))
        .call()
        .expect("fetching genesis block")
        .0
        .unwrap();
    let system_cell_transaction = genesis_block.transactions[0].clone();
    let secp_out_point = CoreOutPoint {
        cell: Some(CoreCellOutPoint {
            tx_hash: system_cell_transaction.hash.clone(),
            index: 1,
        }),
        block_hash: None,
    };
    let code_hash: H256 =
        blake2b_256(system_cell_transaction.inner.outputs[1].data.as_bytes()).into();
    let lock = CoreScript {
        args: vec![blake160_hash],
        code_hash,
        hash_type: CoreScriptHashType::Data,
    };
    let lock_hash = lock.hash();

    let mut lock_cell = CoreCellOutput {
        capacity: 0u64.as_capacity(),
        data: Bytes::from(BUNDLED_CELL.get("cells/lock").unwrap().as_ref()),
        lock: lock.clone(),
        type_: None,
    };
    lock_cell.capacity = lock_cell.occupied_capacity().expect("occupied capacity");
    let mut contract_lock_cell = CoreCellOutput {
        capacity: 0u64.as_capacity(),
        data: Bytes::from(BUNDLED_CELL.get("cells/contract_lock").unwrap().as_ref()),
        lock: lock.clone(),
        type_: None,
    };
    contract_lock_cell.capacity = contract_lock_cell
        .occupied_capacity()
        .expect("occupied capacity");
    let total_capacity = lock_cell
        .capacity
        .safe_add(contract_lock_cell.capacity)
        .expect("add capacity");

    let mut current_capacity = 0u64.as_capacity();
    let tip_number = client.get_tip_block_number().call().unwrap().0;
    let mut start = 0;
    let mut inputs = vec![];
    while start < tip_number {
        let cells = client
            .get_cells_by_lock_hash(
                lock_hash.clone(),
                BlockNumber(start),
                BlockNumber(start + 100 - 1),
            )
            .call()
            .unwrap()
            .0;
        for cell in &cells {
            current_capacity = current_capacity.safe_add(cell.capacity.0).unwrap();
            inputs.push(CoreCellInput {
                previous_output: cell.out_point.clone().into(),
                since: 0,
            });
        }
        if current_capacity >= total_capacity {
            break;
        }
        start += 100;
    }
    let mut transaction_builder = CoreTransactionBuilder::default()
        .dep(secp_out_point)
        .inputs(inputs)
        .output(lock_cell)
        .output(contract_lock_cell);
    if current_capacity > total_capacity {
        transaction_builder = transaction_builder.output(CoreCellOutput {
            capacity: current_capacity.safe_sub(total_capacity).unwrap(),
            data: Bytes::default(),
            lock: lock.clone(),
            type_: None,
        });
    }
    let unsigned_transaction = transaction_builder.build();
    let message = Message::from_slice(&blake2b_256(unsigned_transaction.hash())[..]).unwrap();
    let signature = SECP256K1.sign_recoverable(&message, &secret_key);
    let (recid, compact) = signature.serialize_compact();
    let mut witness_bytes = BytesMut::new();
    witness_bytes.extend_from_slice(&compact[..]);
    witness_bytes.reserve(1);
    witness_bytes.put(recid.to_i32() as u8);
    let witness = witness_bytes.freeze();
    let mut signed_builder = CoreTransactionBuilder::from_transaction(unsigned_transaction.clone());
    for _ in 0..unsigned_transaction.inputs().len() {
        signed_builder = signed_builder.witness(vec![witness.clone()]);
    }
    let transaction = signed_builder.build();
    let tx_hash = client
        .send_transaction((&transaction).into())
        .call()
        .unwrap();
    println!("TX hash: {:x}", tx_hash);

    // Write to DB
    let db = Arc::new(DB::open_default("./data").expect("rocksdb"));
    db.put(
        &LOCK_CODE_DEP_KEY,
        &serialize(&CellOutPoint {
            tx_hash: transaction.hash().clone(),
            index: Unsigned(0),
        })
        .expect("serialize"),
    )
    .expect("rocksdb write");
    db.put(
        &CONTRACT_LOCK_CODE_DEP_KEY,
        &serialize(&CellOutPoint {
            tx_hash: transaction.hash().clone(),
            index: Unsigned(1),
        })
        .expect("serialize"),
    )
    .expect("rocksdb write");

    println!("All done!");
}
