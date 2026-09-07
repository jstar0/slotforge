#!/bin/sh
set -eu

project_root="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
relay_root="$project_root/relay"
temporary_root="${TMPDIR:-/tmp}"
runtime_dir="${SLOTFORGE_RUNTIME_DIR:-$(mktemp -d "$temporary_root/slotforge-acceptance.XXXXXX")}"
port="${SLOTFORGE_ACCEPTANCE_PORT:-5204}"
base_url="http://127.0.0.1:$port"
server_pid=""

cleanup() {
  if [ -n "$server_pid" ] && kill -0 "$server_pid" 2>/dev/null; then
    kill -TERM "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
    server_pid=""
  fi
}
trap cleanup EXIT INT TERM

mkdir -p "$runtime_dir"
if [ -e "$runtime_dir/slotforge.sqlite" ]; then
  echo "Refusing to reuse existing SQLite state: $runtime_dir/slotforge.sqlite" >&2
  echo "Choose a new SLOTFORGE_RUNTIME_DIR for a fresh acceptance run." >&2
  exit 2
fi

echo "Building SlotForge workbench and local services..."
pnpm --dir "$project_root/web" build
cargo build --manifest-path "$project_root/Cargo.toml" --bin slotforge
(cd "$relay_root" && go build -o "$runtime_dir/slotforge-relay" ./cmd/slotforge-relay)
(cd "$relay_root" && go build -o "$runtime_dir/slotforge-acceptance" ./cmd/slotforge-acceptance)

"$project_root/target/debug/slotforge" serve --host 127.0.0.1 --port "$port" --runtime-dir "$runtime_dir" &
server_pid=$!
attempt=0
until curl --fail --silent --output /dev/null "$base_url/api/v1/health"; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 100 ] || ! kill -0 "$server_pid" 2>/dev/null; then
    echo "SlotForge did not become ready at $base_url" >&2
    exit 1
  fi
  sleep 0.1
done

"$runtime_dir/slotforge-acceptance" --base-url "$base_url" --database "$runtime_dir/slotforge.sqlite" --relay "$runtime_dir/slotforge-relay"
echo "Acceptance evidence retained in $runtime_dir"
