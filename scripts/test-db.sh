#!/usr/bin/env bash
# Starts (or stops, with "down") the throwaway servers used by the live tests:
#   Postgres 17 on :55432, MySQL 8.4 on :53306 and an SSH bastion on :52222
#   that can reach Postgres as huginn-pg:5432.
set -euo pipefail

if [[ "${1:-up}" == "down" ]]; then
  docker rm -f huginn-pg huginn-mysql huginn-ssh >/dev/null 2>&1 || true
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

echo "Run: (cd src-tauri && cargo test -- --ignored --test-threads=1)"
