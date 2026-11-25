# Project Governance

## Roles

- **Contributors**: Anyone contributing issues, discussions, documentation, themes, or pull requests.
- **Area Maintainers**: Trusted contributors listed in `.github/CODEOWNERS` with merge and review responsibility for specific workspace crates (e.g., `vynl-images`, `vynl-player`, `vynl-tui`).
- **Core Maintainers**: Responsible for overall architecture, release signing/publishing, security advisories, and Code of Conduct enforcement.

## Decision-Making Process

1. **Routine Changes**: Decided via standard Pull Request review by at least one relevant Area Maintainer.
2. **Significant Changes (RFCs)**: Changes affecting public CLI/IPC contracts, configuration schemas, crate boundaries, or core UX must go through an RFC in `docs/rfcs/` discussed in GitHub Discussions.
3. **Architecture Decision Records (ADRs)**: Once accepted, durable architectural decisions are recorded in `docs/adr/`.
4. **Consensus**: Decisions aim for lazy consensus among maintainers; if consensus cannot be reached, Core Maintainers make the final call and document the rationale in an ADR.
