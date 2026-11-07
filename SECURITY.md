# Security

## Reporting a vulnerability

Please report privately via GitHub's [private vulnerability reporting](https://github.com/thebigheadlab/spur/security/advisories/new). Don't open a public issue.

## This repo is public

Everything here (code, commits, issues, PRs, comments, CI logs) is public and permanent. Before you push or post:

- **No secrets.** No tokens, API keys, passwords, private keys, or `.env` files. Use GitHub Actions secrets or local env vars. Spur config must reference secrets (keychain / env), never embed them.
- **No personal data.** No private emails, phone numbers, home paths (`/Users/<name>`, `/home/<name>`), internal hostnames or URLs.
- **Noreply commit email.** Commit as `<id>+<login>@users.noreply.github.com`:
  ```sh
  git config user.email "<id>+<login>@users.noreply.github.com"
  ```
  Find it at GitHub → Settings → Emails, and enable "Keep my email addresses private" and "Block command line pushes that expose my email".
- **Redact logs** before pasting them into issues.

CI (`.github/workflows/secrets.yml`) runs gitleaks and rejects commits whose author/committer email isn't a noreply address.

## If a secret leaks

Revoke/rotate it first. Removing it from history is not enough: assume it was copied the moment it was pushed.
