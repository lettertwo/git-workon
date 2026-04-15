# 022 — SSH `IdentityAgent` Support via `~/.ssh/config` Parsing

## Context

libgit2 (and by extension `git2_credentials`) determines which SSH agent to
use solely by reading the `SSH_AUTH_SOCK` environment variable. It does not
read `~/.ssh/config` for the `IdentityAgent` directive.

The system SSH client (`ssh`, `git`) does read `IdentityAgent`, so users who
configure a third-party SSH agent (e.g. 1Password) via that directive find that
`git` commands work while `git-workon` fails with "try with an other username"
— the error libgit2 surfaces when all credential options are exhausted.

The root cause: `SSH_AUTH_SOCK` points to the macOS system agent (set by
launchd at login), not to the agent configured in `~/.ssh/config`. There is no
OS-level mechanism to redirect `SSH_AUTH_SOCK` automatically, so the
application must bridge the gap.

## Alternatives Considered

**Shell config workaround** — users set `SSH_AUTH_SOCK` in `~/.config/fish/config.fish`
(or equivalent). Rejected: puts the burden on the user, is not discoverable,
and breaks whenever the socket path changes.

**Shell out to `git`** — replace `RepoBuilder::clone` with `std::process::Command`
running `git clone --bare`. `git` respects `~/.ssh/config` fully. Rejected: the
point of bundling libgit2 is to avoid managing external processes; shelling out
trades one dependency for another.

**Full SSH agent protocol client** — open the `IdentityAgent` socket directly,
speak the agent protocol (e.g. via `ssh-agent-client-rs`), and supply key
material and signatures to libgit2 via a custom `Cred::ssh_key` callback.
Correct but significantly more complex, and `git2_credentials` offers no
extension point for this.

**Contribute `IdentityAgent` to `git2_credentials`** — the crate's pest grammar
already parses arbitrary key-value pairs; only the Rust extraction code would
need to change. Still leaves the `SSH_AUTH_SOCK` mutation problem, since
libgit2 reads that env var directly. Viable upstream contribution for the
future, but does not help now.

## Decision

Before each libgit2 SSH operation, parse `~/.ssh/config` for the `IdentityAgent`
directive matching the target host and, if found, update `SSH_AUTH_SOCK` to
that path.

The implementation lives in `git-workon-lib/src/ssh_config.rs`:

- `extract_host_from_url(url)` — parses SCP-style (`git@host:...`) and
  URL-style (`ssh://`, `https://`) remote URLs to extract the hostname.
- `resolve_identity_agent(host)` — reads `~/.ssh/config` line-by-line,
  matches `Host` patterns (exact and `*` glob), extracts the `IdentityAgent`
  value, and returns the resolved `PathBuf` after `~` expansion and
  double-quote stripping.
- `apply_identity_agent(url)` — calls the two functions above and, on success,
  calls `std::env::set_var("SSH_AUTH_SOCK", path)`.

`get_remote_callbacks(url: Option<&str>)` calls `apply_identity_agent` before
constructing the `CredentialHandler`, covering all three call sites: clone,
fetch (PR workflow), and default-branch discovery.

`set_var` is process-global and not thread-safe. This is intentional: `git-workon`
is a single-threaded CLI, and the mutation happens once per operation before
any libgit2 SSH connection is opened.

## Consequences

- Users with `IdentityAgent` in `~/.ssh/config` (1Password, etc.) no longer
  need to manually set `SSH_AUTH_SOCK` in their shell config.
- `SSH_AUTH_SOCK` is mutated in-process; if a future use case requires parallel
  operations against different hosts with different agents, this will need
  revisiting.
- Known limitations (acceptable for v1): `Include` directives are not followed,
  `Match` blocks are not supported, and SSH token expansion (`%d`, `%h`, etc.)
  is not implemented beyond `~`.

## References

- `git-workon-lib/src/ssh_config.rs` — implementation and unit tests
- `git-workon-lib/src/get_remote_callbacks.rs` — call site
