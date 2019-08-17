#include <stddef.h>
#include <stdint.h>

#include "keccak.h"
#include "rlp.h"

#include "ckb_syscalls.h"
#include "protocol_reader.h"

/* TODO: maybe we should really use 256bit bigint */
typedef unsigned __int128 uint128_t;
#define CAPACITY_TO_WEI 10000000000

#define ERROR_HEX_DECODING -1
#define ERROR_BUFFER_NOT_ENOUGH -2
#define ERROR_LOAD_WITNESS -3
#define ERROR_INVALID_TOKEN_TYPE -4
#define ERROR_VALUE_OUT_OF_RANGE -5
#define ERROR_INVALID_VALUE -6
#define ERROR_LOAD_SCRIPT -7
#define ERROR_INVALID_SCRIPT -8
#define ERROR_LOAD_DATA -9
#define ERROR_TOO_MANY_MAIN_CELLS -10
#define ERROR_LOAD_CAPACITY -11
#define ERROR_INDEX_OUT_OF_BOUND -12
#define ERROR_TOO_MANY_OUTPUT_CELLS -13
#define ERROR_SECP_INITIALIZATION -14
#define ERROR_SECP_LOAD_SIGNATURE -15
#define ERROR_SECP_RECOVER_PUBKEY -16
#define ERROR_SECP_SERIALIZE_PUBKEY -17
#define ERROR_INVALID_PUBKEY_HASH -18
#define ERROR_ARGUMENTS -19
#define ERROR_DATA_LENGTH -20
#define ERROR_LOAD_TX_HASH -21
#define ERROR_INVALID_NONCE -22
#define ERROR_RLP -23
#define ERROR_INVALID_CAPACITY -24
#define ERROR_CHAIN_ID_NOT_FIT -25
#define ERROR_V -26
#define ERROR_OVERFLOW -27

#undef ns
#define ns(x) FLATBUFFERS_WRAP_NAMESPACE(Ckb_Protocol, x)

#define CHAIN_ID 1

/*
 * We are including secp256k1 implementation directly so gcc can strip
 * unused functions. For some unknown reasons, if we link in libsecp256k1.a
 * directly, the final binary will include all functions rather than those used.
 */
#define HAVE_CONFIG_H 1
#define USE_EXTERNAL_DEFAULT_CALLBACKS
#include <secp256k1.c>

void secp256k1_default_illegal_callback_fn(const char* str, void* data) {
  (void)str;
  (void)data;
  exit(-1000);
}

void secp256k1_default_error_callback_fn(const char* str, void* data) {
  (void)str;
  (void)data;
  exit(-1001);
}

int secp256k1_custom_verify_only_initialize(
    secp256k1_context* context, secp256k1_ge_storage (*pre_g)[],
    secp256k1_ge_storage (*pre_g_128)[]) {
  context->illegal_callback = default_illegal_callback;
  context->error_callback = default_error_callback;

  secp256k1_ecmult_context_init(&context->ecmult_ctx);
  secp256k1_ecmult_gen_context_init(&context->ecmult_gen_ctx);

  context->ecmult_ctx.pre_g = pre_g;
  context->ecmult_ctx.pre_g_128 = pre_g_128;

  return 1;
}

/* Utilities */
int char_to_int(uint8_t ch) {
  if (ch >= '0' && ch <= '9') {
    return ch - '0';
  }
  if (ch >= 'a' && ch <= 'f') {
    return ch - 'a' + 10;
  }
  return ERROR_HEX_DECODING;
}

int hex_to_bin(uint8_t* buf, size_t buf_len, const uint8_t* hex) {
  int i = 0;

  for (; i < buf_len && hex[i * 2] != '\0' && hex[i * 2 + 1] != '\0'; i++) {
    int a = char_to_int(hex[i * 2]);
    int b = char_to_int(hex[i * 2 + 1]);

    if (a < 0 || b < 0) {
      return ERROR_HEX_DECODING;
    }

    buf[i] = ((a & 0xF) << 4) | (b & 0xF);
  }

  if (i == buf_len && hex[i * 2] != '\0') {
    return ERROR_HEX_DECODING;
  }
  return i;
}

