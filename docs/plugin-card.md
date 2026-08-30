# Plugin card: PostgreSQL Notification Template

## Job and deletion boundary

`lenso.notification-template.postgres` owns notification template releases,
active-version heads, administrative idempotency receipts, locale selection,
and deterministic rendered output. Removing the Plugin removes template
availability from the resolved App without a Kernel branch. It does not delete
the operator-owned PostgreSQL schema.

## Owns

- immutable `(template_id, version, locale)` source and renderer identity;
- active version head revisions and CAS transitions;
- caller, Actor, operation, idempotency-key receipts;
- exact/base/configured locale fallback;
- bounded placeholder parsing, sections, escaping, URL checks, and digests;
- seeded v1 organization invitation and access request templates.

## Does not own

- notification intents, recipients, render snapshots, retries, or receipts;
- email transport, credentials, provider delivery state, or campaigns;
- Auth issuance, identities, organization membership, or RBAC policy;
- HTTP ingress or a template-management UI.

## Contract and implementation

- Provides `lenso.notification-template@1` descriptor `1.0.0` through generated
  linked native Rust glue.
- Requires exactly one `lenso.secrets@1` Provider.
- Uses a fresh verified PostgreSQL pool for each Generation.
- Performs no DDL during activation; operator setup and upgrade own migrations.
- Service rendering requires an exact caller. Admin operations require an exact
  disjoint caller plus issuer, proof, validity, actor-kind, and operation-bound
  Auth assertion verification.
