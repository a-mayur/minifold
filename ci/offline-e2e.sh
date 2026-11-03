#!/usr/bin/env bash
# Runs the image on a Docker network with no route to the outside and compresses one file of
# each kind. Proves the image starts (every tool installed), works, and needs no network.
# Usage: ci/offline-e2e.sh <image>
set -euo pipefail
IMAGE=$1
NET=minifold-e2e-net
WORK=$(mktemp -d)
trap 'docker rm -f minifold-e2e >/dev/null 2>&1 || true; docker network rm "$NET" >/dev/null 2>&1 || true; rm -rf "$WORK"' EXIT
mkdir "$WORK/samples"
chmod 777 "$WORK/samples"

# Sample files, made with the image's own tools and no network.
docker run --rm --network none -v "$WORK/samples:/w" --entrypoint sh "$IMAGE" -c '
    set -e; cd /w
    seq 1 50000 > logs.txt
    vips gaussnoise n.v 800 600 && vips copy n.v "photo.jpg[Q=95]" && vips copy n.v photo.png
    vips copy n.v "photo.heic[Q=90]" && vips copy n.v "photo.avif[Q=90]" && vips copy n.v photo.webp && vips copy n.v photo.tif
    rm n.v
    ffmpeg -loglevel error -f lavfi -i testsrc2=size=160x120:rate=5 -t 1 anim.gif
    ffmpeg -loglevel error -f lavfi -i testsrc2=size=640x360:rate=25 -f lavfi -i sine -t 2 -c:v libx264 -crf 10 -c:a aac clip.mp4
    gs -q -sDEVICE=pdfwrite -o doc.pdf -c "/Helvetica findfont 24 scalefont setfont 72 720 moveto (Minifold) show showpage"
'

docker network create --internal "$NET" >/dev/null
docker run -d --name minifold-e2e --network "$NET" \
    -e MINIFOLD_AUTH_TOKEN=e2e-user-token-0123456789 -e MINIFOLD_ADMIN_TOKEN=e2e-admin-token-0123456789 \
    "$IMAGE" >/dev/null

# The container must have no way out.
# (bash, not sh: Debian's sh is dash, which has no /dev/tcp and would always "fail" to connect)
if docker exec minifold-e2e timeout 10 bash -c 'exec 3<>/dev/tcp/1.1.1.1/443' 2>/dev/null; then
    echo "FAIL: the test network allows outbound connections"; exit 1
fi

status=0
docker run --rm --network "$NET" -v "$WORK/samples:/samples:ro" -v "$PWD/ci/e2e-client.sh:/e2e.sh:ro" \
    -e MINIFOLD_URL=http://minifold-e2e:8080 -e TOKEN=e2e-user-token-0123456789 -e SAMPLES=/samples \
    curlimages/curl:8.11.1 sh /e2e.sh || status=$?
echo "--- server log"
docker logs minifold-e2e 2>&1 | tail -n 40
docker run --rm --entrypoint test "$IMAGE" -s /usr/share/doc/minifold/THIRD_PARTY_LICENSES.md \
    || { echo "FAIL: THIRD_PARTY_LICENSES.md missing from the image"; status=1; }
exit "$status"