int extract_bytes(ns(Bytes_table_t) bytes, uint8_t* buffer, size_t* s) {
  flatbuffers_uint8_vec_t seq = ns(Bytes_seq(bytes));
  size_t len = flatbuffers_uint8_vec_len(seq);

  if (len > *s) {
    return ERROR_BUFFER_NOT_ENOUGH;
  }

  for (size_t i = 0; i < len; i++) {
    buffer[i] = flatbuffers_uint8_vec_at(seq, i);
  }
  *s = len;

  return CKB_SUCCESS;
}

int extract_data_from_witness(uint8_t* data, size_t* length,
                              size_t input_index) {
  uint8_t witness_buffer[32768];
  volatile uint64_t len = 32768;
  int ret =
      ckb_load_witness(witness_buffer, &len, 0, input_index, CKB_SOURCE_INPUT);
  if (ret != CKB_SUCCESS) {
    return ERROR_LOAD_WITNESS;
  }
  if (len > 32768) {
    return ERROR_BUFFER_NOT_ENOUGH;
  }

  ns(Witness_table_t) witness_table;
  if (!(witness_table = ns(Witness_as_root(witness_buffer)))) {
    return ERROR_LOAD_WITNESS;
  }
  ns(Bytes_vec_t) args = ns(Witness_data(witness_table));
  if (ns(Bytes_vec_len(args)) != 1) {
    return ERROR_LOAD_WITNESS;
  }
  return extract_bytes(ns(Bytes_vec_at(args, 0)), data, length);
}

int rlp_string_to_integer(const uint8_t* source, int64_t len,
                          const crlp_token_t* t, uint128_t* value) {
  if (!crlp_token_is_string(t)) {
    return ERROR_INVALID_TOKEN_TYPE;
  }
  int start_char = t->data.string.start_char;
  int end_char = t->data.string.end_char;
  if (end_char - start_char > 16) {
    return ERROR_VALUE_OUT_OF_RANGE;
  }
  const uint8_t* p = crlp_token_string_pointer(source, len, t);
  if ((!p) || (!p[0])) {
    return ERROR_INVALID_VALUE;
  }
  *value = 0;
  for (int i = 0; i < end_char - start_char; i++) {
    *value = ((*value) << 8) | p[i];
  }
  return CKB_SUCCESS;
}

/*
 * A normal account has 2 types of cells:
 * * Main cell: cell data is at least 4 bytes long containing nonce. When used
 * in a transaction, the witness part corresponding to this input cell will have
 * Transaction structure.
 * * Fund cell: cell data is empty, those are used when others send this account
 * some fund. They can do so by create a fund cell.
 *
 * TODO: You might notice that an attacker can create a fund cell with
 * invalid nonce data, hoping to disrupt the data. Later when we have contract
 * account, attacker might want to attack the data by creating fake main cell
 * with invalid data. This can be fixed by introducing a type script guarding
 * necessary rules.
 */
