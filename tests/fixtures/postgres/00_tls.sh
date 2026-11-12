#!/bin/bash
# Turn TLS on with the generated test certificates and allow certificate
# authentication for the kerno_cert role. Runs once, at first start.
set -e
if [ -f /kerno-tls/server.crt ]; then
  cp /kerno-tls/server.crt /kerno-tls/server.key /kerno-tls/ca.crt "$PGDATA/"
  chmod 600 "$PGDATA/server.key"
  cat >> "$PGDATA/postgresql.conf" <<CONF
ssl = on
ssl_cert_file = 'server.crt'
ssl_key_file = 'server.key'
ssl_ca_file = 'ca.crt'
CONF
  { echo "hostssl all kerno_cert all cert"; cat "$PGDATA/pg_hba.conf"; } > "$PGDATA/pg_hba.conf.new"
  mv "$PGDATA/pg_hba.conf.new" "$PGDATA/pg_hba.conf"
fi
