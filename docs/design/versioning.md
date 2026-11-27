# Version the repository with semantic versioning

## Design question

Should the repository's one version follow semantic versioning or calendar
versioning?

## Decision

The repository's one version follows [semantic versioning](https://semver.org)
([Versioning](../../README.md#versioning)).

## Rationale

Calendar versioning was considered because this repository releases often.
What calendar versioning saves is deciding, at each release, which part of the
version to increase. Before 1.0 that decision is only whether a change is
breaking.

What semantic versioning gives is a version that carries compatibility for
readers who will rely on it. Today the library and its callers ship together
and are updated together
([Compatibility and versions](../connector-contract.md#compatibility-and-versions)),
so no reader relies on it yet. Two expected readers will:

- **External connector authors** depend on the SDK crates through Cargo, whose
  default requirement treats versions with the same major version as
  compatible. Under calendar versioning the major version is the year, so
  Cargo would accept a breaking SDK change released within one year and reject
  an unchanged SDK released in the next.
- **Adapters and applications updated separately from this repository** need
  to tell from the version whether a library is compatible with the C ABI they
  call.

Semantic versioning serves them without a change of scheme. Moving from
calendar to semantic versioning later would make the version go down (for
example, from `2026.11.15` to `1.0.0`) and break its ordering.

## Alternatives not taken

- **Calendar versioning for everything.** It fits today's readers, who ship
  the library together with the product. The SDK crates' version
  would have to be switched or separated once external connectors appear.
- **Calendar versioning for the distribution package, semantic versioning for
  the crates and the C ABI.** It fits both kinds of reader, but every release
  carries two versions whose correspondence must be explained.
