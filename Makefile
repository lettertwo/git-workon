.PHONY: install install-dev install-man install-hooks build test smoke fmt clippy

PREFIX ?= /usr/local

install: install-hooks install-man
	cargo install --path ./git-workon

install-dev: install-hooks install-man
	cargo install --path ./git-workon --debug

install-man:
	@cargo build -p git-workon
	@mkdir -p "$(PREFIX)/share/man/man1"
	@build_dir=$$(cargo metadata --format-version 1 --no-deps 2>/dev/null | jq -r '.build_directory // empty'); \
	build_dir=$${build_dir:-target}; \
	ln -sf "$$(find "$$build_dir/debug/build" -path '*/git-workon-*/out/git-workon.1' -print -quit)" "$(PREFIX)/share/man/man1/git-workon.1"
	@echo "Installed man page to $(PREFIX)/share/man/man1/git-workon.1"

install-hooks:
	@./git-hooks/install.sh

build:
	cargo build --workspace

test:
	cargo test --workspace

# PTY tests (ignored by default: wall-clock-bound and load-sensitive). Spawns the review
# binary under a pseudo-terminal; covers the theme=auto probe conversation (see
# tests/pty/pty_smoke.rs) and launch/nav/streamed-startup responsiveness bounds (see
# tests/pty/pty_responsiveness.rs) — merged into one `pty` test binary, see tests/pty/main.rs.
smoke:
	cargo test -p git-workon-review --test pty -- --ignored

fmt:
	cargo fmt

clippy:
	cargo clippy --all-targets --all-features -- -D warnings
