# Repository Guidelines

## Sources of truth

- Start with `README.md` and `docs/architecture.md`. Follow
  `docs/connector-contract.md` for the contract, `docs/connector-sdk.md`
  for crate boundaries, `docs/connector-implementation.md` for Connector
  conventions, and `docs/adapter-implementation.md` for Query Engine adapter
  conventions.

## Issues, commits, and changelog

- Follow `docs/issue-workflow.md` for Issues, labels, milestones, commit and
  pull request titles, and changelog entries.

## Structure and ownership

- Organize modules and files so the data model and its ownership are evident
  from the file tree. Keep model-owned logic and flows nearby, and separate
  external representations and I/O at boundaries.
- Follow the crate responsibilities and dependency direction in `README.md` and
  `docs/connector-sdk.md`.
- Keep domain concepts and the data model minimal. Do not introduce another
  name, trait, wrapper, or intermediate representation when existing types and
  operations already express the behavior.

## Rust design

- Use methods for behavior owned by a type or state, and free functions for pure
  transformations that belong to neither.
- Do not add trivial getters; expose fields directly.
- Do not add type erasure or abstraction solely to hide generics.
- Write English rustdoc for non-obvious types and methods, including private ones.
- Do not anticipate exceptions, guards, fallbacks, constants, or limits absent
  from the requirements or contract.

## Tests

- Test observable behavior where it is used, not test-only mocks or helpers.
