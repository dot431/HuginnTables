#!/usr/bin/env bash
# Starts (or stops, with "down") the throwaway servers used by the live tests:
#   Postgres 17 :55432, MySQL 8.4 :53306, an SSH bastion :52222 (reaches
#   Postgres as huginn-pg:5432), Redis 7 :56379, MongoDB 8 :57017 and the
#   BigQuery emulator :59050 (project huginn-test, seeded from bigquery-seed.yaml).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
names=(huginn-pg huginn-mysql huginn-ssh huginn-redis huginn-mongo huginn-bigquery)

if [[ "${1:-up}" == "down" ]]; then
  docker rm -f "${names[@]}" >/dev/null 2>&1 || true
  docker network rm huginn-test >/dev/null 2>&1 || true
  exit 0
fi

docker network create huginn-test >/dev/null 2>&1 || true
docker run -d --rm --name huginn-pg --network huginn-test \
  -e POSTGRES_PASSWORD=huginn -p 55432:5432 postgres:17
docker run -d --rm --name huginn-mysql --network huginn-test \
  -e MYSQL_ROOT_PASSWORD=huginn -e MYSQL_DATABASE=shop -p 53306:3306 mysql:8.4
docker run -d --rm --name huginn-ssh --network huginn-test \
  -e PASSWORD_ACCESS=true -e USER_NAME=tunnel -e USER_PASSWORD=huginn \
  -e DOCKER_MODS=linuxserver/mods:openssh-server-ssh-tunnel \
  -p 52222:2222 lscr.io/linuxserver/openssh-server:latest
docker run -d --rm --name huginn-redis --network huginn-test \
  -p 56379:6379 redis:7 redis-server --requirepass huginn
docker run -d --rm --name huginn-mongo --network huginn-test \
  -e MONGO_INITDB_ROOT_USERNAME=root -e MONGO_INITDB_ROOT_PASSWORD=huginn -p 57017:27017 mongo:8
docker run -d --rm --name huginn-bigquery --network huginn-test --platform linux/amd64 \
  -v "$here/bigquery-seed.yaml:/seed.yaml:ro" -p 59050:9050 \
  ghcr.io/goccy/bigquery-emulator:latest --project=huginn-test --data-from-yaml=/seed.yaml

echo "Run: (cd src-tauri && cargo test -- --ignored --test-threads=1)"
