.PHONY: install install-hooks test fmt clippy

install: install-hooks

install-hooks:
	@./git-hooks/install.sh

test:
	cargo test --workspace

fmt:
	cargo fmt

clippy:
	cargo clippy --all-targets --all-features -- -D warnings
