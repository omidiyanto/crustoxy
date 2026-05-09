#!/bin/bash

# Start system dbus for warp-svc to prevent warnings
mkdir -p /run/dbus
dbus-daemon --system --nopidfile

echo "Starting Cloudflare WARP daemon (controlled via Web UI)..."
warp-svc > /dev/null 2>&1 &
sleep 3

# Wait for daemon to settle
echo "WARP daemon started. Connection will be handled by Crustoxy proxy."

echo "Starting Crustoxy..."
exec crustoxy