#!/bin/sh
# Fail when the release binary exceeds the size budget.
# The limit is fx.sh's size: 7,067,402 bytes (6.74 MiB).
set -eu

cd "$(dirname "$0")/.."

limit=7067402
cargo build --locked --release
binary=target/release/min-agent
size=$(wc -c < "$binary" | tr -d ' ')
triple=$(rustc -vV | awk '/^host:/{print $2}')

echo "target: ${triple}"
echo "rustc: $(rustc --version)"
echo "release binary: ${size} bytes (limit ${limit})"

if [ "$size" -gt "$limit" ]; then
    echo "release binary exceeds the ${limit}-byte size budget" >&2
    exit 1
fi
