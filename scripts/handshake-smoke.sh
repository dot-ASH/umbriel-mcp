#!/bin/sh
set -eu

binary="${1:?usage: handshake-smoke.sh <path-to-umbriel-mcp>}"

requests='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"handshake-smoke","version":"1"}}}
{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}'

replies="$( (printf '%s\n' "$requests"; sleep 1) | timeout 10 "$binary" 2>/dev/null)"

printf '%s\n' "$replies" | grep -q '"serverInfo"' || { echo "initialize got no serverInfo" >&2; exit 1; }
printf '%s\n' "$replies" | grep -q '"launch_app"' || { echo "tools/list did not advertise launch_app" >&2; exit 1; }
echo "handshake ok: $binary"
