# Agent instructions

Notification Template owns versioned template source, locale resolution, and
deterministic rendering. It does not own notification intent, recipients,
delivery, transport, campaigns, or another Plugin's storage.

- Keep the Capability Descriptor and Schemas as the only portable contract source.
- Published template versions are immutable; active selection uses CAS.
- Rendering callers are exact service Instances. Administrative operations also
  require an operation-bound, locally verified Auth assertion.
- Never log variables, rendered bodies, database URLs, assertions, or secret material.
- Runtime activation performs no DDL; use the operator setup and upgrade Surface.
