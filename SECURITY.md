# Security Policy

## Supported Versions

Security fixes are applied to the latest minor release series on `main`.

| Version | Supported |
| :--- | :--- |
| `0.1.x` (development) | Yes |

## Reporting a Vulnerability

Please **do not** report security vulnerabilities through public GitHub issues or discussions.

Instead, use **GitHub Private Vulnerability Reporting** under the **Security** tab of this repository (`Security → Advisories → Report a vulnerability`).

We aim to acknowledge reports within **72 hours** and provide a remediation plan or patch timeline promptly.

## Security Design Principles

- **OAuth Tokens & Credentials**: Stored via the native OS keyring (macOS Keychain, Windows Credential Manager, Linux Secret Service via pure-Rust `zbus`). Fallback file storage enforces `0600` permissions. Tokens must never be written to logs.
- **No Telemetry / No Third-Party APIs**: `vynl` communicates exclusively with Spotify endpoints.
- **Supply-Chain Integrity**:
  - Dependencies are audited in CI via `cargo-deny` (RustSec Advisory DB).
  - Release binaries include embedded dependency metadata (`cargo-auditable`), CycloneDX SBOMs, and GitHub **SLSA Build Level 3** provenance attestations (`gh attestation verify`).
  - Third-party GitHub Actions are pinned to immutable commit SHAs with minimal token permissions.
