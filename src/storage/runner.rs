use super::{
    CellType, Error, EthAccount, EthAddress, EthCell, EthContractData, EthTransaction, Loader,
};
use crate::{CODE_HASH_CONTRACT_LOCK, CODE_HASH_LOCK};
use bincode::serialize;
use bytes::{Bytes, BytesMut};
use ckb_core::transaction::CellOutput as CoreCellOutput;
use ckb_jsonrpc_types::{
    Capacity, CellInput, CellOutput, JsonBytes, OutPoint, Script, Transaction, Unsigned, Version,
};
use ckb_occupied_capacity::AsCapacity;
use ethereum_types::{Address as ParityAddress, H256 as ParityH256, U256 as ParityU256};
use evm::Factory;
use numext_fixed_uint::U256;
use rlp::RlpStream;
use std::collections::HashMap;
use std::sync::Arc;
use tiny_keccak::keccak256;
use vm::{
    ActionParams, ActionValue, CallType, ContractCreateResult, CreateContractAddress, EnvInfo, Ext,
    GasLeft, MessageCallResult, ParamsType, Result as ParityVmResult, ReturnData, Schedule,
    TrapKind,
};

fn numext_u256_to_parity_h256(v: &U256) -> ParityH256 {
    ParityH256::from_slice(&v.to_be_bytes())
}

fn parity_h256_to_numext_u256(v: &ParityH256) -> U256 {
    U256::from_be_bytes(&v.to_fixed_bytes())
}

fn to_parity_u256(v: &U256) -> ParityU256 {
    ParityU256::from_little_endian(&v.to_le_bytes())
}

pub struct Runner<'a> {
    pub loader: &'a Loader,
    pub tx: &'a EthTransaction,
    pub block_number: u64,
}

impl<'a> Runner<'a> {
    pub fn run(&mut self) -> Result<Transaction, Error> {
        if self.tx.to.is_none() {
            self.create_contract()
        } else {
            let to_address = self.tx.to.clone().unwrap();
            let to = self
                .loader
                .load_account(&to_address, self.block_number, false)?
                .ok_or(Error::MalformedData(
                    "Contract does not exist yet!".to_string(),
                ))?;
            if to.contract_account()? {
                self.call_contract(&to)
            } else {
                self.send_to_normal_account()
            }
        }
    }

    fn send_to_normal_account(&self) -> Result<Transaction, Error> {
        // TODO: check gas limit
        let data = JsonBytes::default();
        let mut lock = Script::default();
        lock.code_hash = CODE_HASH_LOCK.into();
        lock.args
            .push(JsonBytes::from_bytes(self.tx.to.clone().unwrap().0));

        self.build_ckb_transaction(data, lock, Capacity(0u64.as_capacity()))
    }

    fn call_contract(&mut self, contract_account: &EthAccount) -> Result<Transaction, Error> {
        let contract_address = self.tx.to.clone().unwrap();
        let contract_data = contract_account.contract_data()?;
        let (_, _, contract_data) = self.call_evm(&contract_address, contract_data)?;

        let mut data = BytesMut::from(&[CellType::ContractMainCell as u8][..]);
        data.extend_from_slice(&serialize(&contract_data)?);
        let data = JsonBytes::from_bytes(data.freeze());
        let mut lock = Script::default();
        lock.code_hash = CODE_HASH_CONTRACT_LOCK.into();
        lock.args
            .push(JsonBytes::from_bytes(contract_address.0.clone()));

        let EthCell(main_cell_output, main_cell_out_point) = contract_account
            .main_cell
            .clone()
            .expect("contract account must have main cell");
        let mut ckb_transaction =
            self.build_ckb_transaction(data, lock, main_cell_output.capacity)?;
        ckb_transaction.inputs.push(CellInput {
            previous_output: OutPoint {
                cell: Some(main_cell_out_point),
                block_hash: None,
            },
            since: Unsigned(0),
        });
        ckb_transaction.witnesses.push((&vec![]).into());
        Ok(ckb_transaction)
    }

    fn create_contract(&mut self) -> Result<Transaction, Error> {
        if self.tx.data.is_none() {
            return Err(Error::MalformedData(
                "Contract creation transaction is missing data!".to_string(),
            ));
        }
        let code = self.tx.data.clone().unwrap();
        let mut stream = RlpStream::new_list(2);
        stream
            .append(&self.tx.from.as_ref().to_vec())
            .append(&self.tx.nonce);
        let rlp_data = stream.out();
        let contract_address = EthAddress(Bytes::from(&keccak256(&rlp_data)[12..]));
        let contract_data = EthContractData {
            code,
            storage: HashMap::default(),
        };

        // Run contract on CKB to initialize real code
        let (_gas_left, return_data, contract_data) =
            self.call_evm(&contract_address, contract_data)?;
        if return_data.is_none() {
            return Err(Error::MalformedData(
                "Initializer is missing return data".to_string(),
            ));
        }
        // TODO: finalize code gas
        let initialized_code = Bytes::from(&*return_data.unwrap());
        let initialized_storage = contract_data.storage;

        let mut data = BytesMut::from(&[CellType::ContractMainCell as u8][..]);
        data.extend_from_slice(&serialize(&EthContractData {
            code: initialized_code,
            storage: initialized_storage,
        })?);
        let data = JsonBytes::from_bytes(data.freeze());
        let mut lock = Script::default();
        lock.code_hash = CODE_HASH_CONTRACT_LOCK.into();
        lock.args
            .push(JsonBytes::from_bytes(contract_address.0.clone()));
        self.build_ckb_transaction(data, lock, Capacity(0u64.as_capacity()))
    }

