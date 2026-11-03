#!/bin/sh
# Compresses every file in $SAMPLES through a running Minifold and checks each job finishes.
# Usage: MINIFOLD_URL=http://host:8080 TOKEN=... SAMPLES=/dir sh e2e-client.sh
set -eu
: "${MINIFOLD_URL:?}" "${TOKEN:?}" "${SAMPLES:?}"
AUTH="Authorization: Bearer $TOKEN"
failed=0

field() { sed -n "s/.*\"$1\":\"\([^\"]*\)\".*/\1/p"; }

i=0
until curl -sf "$MINIFOLD_URL/healthz" >/dev/null; do
    i=$((i + 1)); [ "$i" -gt 60 ] && { echo "Minifold did not start"; exit 1; }
    sleep 1
done

for file in "$SAMPLES"/*; do
    name=$(basename "$file")
    job=$(curl -sS -H "$AUTH" --data-binary "@$file" "$MINIFOLD_URL/api/jobs?name=$name&level=high")
    id=$(echo "$job" | field id)
    [ -n "$id" ] || { echo "FAIL $name: upload rejected: $job"; failed=1; continue; }
    state=queued
    while [ "$state" = queued ] || [ "$state" = running ]; do
        sleep 1
        job=$(curl -sS -H "$AUTH" "$MINIFOLD_URL/api/jobs/$id")
        state=$(echo "$job" | field state)
    done
    if [ "$state" != done ]; then
        echo "FAIL $name: $job"; failed=1; continue
    fi
    bytes=$(curl -sS -H "$AUTH" "$MINIFOLD_URL/api/jobs/$id/download" | wc -c)
    echo "ok   $name: $(wc -c < "$file") -> $bytes bytes ($(echo "$job" | field note))"
    [ "$bytes" -gt 0 ] || { echo "FAIL $name: empty download"; failed=1; }
done
exit "$failed"
