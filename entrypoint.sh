#!/bin/bash
set -euo pipefail

if [[ "${CRUSTOXY_ENABLE_WARP:-false}" == "true" ]]; then
  # Start system dbus for warp-svc to prevent warnings.
  mkdir -p /run/dbus
  dbus-daemon --system --nopidfile

  echo "Starting Cloudflare WARP daemon (controlled via Web UI)..."
  warp-svc > /dev/null 2>&1 &
  sleep 3

  echo "WARP daemon started. Connection will be handled by Crustoxy proxy."
else
  echo "Cloudflare WARP daemon disabled. Set CRUSTOXY_ENABLE_WARP=true and use the WARP compose profile to enable it."
fi

echo "Starting Crustoxy..."
exec crustoxy
