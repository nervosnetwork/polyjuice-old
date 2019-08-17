#include <stddef.h>
#include <stdint.h>

#include "rlp.h"

#include "ckb_syscalls.h"
#include "protocol_reader.h"

#define ERROR_ARGUMENTS -19

#undef ns
#define ns(x) FLATBUFFERS_WRAP_NAMESPACE(Ckb_Protocol, x)

int main(int argc, char* argv[]) {
  if (argc != 2) {
    return ERROR_ARGUMENTS;
  }

  /*
   * TODO: validate there is a ETH account input cell in current transaction,
   * and the to field of RLP transaction matches argv[1]
   */
  return 0;
}
