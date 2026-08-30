# Release process

Publish crates in dependency order:

1. `lenso-capability-notification-template`
2. `lenso-notification-template-postgres-plugin`

Publication is manual-only from reviewed `main` through
`.github/workflows/release-plz.yml`. A push may refresh a Release-plz pull
request but cannot publish. Live publication requires `live=true` and literal
confirmation `publish` on `main`.

## Trusted Publisher

Configure one crates.io Trusted Publisher per crate:

- owner: `LioRael`
- repository: `lenso-notification-template-plugin`
- workflow: `release-plz.yml`
- environment: unset

Only the confirmed live job receives `id-token: write`; there is no registry
token fallback. Allocate a previously unowned crate name once with a temporary,
narrowly scoped token, revoke it, and use OIDC thereafter.

## Evidence

Run the README validation and PostgreSQL acceptance commands. Confirm the
Capability projection is fresh, both public packages include runtime assets,
and the lockfile contains every exact Lenso dependency revision from the root
manifest.