    fn call_evm(
        &mut self,
        contract_address: &EthAddress,
        contract_data: EthContractData,
    ) -> Result<(ParityU256, Option<ReturnData>, EthContractData), Error> {
        let params = ActionParams {
            code_address: contract_address.into(),
            code_hash: Some(keccak256(&contract_data.code).into()),
            address: contract_address.into(),
            sender: (&self.tx.from).into(),
            origin: (&self.tx.from).into(),
            gas: to_parity_u256(&self.tx.fees()?),
            gas_price: to_parity_u256(&self.tx.gas_price),
            value: ActionValue::Transfer(to_parity_u256(&self.tx.value)),
            code: Some(Arc::new(contract_data.code.to_vec())),
            code_version: ParityU256::zero(),
            data: self.tx.data.clone().map(|bytes| bytes.to_vec()),
            call_type: CallType::Call,
            params_type: ParamsType::Separate,
        };
        let schedule = Schedule::new_constantinople();
        let exec = Factory::default().create(params, &schedule, 0);
        let mut contract_runner = ContractRunner::new(self, contract_data);
        let result = exec
            .exec(&mut contract_runner)
            .map_err(|_| Error::EVM("Trap is not yet supported".to_string()))??;
        let (gas_left, return_data) = match result {
            GasLeft::NeedsReturn {
                data,
                apply_state,
                gas_left,
            } => {
                if apply_state {
                    (gas_left, Some(data))
                } else {
                    return Err(Error::EVM("Reverted!".to_string()));
                }
            }
            GasLeft::Known(gas_left) => (gas_left, None),
        };
        Ok((gas_left, return_data, contract_runner.data))
    }

    fn build_ckb_transaction(
        &self,
        data: JsonBytes,
        lock: Script,
        spare_capacity: Capacity,
    ) -> Result<Transaction, Error> {
        let account = self
            .loader
            .load_account(&self.tx.from, self.block_number, false)?
            .ok_or(Error::MalformedData(
                "Account does not exist yet!".to_string(),
            ))?;
        let value_capacity = self.tx.value_in_capacity()?;
        let target_cell = CoreCellOutput {
            capacity: value_capacity
                .0
                .safe_add(spare_capacity.0)
                .map_err(|_| Error::MalformedData("Capacity addition overflow".to_string()))?,
            data: data.into_bytes(),
            lock: lock.into(),
            type_: None,
        };
        if target_cell
            .is_lack_of_capacity()
            .map_err(|_| Error::MalformedData("Capacity error".to_string()))?
        {
            return Err(Error::MalformedData(
                format!(
                    "Capacity is not enough!, required: {:?} actual: {:?}",
                    target_cell.occupied_capacity(),
                    target_cell.capacity,
                )
                .to_string(),
            ));
        }
        let change_capacity = Capacity(
            account
                .total_capacities()?
                .0
                .safe_sub(self.tx.fees_in_capacity()?.0)
                .and_then(|c| c.safe_sub(value_capacity.0))
                .map_err(|_| Error::MalformedData("Account capacity is not enough!".to_string()))?,
        );
        let original_lock = match &account.main_cell {
            Some(cell) => cell.0.lock.clone(),
            None => account.fund_cells[0].0.lock.clone(),
        };
        let mut change_data = BytesMut::from(&[CellType::NormalMainCell as u8][..]);
        change_data.extend_from_slice(&self.tx.nonce.to_le_bytes());
        let change_data = JsonBytes::from_bytes(change_data.freeze());
        let mut ckb_transaction = Transaction {
            version: Version(0),
            deps: vec![
                OutPoint {
                    cell: Some(self.loader.load_lock_out_point()?),
                    block_hash: None,
                },
                OutPoint {
                    cell: Some(self.loader.load_contract_lock_out_point()?),
                    block_hash: None,
                },
            ],
            inputs: account
                .fund_cells
                .iter()
                .map(|c| CellInput {
                    previous_output: OutPoint {
                        cell: Some(c.1.clone()),
                        block_hash: None,
                    },
                    since: Unsigned(0),
                })
                .collect(),
            outputs: vec![
                CellOutput {
                    capacity: change_capacity,
                    data: change_data,
                    lock: original_lock,
                    type_: None,
                },
                target_cell.into(),
            ],
            witnesses: account
                .fund_cells
                .iter()
                .map(|_| (&vec![]).into())
                .collect(),
        };
        if let Some(main_cell) = account.main_cell {
            ckb_transaction.inputs.insert(
                0,
                CellInput {
                    previous_output: OutPoint {
                        cell: Some(main_cell.1.clone()),
                        block_hash: None,
                    },
                    since: Unsigned(0),
                },
            );
            ckb_transaction.witnesses.insert(0, (&vec![]).into());
        }
        ckb_transaction.witnesses[0] = (&vec![self.tx.raw.clone()]).into();
        Ok(ckb_transaction)
    }
}

