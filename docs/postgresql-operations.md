# PostgreSQL operations

## Setup and upgrade

Run `NotificationTemplateOperator::setup` for a new schema and
`NotificationTemplateOperator::upgrade` before activating a Plugin release with
new migrations. Runtime preparation checks the migration ledger and never runs
DDL.

The runtime role needs DML only in the owned schema. Do not grant it access to
Notification, Auth, Organization, Access Control, or email-provider storage.

## Backup and restore

Back up these tables with the Postgres Kit migration ledger:

- `notification_template_versions`;
- `notification_template_heads`;
- `notification_template_command_receipts`.

Restore versions before heads and receipts. Template source may contain product
or security-sensitive links and wording; encrypt backups and restrict operator
access. Never restore heads without the exact immutable releases they select.

## Acceptance

The optional acceptance suite creates and drops only a UUID-suffixed
`ntpl_accept_*` schema. It verifies seeded rows, release uniqueness, concurrent
CAS, receipt uniqueness, and restart persistence.

```sh
LENSO_NOTIFICATION_TEMPLATE_TEST_DATABASE_URL=postgres://postgres@127.0.0.1:5432/postgres \
  cargo test --locked -p lenso-notification-template-postgres-plugin \
  --features postgres-acceptance \
  postgres_seed_restart_immutability_cas_and_receipt_acceptance -- --nocapture
```
