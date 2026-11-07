# Contributing to Spur

Thanks for helping. Specs and tasks are GitHub issues; start from the [epic](https://github.com/thebigheadlab/spur/issues/1).

## Before you open a PR

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check   # optional locally: cargo install cargo-deny
```

CI is kept lean: PRs run fmt, clippy and tests on Linux only. macOS and Windows run on `main`, nightly, or when a PR has the `ci:full` label (add it for platform-sensitive changes). `cargo-deny` runs only when dependencies change. Docs-only PRs skip CI.

## Public repo: no secrets, no personal data

This repo is public. Never put secrets, tokens, credentials, private keys, personal emails, or local paths in code, commits, PRs, issues or comments. Configure git with your GitHub noreply email:

```sh
git config user.email "<id>+<login>@users.noreply.github.com"
```

The `hygiene` CI job (gitleaks + email checks) blocks violations. If you leak a secret, rotate it first; deleting the commit is not enough.

## Ground rules

- **Minimal core.** If a feature can be an extension, it is one. Bundled extensions use only the public API.
- **Frontends never mutate state.** They send commands and render snapshots from the session server.
- **Every action is a named command.** Keymaps, palette, extensions, RPC and agents all call commands.
- **No `unsafe`** without an ADR and a `// SAFETY:` comment.
- **Dependencies** must pass `cargo deny` (permissive licenses only). Prefer few, well-maintained crates.
- **Significant decisions** get an ADR in `docs/adr/` (see ADR 0001).

## Commits and PRs

Small, focused PRs. Reference the issue (`Closes #N`). Keep the PR description short: what changed and why.

## License

By contributing you agree your work is dual licensed under MIT OR Apache-2.0.
