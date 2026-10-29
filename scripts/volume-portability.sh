#!/usr/bin/env bash
# Spec §12: is a Postgres data directory created on arm64 usable on amd64 (same image + major)?
# Needs Docker with binfmt/qemu for the foreign platform. Prints RESULT: ok|fail.
set -euo pipefail
IMAGE=${IMAGE:-postgres:16}
VOL=spoko_port_$$
cleanup() { docker rm -f spoko-port-a spoko-port-b >/dev/null 2>&1 || true; docker volume rm -f "$VOL" >/dev/null 2>&1 || true; }
trap cleanup EXIT
# over TCP: the image's first-boot init server listens on the unix socket only, then restarts
wait_ready() { for _ in $(seq 1 180); do docker exec "$1" psql -h 127.0.0.1 -U postgres -tAc 'select 1' >/dev/null 2>&1 && return 0; sleep 1; done; return 1; }

docker run -d --platform linux/arm64 --name spoko-port-a -e POSTGRES_PASSWORD=x -e POSTGRES_HOST_AUTH_METHOD=trust -v "$VOL:/var/lib/postgresql/data" "$IMAGE" >/dev/null
wait_ready spoko-port-a
docker exec spoko-port-a psql -h 127.0.0.1 -U postgres -c "create table t(v text); insert into t values ('zażółć'); create index on t(v);" >/dev/null
docker stop spoko-port-a >/dev/null
docker run -d --platform linux/amd64 --name spoko-port-b -e POSTGRES_PASSWORD=x -e POSTGRES_HOST_AUTH_METHOD=trust -v "$VOL:/var/lib/postgresql/data" "$IMAGE" >/dev/null
if wait_ready spoko-port-b && [ "$(docker exec -e PGOPTIONS='-c enable_seqscan=off' spoko-port-b psql -h 127.0.0.1 -U postgres -tAc "select v from t where v='zażółć'")" = "zażółć" ]; then
  echo "RESULT: ok ($IMAGE arm64 → amd64)"
else
  docker logs spoko-port-b 2>&1 | tail -20
  echo "RESULT: fail ($IMAGE arm64 → amd64)"
fi
