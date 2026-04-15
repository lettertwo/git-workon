# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/lettertwo/git-workon/compare/git-workon-lib-v0.1.0...git-workon-lib-v0.2.0) - 2026-04-15

### Added

- *(lib)* respect IdentityAgent from ~/.ssh/config for SSH auth
- *(lib)* add lock parameter to add_worktree() and --lock flag to new
- *(lib)* implement is_locked() and is_valid() on WorktreeDescriptor

### Fixed

- *(lib)* ensure 'main' fallback covers repo.config() failures