int validate_input_cells(const uint8_t* current_script_hash, uint64_t* nonce,
                         uint64_t* from_capacities,
                         uint64_t* other_capacities) {
  uint64_t current_capacities = 0;
  size_t i = 0, input_index = SIZE_MAX;
  int looping = 1;
  *nonce = UINT64_MAX;
  while (looping && i < SIZE_MAX) {
    uint8_t hash[32];
    volatile uint64_t len = 32;
    int ret = ckb_load_cell_by_field(hash, &len, 0, i, CKB_SOURCE_INPUT,
                                     CKB_CELL_FIELD_LOCK_HASH);
    switch (ret) {
      case CKB_INDEX_OUT_OF_BOUND:
        looping = 0;
        break;
      case CKB_SUCCESS:
        if (len != 32) {
          return ERROR_LOAD_SCRIPT;
        }
        if (memcmp(current_script_hash, hash, 32) == 0) {
          uint8_t nonce_buffer[9];
          len = 9;
          ret = ckb_load_cell_by_field(nonce_buffer, &len, 0, i,
                                       CKB_SOURCE_INPUT, CKB_CELL_FIELD_DATA);
          if (ret != CKB_SUCCESS) {
            return ERROR_LOAD_DATA;
          }
          if (len >= 9) {
            if (input_index != SIZE_MAX) {
              /* Multiple main cell */
              return ERROR_TOO_MANY_MAIN_CELLS;
            }
            *nonce = *((uint64_t*)&nonce_buffer[1]);
            input_index = i;
          }
          volatile uint64_t capacity = 0;
          len = 8;
          ret =
              ckb_load_cell_by_field((void*)&capacity, &len, 0, i,
                                     CKB_SOURCE_INPUT, CKB_CELL_FIELD_CAPACITY);
          if (ret != CKB_SUCCESS) {
            return ERROR_LOAD_CAPACITY;
          }
          current_capacities += capacity;
          i++;
        } else {
          looping = 0;
        }
        break;
      default:
        return ERROR_LOAD_SCRIPT;
    }
  }
  if (i == 0) {
    return ERROR_INVALID_SCRIPT;
  }
  if (i == SIZE_MAX) {
    return ERROR_INDEX_OUT_OF_BOUND;
  }
  *from_capacities = current_capacities;
  current_capacities = 0;
  looping = 1;
  while (looping && i < SIZE_MAX) {
    uint8_t hash[32];
    volatile uint64_t len = 32;
    int ret = ckb_load_cell_by_field(hash, &len, 0, i, CKB_SOURCE_INPUT,
                                     CKB_CELL_FIELD_LOCK_HASH);
    switch (ret) {
      case CKB_INDEX_OUT_OF_BOUND:
        looping = 0;
        break;
      case CKB_SUCCESS:
        if (len != 32) {
          return ERROR_LOAD_SCRIPT;
        }
        if (memcmp(current_script_hash, hash, 32) == 0) {
          return ERROR_INVALID_SCRIPT;
        }
        {
          volatile uint64_t capacity = 0;
          len = 8;
          ret =
              ckb_load_cell_by_field((void*)&capacity, &len, 0, i,
                                     CKB_SOURCE_INPUT, CKB_CELL_FIELD_CAPACITY);
          if (ret != CKB_SUCCESS) {
            return ERROR_LOAD_CAPACITY;
          }
          current_capacities += capacity;
        }
        i++;
        break;
      default:
        return ERROR_LOAD_SCRIPT;
    }
  }
  if (i == SIZE_MAX) {
    return ERROR_INDEX_OUT_OF_BOUND;
  }
  *other_capacities = current_capacities;
  return CKB_SUCCESS;
}

