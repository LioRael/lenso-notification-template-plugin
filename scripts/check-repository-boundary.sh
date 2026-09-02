#!/usr/bin/env bash
set -euo pipefail

forbidden='lenso-platform-|lenso-module-|HostBuilder|HostLinkedModule|ModuleManifest|platform_core|platform_module|lenso-http-auth'
if rg -n "$forbidden" Cargo.toml crates README.md docs --glob '!**/generated.rs'; then
  echo "legacy Lenso dependency or API found" >&2
  exit 1
fi

if rg -n 'CREATE TABLE (notifications|deliveries|intents|render_snapshots|identities|organizations|memberships|roles|permissions)' crates/lenso-notification-template-postgres-plugin/migrations; then
  echo "Notification Template crossed another Plugin storage boundary" >&2
  exit 1
fi

if rg -n '(println!|eprintln!|dbg!|tracing::[a-z]+!)\([^\n]*(variable|rendered|database_url|assertion|secret)' crates/lenso-notification-template-postgres-plugin/src --glob '!postgres_tests.rs'; then
  echo "sensitive template material reached a diagnostic macro" >&2
  exit 1
fi

machine_root='/''Users/'
if rg -n "$machine_root" README.md docs scripts .github AGENTS.md; then
  echo "public repository material contains a machine-specific absolute path" >&2
  exit 1
fi

if find . -name .gitkeep -print -quit | rg -q .; then
  echo ".gitkeep placeholders are not allowed in the released repository" >&2
  exit 1
fi

printf 'repository boundary is template-owned, authority-separated, and descriptor-first\n'
