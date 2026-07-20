# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog and this project adheres to
Semantic Versioning.

## [Unreleased]

- Add the three-crate compiler workspace, DTCG resolution, deterministic CSS
  and Rust generation, locks, and the `init`, `add`, `build`, `check`, `list`,
  `explain`, and strict `doctor` CLI flows.
- Separate workspace-scoped installed-kit capability inputs from app-scoped
  theme inputs and outputs, add relocation-stable capability fingerprints,
  and reconcile HTML through an explicit insertion anchor.
- Record publication modes in plans and recovery journals, converge generated
  outputs to `0644` under restrictive umasks, and retain deterministic inline
  bootstrap CSP hash evidence.
