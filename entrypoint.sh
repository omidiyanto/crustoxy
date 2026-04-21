#!/bin/bash

if [ "$ENABLE_IP_ROTATION" = "true" ]; then
    # Start system dbus for warp-svc to prevent warnings
    mkdir -p /run/dbus
    dbus-daemon --system --nopidfile

    echo "Starting Cloudflare WARP daemon..."
    warp-svc > /dev/null 2>&1 &
    sleep 3

    # Initial registration and connection
    warp-cli --accept-tos registration new > /dev/null 2>&1 || true
    warp-cli --accept-tos mode warp > /dev/null 2>&1 || true
    warp-cli --accept-tos connect > /dev/null 2>&1 || true
else
    echo "IP rotation is disabled. Skipping Cloudflare WARP initialization."
fi

echo "Starting Crustoxy..."
exec crustoxy