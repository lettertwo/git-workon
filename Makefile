.PHONY: install install-hooks build test fmt clippy

install: install-hooks

install-hooks:
	@./git-hooks/install.sh

build:
	cargo build --workspace

test:
	cargo test --workspace

fmt:
	cargo fmt

clippy:
	cargo clippy --all-targets --all-features -- -D warnings