int validate_output_cells(const uint8_t* current_script_hash, uint64_t* nonce,
                          uint64_t* sent_capacity, uint64_t* change_capacity) {
  uint8_t hash[32];
  volatile uint64_t len = 32;
  int ret = ckb_load_cell_by_field(hash, &len, 0, 0, CKB_SOURCE_OUTPUT,
                                   CKB_CELL_FIELD_LOCK_HASH);
  if (ret != CKB_SUCCESS || len != 32) {
    return ERROR_LOAD_SCRIPT;
  }
  if (memcmp(current_script_hash, hash, 32) != 0) {
    return ERROR_INVALID_SCRIPT;
  }
  /*
   * at least 2 outputs, first is sender account's main cell, after that it
   * could contain anything.
   */
  /* Gather output nonce */
  uint8_t nonce_buffer[9];
  len = 9;
  ret = ckb_load_cell_by_field(nonce_buffer, &len, 0, 0, CKB_SOURCE_OUTPUT,
                               CKB_CELL_FIELD_DATA);
  if (ret != CKB_SUCCESS || len < 9) {
    return ERROR_LOAD_DATA;
  }
  *nonce = *((uint64_t*)&nonce_buffer[1]);
  /* Gather change capacity */
  len = 8;
  ret = ckb_load_cell_by_field((void*)change_capacity, &len, 0, 0,
                               CKB_SOURCE_OUTPUT, CKB_CELL_FIELD_CAPACITY);
  if (ret != CKB_SUCCESS || len != 8) {
    return ERROR_LOAD_CAPACITY;
  }
  len = 8;
  /* Gather other other capacities */
  size_t i = 1;
  int looping = 1;
  *sent_capacity = 0;
  for (; looping && i < SIZE_MAX; i++) {
    volatile uint64_t current_capacity = 0;
    len = 8;
    ret = ckb_load_cell_by_field((void*)&current_capacity, &len, 0, i,
                                 CKB_SOURCE_OUTPUT, CKB_CELL_FIELD_CAPACITY);
    switch (ret) {
      case CKB_INDEX_OUT_OF_BOUND:
        looping = 0;
        break;
      case CKB_SUCCESS:
        if (len != 8) {
          return ERROR_LOAD_CAPACITY;
        }
        len = 8;
        ret = ckb_load_cell_by_field(hash, &len, 0, i, CKB_SOURCE_OUTPUT,
                                     CKB_CELL_FIELD_LOCK_HASH);
        if (ret != CKB_SUCCESS || len != 32) {
          return ERROR_LOAD_SCRIPT;
        }
        if (memcmp(current_script_hash, hash, 32) == 0) {
          return ERROR_INVALID_SCRIPT;
        }
        *sent_capacity += current_capacity;
        break;
      default:
        return ERROR_LOAD_CAPACITY;
    }
  }
  if (i == SIZE_MAX) {
    return ERROR_INDEX_OUT_OF_BOUND;
  }
  return CKB_SUCCESS;
}

int validate_from_to(char* argv[]) {
  uint8_t script[1024];
  volatile uint64_t len = 1024;
  int ret = ckb_load_cell_by_field(script, &len, 0, 0, CKB_SOURCE_INPUT,
                                   CKB_CELL_FIELD_LOCK);
  if (ret != CKB_SUCCESS) {
    return ret;
  }
  ns(Script_table_t) script_table;
  if (!(script_table = ns(Script_as_root(script)))) {
    return ERROR_LOAD_SCRIPT;
  }
  ns(Bytes_vec_t) args = ns(Script_args(script_table));
  if (ns(Bytes_vec_len(args)) != 1) {
    return ERROR_INVALID_SCRIPT;
  }
  uint8_t buffer[20];
  size_t buffer_length = 20;
  ret = extract_bytes(ns(Bytes_vec_at(args, 0)), buffer, &buffer_length);
  if (ret != CKB_SUCCESS || buffer_length != 20) {
    return ERROR_INVALID_SCRIPT;
  }
  /* The length of argv[1] will be verified later in validate_signature */
  if (memcmp(buffer, argv[1], 20) != 0) {
    return ERROR_INVALID_SCRIPT;
  }
  /* TODO: validate to field once we figure out the format of contract calling
   */
  return CKB_SUCCESS;
}

/*
 * message should be 32 bytes, compact_signature should be
 * 64 bytes.
 */
int validate_signature(const uint8_t* message, const uint8_t* compact_signature,
                       int recid, char* argv[]) {
  secp256k1_context context;
  if (secp256k1_custom_verify_only_initialize(
          &context,
          (secp256k1_ge_storage(*)[]) & secp256k1_ecmult_static_pre_context,
          (secp256k1_ge_storage(*)[]) &
              secp256k1_ecmult_static_pre128_context) != 1) {
    return ERROR_SECP_INITIALIZATION;
  }

  secp256k1_ecdsa_recoverable_signature signature;
  if (secp256k1_ecdsa_recoverable_signature_parse_compact(
          &context, &signature, compact_signature, recid) == 0) {
    return ERROR_SECP_LOAD_SIGNATURE;
  }

  secp256k1_pubkey pubkey;
  if (secp256k1_ecdsa_recover(&context, &pubkey, &signature, message) != 1) {
    return ERROR_SECP_RECOVER_PUBKEY;
  }

  uint8_t pubkey_bytes[65];
  size_t pubkey_bytes_length = 65;
  if (secp256k1_ec_pubkey_serialize(&context, pubkey_bytes,
                                    &pubkey_bytes_length, &pubkey,
                                    SECP256K1_EC_UNCOMPRESSED) != 1) {
    return ERROR_SECP_SERIALIZE_PUBKEY;
  }
  if (pubkey_bytes_length != 65) {
    return ERROR_SECP_SERIALIZE_PUBKEY;
  }

  sha3_ctx_t ctx;
  sha3_init(&ctx, 32);
  sha3_update(&ctx, &pubkey_bytes[1], pubkey_bytes_length - 1);
  uint8_t pubkey_hash[32];
  keccak_final(pubkey_hash, &ctx);

  if (ckb_argv_length(argv, 1) != 20) {
    return ERROR_INVALID_PUBKEY_HASH;
  }

  if (memcmp(argv[1], &pubkey_hash[12], 20) != 0) {
    return ERROR_INVALID_PUBKEY_HASH;
  }

  return CKB_SUCCESS;
}

