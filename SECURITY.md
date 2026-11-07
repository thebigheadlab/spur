# Security policy

## Reporting a vulnerability

Please report vulnerabilities privately via [GitHub security advisories](https://github.com/thebigheadlab/spur/security/advisories/new). Do not open a public issue.

## Public repository hygiene

This repository is public. Never commit or post secrets, tokens, credentials, private keys, personal data, or home/local paths, in code, commits, PRs, issues or comments.

- Set a noreply git email: `git config user.email "<id>+<login>@users.noreply.github.com"`.
- The `hygiene` workflow scans every PR and the full history weekly.
- **If a secret leaks, revoke or rotate it first.** Removing the commit does not make it safe.
