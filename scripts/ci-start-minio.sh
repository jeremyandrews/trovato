#!/usr/bin/env bash
#
# Start a throwaway MinIO for the S3 file storage tests (BG16), and tell the
# tests where it is.
#
#   ./scripts/ci-start-minio.sh            # in CI: writes to $GITHUB_ENV
#   eval "$(./scripts/ci-start-minio.sh)"  # locally: prints export lines
#
# The credentials are random, made here for this one container, and masked in
# the job log: no key, AWS or otherwise, is ever written into a workflow file.
# The bucket is a directory created before the server starts (MinIO serves a
# top-level directory of its data path as a bucket), so no client is needed.
#
# The image is pinned by digest. MinIO stopped publishing community images to
# Docker Hub and Quay in 2025; Chainguard builds this one from MinIO's source.
# If the digest is ever no longer served, pull `cgr.dev/chainguard/minio:latest`,
# take its digest from `docker image inspect`, and update it here.
set -euo pipefail

IMAGE="cgr.dev/chainguard/minio@sha256:b288eaa709ba2f799a39e9a8ceda71a08fabf90109b12c90badba9afff29485c"
NAME="${MINIO_CONTAINER_NAME:-trovato-minio}"
PORT="${MINIO_PORT:-9000}"
BUCKET="trovato-test"
DATA="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/trovato-minio-$$"

USER_NAME="minio$(openssl rand -hex 8)"
PASSWORD="$(openssl rand -hex 24)"

mkdir -p "$DATA/$BUCKET"
docker rm -f "$NAME" >/dev/null 2>&1 || true
docker run -d --name "$NAME" -p "127.0.0.1:$PORT:9000" -v "$DATA:/data" \
  -e MINIO_ROOT_USER="$USER_NAME" -e MINIO_ROOT_PASSWORD="$PASSWORD" \
  "$IMAGE" server /data >/dev/null

for _ in $(seq 1 60); do
  if curl -fsS "http://127.0.0.1:$PORT/minio/health/ready" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
curl -fsS "http://127.0.0.1:$PORT/minio/health/ready" >/dev/null || {
  echo "::error::MinIO did not become ready" >&2
  docker logs "$NAME" >&2 || true
  exit 1
}

lines=$(cat <<EOF
TROVATO_TEST_S3_ENDPOINT=http://127.0.0.1:$PORT
TROVATO_TEST_S3_BUCKET=$BUCKET
AWS_ACCESS_KEY_ID=$USER_NAME
AWS_SECRET_ACCESS_KEY=$PASSWORD
EOF
)

if [ -n "${GITHUB_ENV:-}" ]; then
  echo "::add-mask::$PASSWORD"
  printf '%s\n' "$lines" >> "$GITHUB_ENV"
  echo "MinIO is ready on 127.0.0.1:$PORT with bucket $BUCKET."
else
  printf '%s\n' "$lines" | sed 's/^/export /'
fi
