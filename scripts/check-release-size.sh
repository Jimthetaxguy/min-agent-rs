#!/bin/sh
# Fail when the release binary exceeds the size budget.
# The limit is the measured stripped release size plus about 10% headroom
# (2,192,360 bytes * 1.10 = 2,411,596).
set -eu

cd "$(dirname "$0")/.."

limit=2411596
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
