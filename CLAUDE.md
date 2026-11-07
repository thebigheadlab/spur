# Agent notes for Spur

## Public repo rule (applies to everything)

This repository is public. Commits, PRs, issues, comments, CI logs and docs must never contain:

- secrets, tokens, API keys, passwords, private keys or any credentials;
- personal email addresses (commit authorship must use a GitHub `noreply` email);
- local machine paths, hostnames, or internal URLs.

Use placeholders like `<TOKEN>` or `user@example.com` in examples. CI (`hygiene` workflow: gitleaks + email checks) enforces this; never bypass or weaken it.

## Workflow

- Specs and tasks are GitHub issues; the epic is #1.
- Before pushing: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- Keep CI lean (Linux on PRs; full OS matrix on `main` or a maintainer's `/ci full` PR comment). See CONTRIBUTING.md.
