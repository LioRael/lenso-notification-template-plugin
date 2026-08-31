# Lenso Notification Template Plugin

Provide immutable, versioned notification templates and deterministic rendering
without coupling Notification intent or delivery state to one renderer package.

## Capability

The descriptor-first `lenso.notification-template@1` role provides:

- `create_version`: create one immutable `(template_id, version, locale)` source;
- `activate_version`: move an active head with revision CAS;
- `get_version` and `list_versions`: bounded administrative inspection;
- `render`: render a stored exact or active version with locale fallback;
- `preview`: render an unpersisted candidate for an authenticated administrator.

The contract is portable and cross-lane transferable. The linked Rust provider
is `lenso.notification-template.postgres`, rooted at `notification_templates`.

## Configuration

```json
{
  "schema": "notification_templates",
  "database_url_secret": "notification-templates/database-url",
  "auth_issuer": "auth.users",
  "auth_assertion_public_key": "<url-safe-base64-ed25519-public-key>",
  "render_callers": ["notification-blue"],
  "admin_callers": ["template-admin-blue"],
  "admin_actor_kinds": ["user", "service_account"],
  "fallback_locales": ["en"]
}
```

All configuration is immutable for one resolved Generation. Caller lists are
exact Instance keys and the service and administration roles are disjoint.
Render is a Plan-bound service operation. Every administrative operation also
verifies a signed, unexpired Auth assertion for its exact Capability operation.

## Rendering safety and fallback

Template values use `{{name}}`, truthy sections `{{#name}}...{{/name}}`, and
inverted sections `{{^name}}...{{/name}}`. Variables are exact and bounded.
HTML substitutions are escaped. Variables ending in `_url` require HTTPS, with
`http://localhost` admitted only for local development. Dangerous active HTML
literals fail closed.

Locale lookup is deterministic: exact canonical locale, base language, then
the configured fallback locales in order. The response records requested and
resolved locale, the immutable template digest, renderer identity, and rendered
content digest.

Migration v1 seeds English and `en-US` organization-invitation and access-
request lifecycle templates. Later template content is added as a new immutable
version; active selection changes through CAS rather than editing a release.

## PostgreSQL setup

Runtime activation performs no DDL. Operators run setup and upgrade explicitly:

```rust,no_run
use lenso_notification_template_postgres_plugin::NotificationTemplateOperator;

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
NotificationTemplateOperator::setup(
    "postgres://postgres@127.0.0.1:5432/app",
    "notification_templates",
).await?;
# Ok(())
# }
```

See `docs/postgresql-operations.md` for backup, restore, and upgrade guidance.

## Validation

```sh
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets --all-features
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
lenso-contract-codegen workspace check --manifest-path Cargo.toml
./scripts/check-public-packages.sh
./scripts/check-repository-boundary.sh
```
