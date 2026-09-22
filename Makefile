EXE ?= target/release/inphish

.PHONY: all test perft bench

all:
	cargo build --release
	@if [ "$(EXE)" != "target/release/inphish" ]; then cp target/release/inphish "$(EXE)"; fi

test:
	cargo fmt --check
	cargo clippy --all-targets --all-features -- -D warnings
	cargo test --release

perft:
	cargo test --release -p inphzugzwang-core --test perft -- --ignored

bench: all
	$(EXE) bench
