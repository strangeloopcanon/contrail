.PHONY: setup check test all bump

setup:
	cargo fetch --locked

check:
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings

test:
	cargo test --workspace

all: check test

bump:
	./scripts/release-bump.sh patch
