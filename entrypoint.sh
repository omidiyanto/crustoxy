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

    # Wait for WARP to be fully connected + healthy (max 30s)
    echo "Waiting for WARP connection..."
    for i in $(seq 1 30); do
        WARP_OUTPUT=$(warp-cli --accept-tos status 2>/dev/null || echo "")
        STATUS=$(echo "$WARP_OUTPUT" | grep "^Status" | head -1 || echo "")
        NETWORK=$(echo "$WARP_OUTPUT" | grep "^Network" | head -1 || echo "")
        if echo "$STATUS" | grep -q "Connected" && echo "$NETWORK" | grep -q "healthy"; then
            echo "WARP connected + healthy after ${i}s"
            break
        fi
        if [ "$i" -eq 30 ]; then
            echo "WARNING: WARP not ready after 30s, starting anyway..."
        fi
        sleep 1
    done
else
    echo "IP rotation is disabled. Skipping Cloudflare WARP initialization."
fi

echo "Starting Crustoxy..."
exec crustoxy