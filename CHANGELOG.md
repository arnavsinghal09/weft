# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Until 1.0.0,
minor versions may contain breaking changes.

## [Unreleased]

### Added

- Project skeleton: Cargo workspace with the `weft-dst` crate (installs the
  `weft` binary; `--help` and `--version` only).
- CI pipeline with blocking gates: rustfmt + clippy (`-D warnings`), RustSec
  vulnerability audit (`cargo deny check advisories`), and license compliance
  (`cargo deny check licenses bans sources`), plus tests and Codecov coverage
  upload.
- Community files: README, CONTRIBUTING, SECURITY policy with private
  disclosure process, Contributor Covenant 2.1 code of conduct, GOVERNANCE,
  issue and PR templates.
- `PROJECT_NOTES.md` with the full planned architecture, language rationale,
  and per-session context-loading workflow (graphify).
- Dual MIT/Apache-2.0 licensing.
