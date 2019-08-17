# polyjuice

Polyjuice is a layer 2 solution that provides a Web3 compatible interface on top of [Nervos CKB](https://github.com/nervosnetwork/ckb). The design goal here is 95%+ compatible with existing Ethereum solution, so that your Solidity smart contracts and tools for Ethereum could work directly on top of Nervos CKB, while enjoying the following benefits:

* A state rent model that everyone needs to pay for their own storage
* An EVM that could be upgraded with new features without needing hardforks
* A modern blockchain that does not suffer from Ethereum's legacies.

Notice this project is still in very early phases, bugs can be expected, and many features are still missing. We will bring it to the day where you can use it in production but that's not today :P If you have any questions or run into any issues, feel free to let us know.

# A short tutorial

Here we provide a short tutorial performing the following operations on a polyjuice on CKB setup:

* Sending ETH to another account
* Simple contract creation
* Calling contract
* Reading storage data from a contract

Throughout the tutorial, we will work with the following 2 Ethereum accounts:

* Account A private key: 002c03c0cd7537d80e47456c33102593f4ae295650c21344b9e11ce9071b988f
* Account A address: 0x997f0b88b4e1203661e176029fe32cfdf7c388be
* Account B private key: 9bd3f57b9805cc401e53096f955b9e4ee6462822451c2eaefd5fd137eb2365cc
* Account B address: 0xcBb93f892F5d743Eba51101FC63D1b2023d33957

## Setting up CKB

As of now, polyjuice works with CKB v0.17. So feel free to either grab the [binary release](https://github.com/nervosnetwork/ckb/releases/tag/v0.17.0) or build from source. For convenience, we will launch a dev chain locally and work from there.

To initialize the dev chain, you can just use the init command:

```bash
$ ckb init -C devnet
```

The only thing we need to change, is the `block_assembler` section in `ckb.toml`:

```toml
[block_assembler]
code_hash = "0x3db879409367d993622f29c4c983a9fbf3ec6a73124e906779720d698c1f2fe6"
args = ["0x997f0b88b4e1203661e176029fe32cfdf7c388be"]
```

The code hash set here comes from polyjuice's account cell lock. And you might already notice that `args` filled here, is exactly the Ethereum address of account A. With this setup, the cell mined by CKB miner will automatically becomes Ethereum balance in account A.

Now we can launch CKB and the miner:

```bash
$ ckb run -C devnet --ba-advanced
$ # in a different terminal
$ ckb miner -C devnet
```

The only thing worth mentioning here, is that since we changed to a different code hash, we need to use `--ba-advanced` flag to tell CKB that we know what we are doing.

## Setting up polyjuice

Now we are ready to setup polyjuice:

```bash
$ git clone https://github.com/nervosnetwork/polyjuice
$ cd polyjuice
$ git submodule update --init --recursive --progress
$ make all-via-docker
$ cargo build --release
```

Before starting polyjuice, we need to first ensure 2 lock scripts used by polyjuice cells are uploaded to CKB. To upload the scripts, you will need a wallet with quite some amount of capacities. If you've been following this tutorial, a good choice is the [already issued tokens](https://github.com/nervosnetwork/ckb/blob/rc/v0.17/resource/specs/dev.toml#L40-L52) in genesis cell for development purposes:

```bash
$ target/release/init d00c06bfd800d27397002dca6fb0993d5ba6399b4238b2f29ee9deb97593d2bc
TX hash: b73b96c41fabd3769f920b4aa81a6b68cbc1b5d4499090bb4aeb77c2095b2cd4
All done!
```

When the transaction landed on CKB, you should be ready to start polyjuice:

```bash
$ target/release/polyjuice
```

Or if you are interested in more logs:

```bash
$ RUST_LOG="debug" target/release/polyjuice
```

## Interacting using Web3.js

We will be using [web3.js](https://github.com/ethereum/web3.js/) to interact with polyjuice as an Ethereum backend. Make sure you have a node.js installation and several packages installed:

```bash
$ npm i web3 ethereumjs-tx
```

### EOA Account

First, let's create a script to read Ethereum account balance:

```bash
$ cat get_balance.js
const {argv} = require("process");
const Web3 = require("web3");
const rpcURL = "http://127.0.0.1:8214";
const web3 = new Web3(rpcURL);

const account = argv[2];

web3.eth.getBalance(account, (err, wei) => {
  if (err) {
    console.log("Error: ", err);
  } else {
    console.log("Balance: ", wei);
  }
});
```

As you can see, it's a pretty standard JavaScript source code leveraging Web3 to read balance of an account, the only interesting part, is that we are using `http://127.0.0.1:8214`, which is the URL for our polyjuice instance.

Now we can try this on our accounts:

```bash
$ node get_balance.js 0x997f0b88b4e1203661e176029fe32cfdf7c388be
Balance:  66741753256290000000000
```

Notice you might see different values here since CKB miner is mining, subsequent runs of the same command would return more balance, proving the miner to be working correctly.

Trying the same thing on account B, however, would result in zero balance:

```bash
$ node get_balance.js 0xcBb93f892F5d743Eba51101FC63D1b2023d33957
Balance:  0
```

Let's create a second script to transfer Ether between accounts:

```bash
$ cat send_transaction.js
const {argv} = require("process");
const Web3 = require("web3");
const {Transaction} = require("ethereumjs-tx");
const rpcURL = "http://127.0.0.1:8214";
const web3 = new Web3(rpcURL);

// Ethereum address is 0x997f0b88b4e1203661e176029fe32cfdf7c388be
const privateKey = "002c03c0cd7537d80e47456c33102593f4ae295650c21344b9e11ce9071b988f";
const fromAddress = web3.eth.accounts.privateKeyToAccount(privateKey).address;

const toAddress = argv[2];
const value = argv[3];

web3.eth.getTransactionCount(fromAddress, (err, txCount) => {
  if (err) {
    console.log("Getting transaction count error: ", err);
    return;
  }

  const txObject = {
    nonce:    web3.utils.toHex(txCount),
    to:       toAddress,
    value:    web3.utils.toHex(web3.utils.toWei(value, 'ether')),
    gasLimit: web3.utils.toHex(21000),
    gasPrice: web3.utils.toHex(web3.utils.toWei('10', 'gwei'))
  };
  const tx = new Transaction(txObject);
  tx.sign(Buffer.from(privateKey, "hex"));
  const serializedTx = tx.serialize();
  const raw = '0x' + serializedTx.toString('hex');

  web3.eth.sendSignedTransaction(raw, (err, txHash) => {
    if (err) {
      console.log("Sending tx error: ", err);
    } else {
      console.log('txHash:', txHash);
    }
  });
});
```

Given a target account address and a balance in ether, this script would transfer ether from account A to the designated account. Now we can try it:

```bash
$ node send_transaction.js 0xcBb93f892F5d743Eba51101FC63D1b2023d33957 100
txHash: 0xd3b625ea841e6da79a85e66761f4a9600ce8e83ddf7a0a5130bb97f767cb7959
$ node get_balance.js 0xcBb93f892F5d743Eba51101FC63D1b2023d33957
Balance:  100000000000000000000
```

As you can see here we are transferring 100 ether from account A to B using Web3.js completely. Note that right now in polyjuice, we set 1 Ether to be 1 CKB, so sending a smaller amount of Ether(such as 30) might result in failure, since it won't be necessary to create a cell in CKB.

Since `send_transaction.js` handles Ethereum nonce correctly, running the same script again works:

```bash
$ node send_transaction.js 0xcBb93f892F5d743Eba51101FC63D1b2023d33957 100
txHash: 0xe2313e8dc7b4ae55702959feab41b5998efebc8ac7816e32e95c1cd3847acb48
$ node get_balance.js 0xcBb93f892F5d743Eba51101FC63D1b2023d33957
Balance:  200000000000000000000
```

### Dealing with Ethereum contracts

We can also try playing with a simple Ethereum contract:

```bash
$ cat SimpleStorage.sol
pragma solidity >=0.4.0 <0.7.0;

contract SimpleStorage {
    uint storedData;

    constructor() public payable {
      storedData = 123;
    }

    function set(uint x) public payable {
        storedData = x;
    }

    function get() public view returns (uint) {
        return storedData;
    }
}
```

First let's compile it with `solc`:

```bash
$ solc SimpleStorage.sol --abi --bin -o SS
```

Our first script here creates a new contract on Ethereum:

```bash
$ cat create_contract.js
const {readFileSync, writeFileSync} = require("fs");
const Web3 = require("web3");
const {Transaction} = require("ethereumjs-tx");
const rpcURL = "http://127.0.0.1:8214"
const web3 = new Web3(rpcURL);

const privateKey = "002c03c0cd7537d80e47456c33102593f4ae295650c21344b9e11ce9071b988f";
const fromAddress = web3.eth.accounts.privateKeyToAccount(privateKey).address;

const bytecode = readFileSync("SS/SimpleStorage.bin");

web3.eth.getTransactionCount(fromAddress, (err, txCount) => {
  if (err) {
    console.log("Getting transaction count error: ", err);
    return;
  }
  console.log("Nonce: ", txCount);

  const txObject = {
    nonce:    web3.utils.toHex(txCount),
    value:    web3.utils.toHex(web3.utils.toWei('2000', 'ether')),
    gasLimit: web3.utils.toHex(50000),
    gasPrice: web3.utils.toHex(web3.utils.toWei('1', 'gwei')),
    data: "0x" + bytecode
  };
  const tx = new Transaction(txObject);
  tx.sign(Buffer.from(privateKey, "hex"));
  const serializedTx = tx.serialize();
  const raw = '0x' + serializedTx.toString('hex');

  web3.eth.sendSignedTransaction(raw, (err, txHash) => {
    if (err) {
      console.log("Sending tx error: ", err);
    } else {
      console.log('txHash:', txHash);
    }
  }).on("receipt", console.log);
});
```

As we can see, it creates a contract using account A:

```bash
$ node create_contract.js
Nonce:  2
txHash: 0x3f7f6756b87f4715c17635e80415911c801729d692c22e2ec114f45fadeda36f
{ blockHash:
   '0x7c88416051551a9af13d220761d3a06c7b0463ce6e7258b66105e81373d46fe4',
  blockNumber: 191,
  contractAddress: '0x21E441A447D2AEEe8107b10d3f356eA9FD565e66',
  cumulativeGasUsed: 50000000000000,
  from: '0x997f0b88b4e1203661e176029fe32cfdf7c388be',
  gasUsed: 50000000000000,
  logs: [],
  logsBloom:
   '0x0000000000000000000000000000000000000000000000000000000000000000',
  status: true,
  to: null,
  transactionHash:
   '0x8bf8d6c3719af85d3ba48944ce95a64242b120c22c62bb6c311d043ab64759bd',
  transactionIndex: 1 }
```

With the created contract address `0x21E441A447D2AEEe8107b10d3f356eA9FD565e66`, we can interact with the contract.

Recall the our `SimpleStorage` module has a instance variable stored in the storage, let's create a script to read Ethereum's contract storage:

```bash
$ cat get_storage_at.js
const {argv} = require("process");
const Web3 = require("web3");
const rpcURL = "http://127.0.0.1:8214";
const web3 = new Web3(rpcURL);

const contractAddress = argv[2];
const position = parseInt(argv[3]);

web3.eth.getStorageAt(contractAddress, position, (err, value) => {
  if (err) {
    console.log("Error: ", err);
  } else {
    console.log("Value: ", value);
  }
});
```

Now let's check the storage value:

```bash
$ node get_storage_at.js 0x21E441A447D2AEEe8107b10d3f356eA9FD565e66 0
Value:  0x000000000000000000000000000000000000000000000000000000000000007b
```

`7b` in hex is exactly 123, the value SimpleStorage contract set to its `storedData` variable in the constructor.

Having created a contract, it won't be fun unless we can call the contract:

```bash
$ cat call_contract.js
const {readFileSync, writeFileSync} = require("fs");
const {argv} = require("process");
const Web3 = require("web3");
const {Transaction} = require("ethereumjs-tx");
const rpcURL = "http://127.0.0.1:8214";
const web3 = new Web3(rpcURL);

const privateKey = "002c03c0cd7537d80e47456c33102593f4ae295650c21344b9e11ce9071b988f";
const fromAddress = web3.eth.accounts.privateKeyToAccount(privateKey).address;

const contractAddress = argv[2];
const contractArg = parseInt(argv[3]);

const abi = JSON.parse(readFileSync("SS/SimpleStorage.abi"));
const contract = new web3.eth.Contract(abi, contractAddress);
const data = contract.methods.set(contractArg).encodeABI();

web3.eth.getTransactionCount(fromAddress, (err, txCount) => {
  if (err) {
    console.log("Getting transaction count error: ", err);
    return;
  }
  console.log("Nonce: ", txCount);

  const txObject = {
    nonce:    web3.utils.toHex(txCount),
    to:       contractAddress,
    gasLimit: web3.utils.toHex(5000),
    gasPrice: web3.utils.toHex(web3.utils.toWei('1', 'gwei')),
    data: data
  };
  const tx = new Transaction(txObject);
  tx.sign(Buffer.from(privateKey, "hex"));
  const serializedTx = tx.serialize();
  const raw = '0x' + serializedTx.toString('hex');

  web3.eth.sendSignedTransaction(raw, (err, txHash) => {
    if (err) {
      console.log("Sending tx error: ", err);
    } else {
      console.log('txHash:', txHash);
    }
  });
});
```

Calling the contract with a different value would alter the value on chain:

```bash
$ node call_contract.js 0x21E441A447D2AEEe8107b10d3f356eA9FD565e66 336
Nonce:  3
txHash: 0xaae9b1a7d2b920171428465aa248b1bd59680f3655b20bc2a4d7ee1cde0db7af
$ node get_storage_at.js 0x21E441A447D2AEEe8107b10d3f356eA9FD565e66 0
Value:  0x0000000000000000000000000000000000000000000000000000000000000150
```

The value in contract storage is updated here as we have called the SimpleStorage contract.
