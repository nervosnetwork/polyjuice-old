TARGET := riscv64-unknown-elf
CC := $(TARGET)-gcc
LD := $(TARGET)-gcc
CFLAGS := -O3 -I deps/flatcc/include -I deps/secp256k1/src -I deps/secp256k1 -I c -Wall -Werror -Wno-nonnull-compare -Wno-unused-function
LDFLAGS := -Wl,-static -fdata-sections -ffunction-sections -Wl,--gc-sections -Wl,-s
SECP256K1_SRC := deps/secp256k1/src/ecmult_static_pre_context.h
FLATCC := deps/flatcc/bin/flatcc

# docker pull nervos/ckb-riscv-gnu-toolchain:bionic-20190702
BUILDER_DOCKER := nervos/ckb-riscv-gnu-toolchain@sha256:7b168b4b109a0f741078a71b7c4dddaf1d283a5244608f7851f5714fbad273ba

all: cells/lock cells/contract_lock

all-via-docker:
	docker run --rm -v `pwd`:/code ${BUILDER_DOCKER} bash -c "cd /code && make"

cells/lock: c/lock.c c/protocol_reader.h $(SECP256K1_SRC)
	$(CC) $(CFLAGS) $(LDFLAGS) -o $@ $<

cells/contract_lock: c/contract_lock.c c/protocol_reader.h
	$(CC) $(CFLAGS) $(LDFLAGS) -o $@ $<

$(SECP256K1_SRC):
	cd deps/secp256k1 && \
		./autogen.sh && \
		CC=$(CC) LD=$(LD) ./configure --with-bignum=no --enable-ecmult-static-precomputation --enable-endomorphism --enable-module-recovery --host=$(TARGET) && \
		make src/ecmult_static_pre_context.h src/ecmult_static_context.h

c/protocol_reader.h: c/protocol.fbs $(FLATCC)
	$(FLATCC) -c --reader -o c $<

$(FLATCC):
	cd deps/flatcc && scripts/initbuild.sh make && scripts/build.sh

clean:
	rm -rf cells/lock
	cd deps/flatcc && scripts/cleanall.sh
	cd deps/secp256k1 && make clean

.PHONY: all all-via-docker clean
