.PHONY: install install-dev install-man install-hooks build test fmt clippy

PREFIX ?= /usr/local

install: install-hooks install-man
	cargo install --path ./git-workon

install-dev: install-hooks install-man
	cargo install --path ./git-workon --debug

install-man:
	@mkdir -p "$(PREFIX)/share/man/man1"
	@ln -sf "$(abspath git-workon/man/git-workon.1)" "$(PREFIX)/share/man/man1/git-workon.1"
	@echo "Installed man page to $(PREFIX)/share/man/man1/git-workon.1"

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
