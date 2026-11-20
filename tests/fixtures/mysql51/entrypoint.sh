#!/bin/sh
# First start: create the data directory and the `kerno` account, then run
# every *.sql of /docker-entrypoint-initdb.d in order, as the official images
# do. Then run the server in the foreground; extra arguments go to mysqld.
set -e
DATA=/var/lib/mysql
SOCK=/var/run/mysqld/mysqld.sock
if [ ! -d "$DATA/mysql" ]; then
  cd /opt/mysql
  scripts/mysql_install_db --no-defaults --basedir=/opt/mysql --datadir="$DATA" --user=mysql >/dev/null
  bin/mysqld --no-defaults --basedir=/opt/mysql --datadir="$DATA" --user=mysql \
    --skip-networking --socket="$SOCK" &
  pid=$!
  until bin/mysqladmin --socket="$SOCK" -uroot ping >/dev/null 2>&1; do sleep 1; done
  bin/mysql --socket="$SOCK" -uroot <<SQL
DELETE FROM mysql.user WHERE User = '';
UPDATE mysql.user SET Password = PASSWORD('kerno_test_pw') WHERE User = 'root';
CREATE DATABASE kerno;
CREATE USER 'kerno'@'%' IDENTIFIED BY 'kerno_test_pw';
GRANT ALL PRIVILEGES ON kerno.* TO 'kerno'@'%';
FLUSH PRIVILEGES;
SQL
  for f in /docker-entrypoint-initdb.d/*.sql; do
    [ -f "$f" ] || continue
    echo "running $f"
    bin/mysql --socket="$SOCK" -uroot -pkerno_test_pw kerno < "$f"
  done
  bin/mysqladmin --socket="$SOCK" -uroot -pkerno_test_pw shutdown
  wait "$pid" || true
fi
exec /opt/mysql/bin/mysqld --no-defaults --basedir=/opt/mysql --datadir="$DATA" --user=mysql \
  --socket="$SOCK" --port=3306 --bind-address=0.0.0.0 "$@"
