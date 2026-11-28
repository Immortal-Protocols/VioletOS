#!/bin/sh
# Rose disables node_repl. Codex still requires a transport for the disabled
# entry when the companion's Codex home has no node_repl configuration.
if [ "$1" = app-server ]; then
    shift
    exec "$VIOLET_CODEX_BIN" app-server -c 'mcp_servers.node_repl.command="/usr/bin/false"' "$@"
fi
exec "$VIOLET_CODEX_BIN" "$@"
