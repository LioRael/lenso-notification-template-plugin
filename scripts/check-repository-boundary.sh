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

required_pins=(
  b763a63adc20f1ccc9955e784c0d04c21489126b
  b4a2f53df882ae51021aa3d5922d8ee41bf97c72
  c31aa142ff59b4536e2bf3e9785ccbb5bb5c0e6a
  9769bc5dc828fd9111da6d28a4ecd5f1bb198ab4
  cd35675a191d815b690c8889756dfe859a0e4d7b
  525c1012c789e6f54c3c2fdaf8507a626c93e65f
)
for pin in "${required_pins[@]}"; do
  rg -q "$pin" Cargo.toml || {
    printf 'required exact Lenso dependency pin is missing: %s\n' "$pin" >&2
    exit 1
  }
done

if find . -name .gitkeep -print -quit | rg -q .; then
  echo ".gitkeep placeholders are not allowed in the released repository" >&2
  exit 1
fi

printf 'repository boundary is template-owned, authority-separated, and descriptor-first\n'
