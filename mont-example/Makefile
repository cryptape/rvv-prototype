CKB_DEBUGGER ?= ckb-debugger-rvv

all: build


simulator:
	cargo build --features=simulator,use_rvv_vector --bin mont-example-simulator --release

rvv-vector:
	cargo build --features=use_rvv_vector --bin mont-example-rvv-vector --release

rvv-asm:
	cargo build --features=use_rvv_asm --bin mont-example-rvv-asm --release

raw:
	cargo build --bin mont-example-raw --release

build: simulator rvv-vector rvv-asm raw

run-simulator:
	RUST_LOG=debug ${CKB_DEBUGGER} --max-cycles 1000000000 --bin ./target/riscv64imac-unknown-none-elf/release/mont-example-simulator | fgrep 'Run result: 0'

run-rvv-vector:
	RUST_LOG=debug ${CKB_DEBUGGER} --max-cycles 1000000000 --bin ./target/riscv64imac-unknown-none-elf/release/mont-example-rvv-vector | fgrep 'Run result: 0'

run-rvv-asm:
	RUST_LOG=debug ${CKB_DEBUGGER} --max-cycles 1000000000 --bin ./target/riscv64imac-unknown-none-elf/release/mont-example-rvv-asm | fgrep 'Run result: 0'

run-raw:
	RUST_LOG=debug ${CKB_DEBUGGER} --max-cycles 1000000000 --bin ./target/riscv64imac-unknown-none-elf/release/mont-example-raw | fgrep 'Run result: 0'

run: run-simulator run-rvv-vector run-rvv-asm run-raw


expand:
	cargo expand --lib
