#!/bin/sh
# Put the Violet Agent relay on a Chariot agent as its app (chariot-agent-app/v1):
# Chariot serves it at https://agent-<id without dashes>.apps.chariots.sh.
#
#   tools/chariot-relay/deploy.sh AGENT     # an agent id, slug or name from `chariot list`
#
# Then point the phone at it once over USB, or build it in as the default:
#   build/moto-package "open-run=@sys set relay https://agent-<id>.apps.chariots.sh"
#   VIOLET_AGENT_RELAY=https://agent-<id>.apps.chariots.sh make moto
set -e
agent=${1:?usage: deploy.sh AGENT}
here=$(cd "$(dirname "$0")" && pwd)
dir=/agent-data/workspace/.chariot-app
chariot ssh "$agent" -- "mkdir -p $dir"
chariot ssh "$agent" -- "cat > $dir/relay.py" < "$here/relay.py"
printf '{"format":"chariot-agent-app/v1","port":8090,"health_path":"/health"}' | chariot ssh "$agent" -- "cat > $dir/app.json"
chariot ssh "$agent" -- "cat > $dir/start && chmod 700 $dir/start" <<'START'
#!/chariot/bin/sh
here=$(cd "$(/chariot/bin/dirname "$0")" && pwd)
exec /usr/bin/python3 "$here/relay.py" --host 127.0.0.1 --port "${PORT:-8090}"
START
id=$(chariot list | awk -v a="$agent" '$1 == a || $2 == a || $3 == a { print $1 }' | tr -d '-')
echo "Deployed. Health: https://agent-$id.apps.chariots.sh/health"
