#!/usr/bin/env bash
# Manage the kerno-* test containers.
#   scripts/test-env.sh up      generate test certificates/keys, start, wait until seeded
#   scripts/test-env.sh down    stop and remove the containers (data volumes are kept)
#   scripts/test-env.sh reset   down, and delete the data volumes (forces a re-seed)
#   scripts/test-env.sh status  show the containers
set -euo pipefail
cd "$(dirname "$0")/.."

GEN=tests/fixtures/generated

generate() {
  mkdir -p "$GEN/tls" "$GEN/ssh"
  if [ ! -f "$GEN/tls/ca.crt" ]; then
    echo "Generating test TLS certificates in $GEN/tls"
    (
      cd "$GEN/tls"
      openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -subj "/CN=Kerno Test CA" \
        -keyout ca.key -out ca.crt 2>/dev/null
      cat > server.cnf <<CNF
[req]
distinguished_name = dn
req_extensions = ext
prompt = no
[dn]
CN = localhost
[ext]
subjectAltName = DNS:localhost,DNS:kerno-postgres,DNS:postgres,IP:127.0.0.1
CNF
      openssl req -newkey rsa:2048 -nodes -keyout server.key -out server.csr -config server.cnf 2>/dev/null
      openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial -days 3650 \
        -extfile server.cnf -extensions ext -out server.crt 2>/dev/null
      # Client certificate for the kerno_cert role (certificate authentication).
      openssl req -newkey rsa:2048 -nodes -keyout client.key -out client.csr -subj "/CN=kerno_cert" 2>/dev/null
      openssl x509 -req -in client.csr -CA ca.crt -CAkey ca.key -CAcreateserial -days 3650 \
        -out client.crt 2>/dev/null
      # A second CA that signed nothing: verify-ca against it must fail.
      openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -subj "/CN=Kerno Wrong CA" \
        -keyout wrong-ca.key -out wrong-ca.crt 2>/dev/null
      rm -f ./*.csr ./*.srl server.cnf
      chmod 644 ./*.crt && chmod 600 ./*.key && chmod 644 server.key
    )
  fi
  if [ ! -f "$GEN/ssh/id_ed25519" ]; then
    echo "Generating test SSH keys in $GEN/ssh"
    ssh-keygen -q -t ed25519 -N "" -C kerno-test -f "$GEN/ssh/id_ed25519"
    ssh-keygen -q -t ed25519 -N "kerno_key_passphrase" -C kerno-test-encrypted -f "$GEN/ssh/id_ed25519_enc"
    ssh-keygen -q -t rsa -b 3072 -N "" -C kerno-test-rsa -f "$GEN/ssh/id_rsa"
    cat "$GEN/ssh/id_ed25519.pub" "$GEN/ssh/id_ed25519_enc.pub" "$GEN/ssh/id_rsa.pub" > "$GEN/ssh/authorized_keys"
  fi
}

case "${1:-}" in
  up)
    generate
    nice -n 10 docker compose up -d --build --wait --wait-timeout 900
    docker compose ps
    ;;
  down)
    docker compose down
    ;;
  reset)
    docker compose down -v
    ;;
  status)
    docker compose ps
    ;;
  *)
    sed -n '2,6p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac
