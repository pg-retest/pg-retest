# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in pg-retest, please report it responsibly.

**Do NOT open a public GitHub issue for security vulnerabilities.**

Instead, email: **matt@theyonk.com** (or open a private security advisory on GitHub)

You should receive a response within 48 hours. We will work with you to understand the issue and coordinate a fix before any public disclosure.

## Security Considerations

pg-retest interacts with live PostgreSQL databases. Please review these important security notes:

- **Web dashboard** binds to `127.0.0.1` by default with bearer token authentication. If you expose it on a network, use a reverse proxy with TLS.
- **Connection strings** may contain credentials. Use `--target-env` to read from environment variables instead of CLI arguments. Passwords are redacted before storage.
- **AI tuning** (`pg-retest tune`) can modify database configuration when `--apply` is used. It defaults to dry-run mode. A safety allowlist restricts which parameters can be changed.
- **TLS** is enabled by default (`--tls-mode prefer`). Use `--tls-mode require` for cloud-hosted databases.

## Supported Versions

| Version | Supported |
|---------|-----------|
| 1.0.0-rc.x | Yes       |
