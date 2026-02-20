.PHONY: install install-hooks

install: install-hooks

install-hooks:
	@./git-hooks/install.sh