struct ContractRunner<'a, 'b> {
    pub runner: &'a mut Runner<'b>,
    pub data: EthContractData,

    schedule: Schedule,
}

impl<'a, 'b> ContractRunner<'a, 'b> {
    fn new(runner: &'a mut Runner<'b>, data: EthContractData) -> Self {
        Self {
            runner,
            data,
            schedule: Schedule::new_constantinople(),
        }
    }
}

impl<'a, 'b> Ext for ContractRunner<'a, 'b> {
    fn initial_storage_at(&self, _key: &ParityH256) -> ParityVmResult<ParityH256> {
        unimplemented!()
    }

    fn storage_at(&self, key: &ParityH256) -> ParityVmResult<ParityH256> {
        let value = self
            .data
            .storage
            .get(&parity_h256_to_numext_u256(key))
            .cloned()
            .unwrap_or(U256::zero());
        Ok(numext_u256_to_parity_h256(&value))
    }

    fn set_storage(&mut self, key: ParityH256, value: ParityH256) -> ParityVmResult<()> {
        self.data.storage.insert(
            parity_h256_to_numext_u256(&key),
            parity_h256_to_numext_u256(&value),
        );
        Ok(())
    }

    fn exists(&self, _address: &ParityAddress) -> ParityVmResult<bool> {
        unimplemented!()
    }

    fn exists_and_not_null(&self, _address: &ParityAddress) -> ParityVmResult<bool> {
        unimplemented!()
    }

    fn origin_balance(&self) -> ParityVmResult<ParityU256> {
        unimplemented!()
    }

    fn balance(&self, _address: &ParityAddress) -> ParityVmResult<ParityU256> {
        unimplemented!()
    }

    fn blockhash(&mut self, _number: &ParityU256) -> ParityH256 {
        unimplemented!()
    }

    fn create(
        &mut self,
        _gas: &ParityU256,
        _value: &ParityU256,
        _code: &[u8],
        _parent_version: &ParityU256,
        _address: CreateContractAddress,
        _trap: bool,
    ) -> ::std::result::Result<ContractCreateResult, TrapKind> {
        unimplemented!()
    }

    fn call(
        &mut self,
        _gas: &ParityU256,
        _sender_address: &ParityAddress,
        _receive_address: &ParityAddress,
        _value: Option<ParityU256>,
        _data: &[u8],
        _code_address: &ParityAddress,
        _call_type: CallType,
        _trap: bool,
    ) -> ::std::result::Result<MessageCallResult, TrapKind> {
        unimplemented!()
    }

    fn extcode(&self, _address: &ParityAddress) -> ParityVmResult<Option<Arc<Vec<u8>>>> {
        unimplemented!()
    }

    fn extcodehash(&self, _address: &ParityAddress) -> ParityVmResult<Option<ParityH256>> {
        unimplemented!()
    }

    fn extcodesize(&self, _address: &ParityAddress) -> ParityVmResult<Option<usize>> {
        unimplemented!()
    }

    fn log(&mut self, _topics: Vec<ParityH256>, _data: &[u8]) -> ParityVmResult<()> {
        unimplemented!()
    }

    fn ret(
        self,
        _gas: &ParityU256,
        _data: &ReturnData,
        _apply_state: bool,
    ) -> ParityVmResult<ParityU256> {
        unimplemented!()
    }

    fn suicide(&mut self, _refund_address: &ParityAddress) -> ParityVmResult<()> {
        unimplemented!()
    }

    fn schedule(&self) -> &Schedule {
        &self.schedule
    }

    fn env_info(&self) -> &EnvInfo {
        unimplemented!()
    }

    fn depth(&self) -> usize {
        unimplemented!()
    }

    fn add_sstore_refund(&mut self, _value: usize) {
        unimplemented!()
    }

    fn sub_sstore_refund(&mut self, _value: usize) {
        unimplemented!()
    }

    fn is_static(&self) -> bool {
        unimplemented!()
    }
}
