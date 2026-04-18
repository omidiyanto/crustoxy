#!/bin/bash

echo "Starting Cloudflare WARP daemon..."
warp-svc &
sleep 3
warp-cli --accept-tos registration new || true
echo "Starting Crustoxy..."
exec crustoxy