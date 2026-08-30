#!/usr/bin/env bash
set -euo pipefail

cargo_bin="${LENSO_CARGO_BIN:-cargo}"
metadata="$($cargo_bin metadata --locked --no-deps --format-version=1)"
repository_root="$(git rev-parse --show-toplevel)"

for package in lenso-capability-notification-template lenso-notification-template-postgres-plugin; do
  publish="$(jq -r --arg package "$package" '.packages[] | select(.name == $package) | .publish == null or (.publish | length > 0)' <<<"$metadata")"
  if [[ "$publish" != "true" ]]; then
    printf '%s is not public\n' "$package" >&2
    exit 1
  fi
done

required_source_set=(
  crates/lenso-capability-notification-template/build.rs
  crates/lenso-capability-notification-template/capability.json
  crates/lenso-capability-notification-template/schemas/render-request.schema.json
  crates/lenso-capability-notification-template/src/generated.rs
  crates/lenso-notification-template-postgres-plugin/configuration.schema.json
  crates/lenso-notification-template-postgres-plugin/migrations/001_create_notification_template_catalog.sql
  crates/lenso-notification-template-postgres-plugin/src/lib.rs
  crates/lenso-notification-template-postgres-plugin/src/render.rs
)
for source in "${required_source_set[@]}"; do
  test -f "$repository_root/$source" || {
    printf 'required package source is missing: %s\n' "$source" >&2
    exit 1
  }
done

for packaged_asset in '"capability.json"' '"schemas/*.json"' '"src/*.rs"'; do
  rg --fixed-strings --quiet "$packaged_asset" "$repository_root/crates/lenso-capability-notification-template/Cargo.toml"
done
for packaged_asset in '"configuration.schema.json"' '"migrations/*.sql"' '"src/*.rs"'; do
  rg --fixed-strings --quiet "$packaged_asset" "$repository_root/crates/lenso-notification-template-postgres-plugin/Cargo.toml"
done

printf 'public Notification Template package metadata and source sets are valid\n'
if [[ "${LENSO_RUN_PACKAGE_SMOKE:-0}" != "1" ]]; then
  printf 'full cargo package smoke skipped; set LENSO_RUN_PACKAGE_SMOKE=1 when registry access is available\n'
  exit 0
fi

flags=(--locked)
if [[ "${LENSO_PACKAGE_ALLOW_DIRTY:-0}" == "1" ]]; then
  flags+=(--allow-dirty)
fi
"$cargo_bin" package --quiet "${flags[@]}" -p lenso-capability-notification-template
"$cargo_bin" package --quiet "${flags[@]}" --no-verify -p lenso-notification-template-postgres-plugin
printf 'public Notification Template package archives were created\n'