int main(int argc, char* argv[]) {
  /* program <ETH address> <RLP serialization of ETH transaction> */
  if (argc != 2) {
    return ERROR_ARGUMENTS;
  }

  uint8_t current_script_hash[32];
  volatile uint64_t len = 32;
  int ret = ckb_load_script_hash(current_script_hash, &len, 0);
  if (ret != CKB_SUCCESS || len != 32) {
    return ERROR_LOAD_SCRIPT;
  }

  uint64_t input_nonce = UINT64_MAX, from_capacity = 0, other_capacity = 0;
  ret = validate_input_cells(current_script_hash, &input_nonce, &from_capacity,
                             &other_capacity);
  if (ret != 0) {
    return ret;
  }

  /* Load witness from first input */
  uint8_t data[32768];
  size_t data_length = 32768;
  ret = extract_data_from_witness(data, &data_length, 0);
  if (ret != 0) {
    return ret;
  }
  /* Set 1 aside for added chain ID */
  if (data_length >= 32768) {
    return ERROR_DATA_LENGTH;
  }

  if (data[0] == 0xFF) {
    /*
     * This is a special mode here, here we bypass all the Ethereum validation
     * rules and only do the signature check on the transaction hash loaded
     * from CKB syscall. This way, we can maintain a way to return CKB from
     * the Ethereum space back to CKB world.
     */
    if (data_length != 66) {
      return ERROR_DATA_LENGTH;
    }
    uint8_t tx_hash[32];
    len = 32;
    ret = ckb_load_tx_hash(tx_hash, &len, 0);
    if (ret != CKB_SUCCESS || len != 32) {
      return ERROR_LOAD_TX_HASH;
    }
    ret = validate_signature(tx_hash, &data[2], data[1], argv);
    if (ret != CKB_SUCCESS) {
      return ret;
    }
    return CKB_SUCCESS;
  }

  uint64_t output_nonce = UINT64_MAX;
  uint64_t sent_capacity = 0, change_capacity = 0;
  /* Validate nonce in CKB */
  ret = validate_output_cells(current_script_hash, &output_nonce,
                              &sent_capacity, &change_capacity);
  if (ret != 0) {
    return ret;
  }

  uint64_t target_nonce = (input_nonce == UINT64_MAX) ? 0 : (input_nonce + 1);
  if (output_nonce != target_nonce) {
    return ERROR_INVALID_NONCE;
  }

  crlp_token_t tokens[16];
  int token_size = crlp_parse_static(data, data_length, tokens, 16);
  if (token_size < 0) {
    return token_size;
  }
  if ((!crlp_token_is_list(&tokens[0])) ||
      (crlp_token_list_size(&tokens[0]) != 9)) {
    return ERROR_RLP;
  }

  /* Verify nonce in Ethereum matches nonce in CKB */
  int list_start_token = tokens[0].data.list.start_token;
  crlp_token_t nonce_token = tokens[list_start_token + 0];
  uint128_t rlp_nonce = 0;
  ret = rlp_string_to_integer(data, data_length, &nonce_token, &rlp_nonce);
  if (ret != CKB_SUCCESS) {
    return ret;
  }
  if (rlp_nonce != (uint128_t)output_nonce) {
    return ERROR_INVALID_NONCE;
  }

  /*
   * Verify value and fee in RLP match the value in transaction
   * to avoid malleability.
   */
  uint128_t gas_price = 0, gas_limit = 0, value;
  ret = rlp_string_to_integer(data, data_length, &tokens[list_start_token + 1],
                              &gas_price);
  if (ret != CKB_SUCCESS) {
    return ret;
  }
  ret = rlp_string_to_integer(data, data_length, &tokens[list_start_token + 2],
                              &gas_limit);
  if (ret != CKB_SUCCESS) {
    return ret;
  }
  ret = rlp_string_to_integer(data, data_length, &tokens[list_start_token + 4],
                              &value);
  if (ret != CKB_SUCCESS) {
    return ret;
  }
  uint128_t from_wei = ((uint128_t)from_capacity) * CAPACITY_TO_WEI;
  ;
  uint128_t change_wei = ((uint128_t)change_capacity) * CAPACITY_TO_WEI;
  uint128_t gas_wei = gas_price * gas_limit;
  if (from_wei != change_wei + gas_wei + value) {
    return ERROR_INVALID_CAPACITY;
  }
  uint128_t other_wei = ((uint128_t)other_capacity) * CAPACITY_TO_WEI;
  uint128_t sent_wei = ((uint128_t)sent_capacity) * CAPACITY_TO_WEI;
  if (from_wei + other_wei != gas_wei + change_wei + sent_wei) {
    return ERROR_INVALID_CAPACITY;
  }

  /*
   * Verify FROM and TO are set correctly per the Ethereum transaction
   */
  ret = validate_from_to(argv);
  if (ret != CKB_SUCCESS) {
    return ret;
  }

  crlp_token_t v = tokens[list_start_token + 6];
  crlp_token_t r = tokens[list_start_token + 7];
  crlp_token_t s = tokens[list_start_token + 8];
  if ((!crlp_token_is_string(&v)) || (crlp_token_string_length(&v) != 1)) {
    return ERROR_RLP;
  }
  if ((!crlp_token_is_string(&r)) || (crlp_token_string_length(&r) != 32)) {
    return ERROR_RLP;
  }
  if ((!crlp_token_is_string(&s)) || (crlp_token_string_length(&s) != 32)) {
    return ERROR_RLP;
  }

  /* TODO: support longer chain ID later */
  if (CHAIN_ID > 0xFF) {
    return ERROR_CHAIN_ID_NOT_FIT;
  }
  data[data_length++] = CHAIN_ID;
  tokens[list_start_token + 6] =
      crlp_create_string_token(data_length - 1, data_length);
  tokens[list_start_token + 7] = crlp_create_string_token(0, 0);
  tokens[list_start_token + 8] = crlp_create_string_token(0, 0);

  uint8_t unsigned_data[32768];
  ssize_t unsigned_data_length = crlp_assemble(
      data, data_length, tokens, token_size, 0, unsigned_data, 32768);
  if (unsigned_data_length < 0) {
    return unsigned_data_length;
  }

  sha3_ctx_t ctx;
  sha3_init(&ctx, 32);
  sha3_update(&ctx, unsigned_data, unsigned_data_length);
  uint8_t message[32];
  keccak_final(message, &ctx);

  uint8_t bit = *crlp_token_string_pointer(data, data_length, &v);
  if (bit >= CHAIN_ID * 2 + 35) {
    bit -= CHAIN_ID * 2 + 35;
  } else {
    bit -= 27;
  }
  if (!(bit == 0 || bit == 1)) {
    return ERROR_V;
  }

  uint8_t input[64];
  memcpy(input, crlp_token_string_pointer(data, data_length, &r), 32);
  memcpy(&input[32], crlp_token_string_pointer(data, data_length, &s), 32);

  ret = validate_signature(message, input, bit, argv);
  if (ret != CKB_SUCCESS) {
    return ret;
  }
  return CKB_SUCCESS;
}
