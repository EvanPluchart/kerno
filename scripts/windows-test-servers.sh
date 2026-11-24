#!/usr/bin/env bash
# The test servers of docker-compose.yml on a Windows machine without Docker (GitHub's windows-2025
# runner, Git Bash), from the installs its image carries: a PostgreSQL 17 (EDB's installer) and a
# MySQL Server 8.4 (the MSI, never configured there). Each gets a data folder of its own, the
# containers' port, role, password, settings, TLS certificates and seed (tests/fixtures), as
# kerno-postgres and kerno-mysql are made (K68, docs/DECISIONS.md D-137).
#   scripts/windows-test-servers.sh up     initialise, start and seed both; fails if one does not
#   scripts/windows-test-servers.sh down   stop both (the data folders are left)
# The folders go under $KERNO_SERVERS_DIR, else $RUNNER_TEMP/kerno-servers. MariaDB, the SSH
# servers and the legacy MySQL servers have no counterpart here: their tests skip themselves.
# Also creates kerno_test_k67 (one table, k67_items) on both servers, for K67's runner tests.
set -euo pipefail
cd "$(dirname "$0")/.."

# Arguments such as /CN=... or a SQL text go to Windows programs as written.
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'

root=$(cygpath -m "${KERNO_SERVERS_DIR:-${RUNNER_TEMP:-$TEMP}/kerno-servers}")
password=kerno_test_pw
pgbin=$(cygpath -m "${PGBIN:-C:\\Program Files\\PostgreSQL\\17\\bin}")
pgdata=$root/postgres
mysql_service=kerno-mysql
# The newest "MySQL Server X.Y" folder of Program Files.
mysqldir() {
  local dirs
  dirs=$(ls -d "$(cygpath -u -F 38)/MySQL/MySQL Server "* | sort -V)
  [ -n "$dirs" ] || { echo "no MySQL Server in Program Files" >&2; exit 1; }
  cygpath -m "$(tail -n 1 <<<"$dirs")"
}

# The runner sets PG* for the whole machine: the values below win.
psql_() {
  PGHOST=127.0.0.1 PGPORT=55432 PGUSER=kerno PGPASSWORD=$password \
    "$pgbin/psql.exe" -X -q -v ON_ERROR_STOP=1 "$@"
}

postgres_up() {
  printf '%s\n' "$password" > "$root/postgres-password"
  # The container's cluster: the bootstrap superuser is kerno, UTF-8, an English locale.
  "$pgbin/initdb.exe" -D "$pgdata" -U kerno --pwfile="$root/postgres-password" \
    --auth-host=scram-sha-256 --auth-local=scram-sha-256 --encoding=UTF8 --locale=en-US >/dev/null
  rm "$root/postgres-password"
  # tests/fixtures/postgres/00_tls.sh, with this machine's paths.
  cp tests/fixtures/generated/tls/{server.crt,server.key,ca.crt} "$pgdata/"
  cat >> "$pgdata/postgresql.conf" <<CONF
listen_addresses = '127.0.0.1'
port = 55432
ssl = on
ssl_cert_file = 'server.crt'
ssl_key_file = 'server.key'
ssl_ca_file = 'ca.crt'
CONF
  { echo "hostssl all kerno_cert all cert"; cat "$pgdata/pg_hba.conf"; } > "$pgdata/pg_hba.conf.new"
  mv "$pgdata/pg_hba.conf.new" "$pgdata/pg_hba.conf"
  "$pgbin/pg_ctl.exe" -D "$pgdata" -l "$root/postgres.log" -w -t 120 start
  psql_ -d postgres -c "CREATE DATABASE kerno"
  for file in tests/fixtures/postgres/*.sql; do
    echo "PostgreSQL: $file"
    psql_ -d kerno -f "$file"
  done
  psql_ -d postgres -c "CREATE DATABASE kerno_test_k67"
  psql_ -d kerno_test_k67 \
    -c "CREATE TABLE k67_items (id integer PRIMARY KEY, label text NOT NULL)" \
    -c "INSERT INTO k67_items VALUES (1, 'one'), (2, 'two'), (3, 'three')"
  psql_ -d kerno -Atc "SELECT version()"
}

mysql_up() {
  local dir tls data ini
  dir=$(mysqldir)
  tls=$(cygpath -m "$PWD/tests/fixtures/generated/tls")
  data=$root/mysql
  ini=$root/mysql.ini
  # docker-compose.yml's command line and the image's own settings (no name lookups). Table
  # names keep their case as on the container's Linux; Windows' default (1) lowers them.
  cat > "$ini" <<INI
[mysqld]
basedir=$dir
datadir=$data
port=53306
bind-address=127.0.0.1
character-set-server=utf8mb4
local-infile=1
max-connections=100
skip-name-resolve
lower_case_table_names=2
ssl-ca=$tls/ca.crt
ssl-cert=$tls/server.crt
ssl-key=$tls/server.key
log-error=$root/mysql.err
INI
  # Root from any host, as the image's MYSQL_ROOT_HOST: with skip-name-resolve, root@localhost
  # does not take a connection to 127.0.0.1.
  echo "CREATE USER 'root'@'%'; GRANT ALL ON *.* TO 'root'@'%' WITH GRANT OPTION;" > "$root/mysql-init.sql"
  "$dir/bin/mysqld.exe" --defaults-file="$ini" --initialize-insecure --init-file="$root/mysql-init.sql" --console
  # A service: what a step starts is stopped when the step ends, a service is not.
  "$dir/bin/mysqld.exe" --install "$mysql_service" --defaults-file="$(cygpath -w "$ini")"
  net start "$mysql_service" || { cat "$root/mysql.err" >&2; exit 1; }
  local mysql=("$dir/bin/mysql.exe" --host=127.0.0.1 --port=53306 --user=root)
  for _ in $(seq 60); do
    "$dir/bin/mysqladmin.exe" --host=127.0.0.1 --port=53306 --user=root ping >/dev/null 2>&1 && break
    sleep 2
  done
  # What the image's entrypoint does with MYSQL_USER, MYSQL_PASSWORD and MYSQL_DATABASE.
  "${mysql[@]}" -e "CREATE USER 'kerno'@'%' IDENTIFIED BY '$password';
    CREATE DATABASE kerno; GRANT ALL ON kerno.* TO 'kerno'@'%';"
  for file in tests/fixtures/mysql/*.sql; do
    echo "MySQL: $file"
    "${mysql[@]}" --comments --database=kerno < "$file"
  done
  "${mysql[@]}" -e "CREATE DATABASE kerno_test_k67;
    CREATE TABLE kerno_test_k67.k67_items (id INT PRIMARY KEY, label VARCHAR(40) NOT NULL);
    INSERT INTO kerno_test_k67.k67_items VALUES (1, 'one'), (2, 'two'), (3, 'three');
    ALTER USER 'root'@'localhost' IDENTIFIED BY '$password', 'root'@'%' IDENTIFIED BY '$password';
    SELECT VERSION();"
}

case "${1:-}" in
  up)
    mkdir -p "$root"
    scripts/test-env.sh generate
    postgres_up
    mysql_up
    ;;
  down)
    "$pgbin/pg_ctl.exe" -D "$pgdata" -m fast stop || true
    net stop "$mysql_service" || true
    "$(mysqldir)/bin/mysqld.exe" --remove "$mysql_service" || true
    ;;
  *)
    sed -n '7,8p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac
