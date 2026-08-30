# Security

Report vulnerabilities privately to the repository maintainers. Do not include
real recipient data, rendered messages, Auth assertions, database URLs, or
Secret values in a public issue.

Template authors are privileged actors. The Plugin still escapes every HTML
variable, checks URL schemes, rejects active-content literals, and records
immutable digests so administrative authority does not weaken render integrity.
