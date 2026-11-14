#!/usr/bin/env bash
# Measures Kerno against the performance targets of CLAUDE.md §5 with the
# app's bench mode (crates/kerno/src/bench.rs) and prints one table.
#
#   scripts/bench.sh [runs] [startup-runs] [--only STEP] [--indicative]
#                                  (default 5 runs of each measure, 30 of the
#                                  startup: it is short and varies the most)
#
# STEP is one of: core (the targets of §5), and the cases competitors fail
# (CLAUDE.md §4 J9): wide (300 columns), navigator (200 schemas, 1,200
# tables), import (a 200 MB .sql file), reconnect (the network drops and
# comes back). Without --only, every step runs. --indicative measures on a
# busy machine instead of waiting for it to calm down, and says so on every
# line: never for published numbers.
#
# The J9 steps create their scratch objects (kerno_bench_* databases on
# kerno-postgres and kerno-mysql) and drop them, pass or fail; the reconnect
# step runs its own server, the kerno-bench-pg container on the
# kerno-bench-net network (host port 55440), and removes both. The shared
# kerno-* containers are never stopped or disconnected.
#
# Needs the kerno-postgres and kerno-mysql containers (scripts/test-env.sh up)
# and an idle machine: it refuses to measure while the 1-minute load average
# is above 4. Builds the release binary through nice; the measured runs are
# not niced. Every run gets a fresh copy of a scratch data directory seeded
# with one connection per server and uses the in-memory secret store: the
# user's own Kerno data and keychain are never touched. The servers'
# password is read from docker-compose.yml at run time.
#
# Keep Kerno's window on screen while it runs: macOS draws no frame for a
# hidden window (covered, minimized, on another Space, display asleep) and
# releases its drawables, so such a run is reported as an error, not
# measured. The bare binary is measured; it sets its own Dock icon, which
# costs it about 2 MB of idle memory more than Kerno.app.
set -euo pipefail
cd "$(dirname "$0")/.."

only=""
indicative=0
positional=()
while [ $# -gt 0 ]; do
  case "$1" in
    --only) only="${2:?--only needs a step}"; shift 2 ;;
    --indicative) indicative=1; shift ;;
    *) positional+=("$1"); shift ;;
  esac
done
case "$only" in
  "" | core | wide | navigator | import | reconnect) ;;
  *) echo "unknown step $only (core, wide, navigator, import, reconnect)" >&2; exit 2 ;;
esac
runs="${positional[0]:-5}"
startup_runs="${positional[1]:-30}"
max_load=4
want() { [ -z "$only" ] || [ "$only" = "$1" ]; }
bin="$PWD/target/release/kerno"

if pgrep -x kerno >/dev/null; then
  echo "another Kerno is running; quit it first" >&2
  exit 1
fi

load1() { sysctl -n vm.loadavg 2>/dev/null | awk '{print $2}' || awk '{print $1}' /proc/loadavg; }

# Waits up to 10 minutes for the machine to calm down, else gives up.
wait_for_quiet() {
  if [ "$indicative" = 1 ]; then
    echo "indicative, load $(load1)" >&2
    return
  fi
  local waited=0
  while awk -v load="$(load1)" -v max="$max_load" 'BEGIN { exit !(load > max) }'; do
    if [ "$waited" -ge 600 ]; then
      echo "load average $(load1) is above $max_load after 10 minutes: not measuring" >&2
      exit 3
    fi
    [ "$waited" -eq 0 ] && echo "load average $(load1) is above $max_load; waiting..." >&2
    sleep 15
    waited=$((waited + 15))
  done
}

password() {
  sed -n "s/^ *$1: *//p" docker-compose.yml | head -n 1
}
pg_password="$(password POSTGRES_PASSWORD)"
my_password="$(password MYSQL_PASSWORD)"
[ -n "$pg_password" ] && [ -n "$my_password" ] || {
  echo "no server password found in docker-compose.yml" >&2
  exit 1
}

nice -n 10 cargo build --release -p kerno

scratch="$(mktemp -d "${TMPDIR:-/tmp}/kerno-bench.XXXXXX")"
raw="$scratch/raw.tsv"
: >"$raw"

# SQL on the shared test servers, as the test user, through the containers'
# own clients; the password stays in the environment.
pg() { docker exec -i -e PGPASSWORD="$pg_password" kerno-postgres psql -h 127.0.0.1 -U kerno -v ON_ERROR_STOP=1 -q -X "$@"; }
my() { docker exec -i -e MYSQL_PWD="$my_password" kerno-mysql mysql -h 127.0.0.1 -ukerno "$@"; }

# Everything the J9 steps create, dropped whatever happened: after Kerno
# quit (it holds sessions on them).
cleanup() {
  pkill -f "^$bin" 2>/dev/null || true
  sleep 0.5
  for db in kerno_bench_wide kerno_bench_many kerno_bench_import; do
    pg -d kerno -c "DROP DATABASE IF EXISTS $db WITH (FORCE)" 2>/dev/null || true
    my -e "DROP DATABASE IF EXISTS $db" 2>/dev/null || true
  done
  docker rm -f kerno-bench-pg >/dev/null 2>&1 || true
  docker network rm kerno-bench-net >/dev/null 2>&1 || true
  rm -rf "$scratch"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# Runs the app with the given environment; prints "label<TAB>ms<TAB>line"
# for each line on its stdout, ms counted from the spawn. Killed after 4
# minutes if it has not quit by itself.
run_app() {
  local label="$1"
  shift
  env -u KERNO_LOG KERNO_SECRET_STORE=memory KERNO_APPEARANCE=light "$@" \
    perl -MTime::HiRes=time -e '
      my ($label, $bin) = (shift, shift);
      $| = 1;  # each line as it comes: the reconnect step waits for one
      my $t0 = time;
      my $pid = open(my $out, "-|") // die "fork: $!";
      if (!$pid) { exec $bin or die "exec $bin: $!" }
      $SIG{ALRM} = sub { kill "KILL", $pid; print "$label\t0\t{\"error\":\"killed after 240 s\"}\n"; exit 1 };
      alarm 240;
      while (my $line = <$out>) { printf "%s\t%.1f\t%s", $label, (time - $t0) * 1000, $line }
      waitpid $pid, 0;
      exit($? >> 8);
    ' "$label" "$bin"
}

template="$scratch/template"
mkdir -p "$template"
seed() {
  run_app seed KERNO_DATA_DIR="$template" KERNO_BENCH=seed KERNO_BENCH_CONNECTION="$1" \
    KERNO_BENCH_URL="$2" >/dev/null
}
seed postgres "postgres://kerno@127.0.0.1:55432/kerno?sslmode=disable"
seed mysql "mysql://kerno@127.0.0.1:53306/kerno?ssl-mode=disabled"
seed wide-pg "postgres://kerno@127.0.0.1:55432/kerno_bench_wide?sslmode=disable"
seed wide-my "mysql://kerno@127.0.0.1:53306/kerno_bench_wide?ssl-mode=disabled"
seed import-pg "postgres://kerno@127.0.0.1:55432/kerno_bench_import?sslmode=disable"
seed reconnect "postgres://kerno@127.0.0.1:55440/kerno?sslmode=disable"

# The first launch of a freshly built binary pays the system's one-time
# checks of a new executable (1.7 to 2.1 s seen): launch it once untimed.
run_app warmup KERNO_DATA_DIR="$(mktemp -d "$scratch/warmup.XXXXXX")" KERNO_BENCH=startup >/dev/null || true

fresh() {
  local dir="$scratch/run-$1"
  rm -rf "$dir"
  cp -R "$template" "$dir"
  echo "$dir"
}

measure() {
  local label="$1" mode="$2" connection="$3" sql="$4" password="$5" runs="${6:-$runs}"
  for i in $(seq 1 "$runs"); do
    wait_for_quiet
    echo "$label $i/$runs (load $(load1))" >&2
    printf '%s\t0\t{"bench":"load","load1":%s}\n' "$label" "$(load1)" >>"$raw"
    # In the environment only, never on a command line.
    KERNO_BENCH_PASSWORD="$password" run_app "$label" KERNO_DATA_DIR="$(fresh "$label-$i")" \
      KERNO_BENCH="$mode" KERNO_BENCH_CONNECTION="$connection" KERNO_BENCH_SQL="$sql" \
      >>"$raw" || echo "$label run $i failed" >&2
    if pgrep -f "^$bin" >/dev/null; then
      echo "Kerno did not quit after $label run $i" >&2
      exit 1
    fi
    sleep 1
  done
}

if want core; then
  measure startup startup "" "" "" "$startup_runs"
  measure postgres first_rows postgres "SELECT * FROM analytics.big_table" "$pg_password"
  measure mysql first_rows mysql "SELECT * FROM kerno_analytics.big_table" "$my_password"
  measure scroll scroll postgres "SELECT * FROM analytics.big_table" "$pg_password"
  # A result of one table is edited like table data and ordered by the
  # server; the sort done here is timed on a result that is not one table's.
  measure sort first_rows postgres \
    "SELECT * FROM analytics.big_table UNION ALL SELECT * FROM analytics.big_table WHERE false" "$pg_password"
fi

# Beekeeper #752: 300 columns x 20,000 rows, scrolled down and across.
if want wide; then
  python3 -I - >"$scratch/wide-pg.sql" <<'PY'
import sys
types = [("int", "g * {i}"), ("bigint", "g::bigint * 1000 + {i}"), ("numeric(10,2)", "g / 7.0"),
         ("text", "'v' || g || '-{i}'"), ("timestamp", "timestamp '2020-01-01' + g * interval '1 minute'"),
         ("boolean", "g % 2 = 0")]
cols = [(f"c{i:03}", *types[i % len(types)]) for i in range(1, 301)]
print("CREATE TABLE wide (id int PRIMARY KEY, " + ", ".join(f"{n} {t}" for n, t, _ in cols) + ");")
print("INSERT INTO wide SELECT g, " + ", ".join(e.format(i=i) for i, (_, _, e) in enumerate(cols, 1))
      + " FROM generate_series(1, 20000) g;")
PY
  python3 -I - <<'PY' >"$scratch/wide-my.sql"
types = [("int", "n * {i}"), ("bigint", "n * 1000 + {i}"), ("decimal(10,2)", "n / 7"),
         ("varchar(20)", "CONCAT('v', n, '-{i}')"), ("date", "DATE_ADD('2020-01-01', INTERVAL n % 3650 DAY)"),
         ("text", "REPEAT('t', 10 + n % 20)")]
cols = [(f"c{i:03}", *types[i % len(types)]) for i in range(1, 301)]
print("SET SESSION cte_max_recursion_depth = 30000;")
print("CREATE TABLE wide (id int PRIMARY KEY, " + ", ".join(f"{n} {t}" for n, t, _ in cols) + ");")
print("INSERT INTO wide WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 20000) "
      "SELECT n, " + ", ".join(e.format(i=i) for i, (_, _, e) in enumerate(cols, 1)) + " FROM s;")
PY
  pg -d kerno -c "DROP DATABASE IF EXISTS kerno_bench_wide WITH (FORCE)" -c "CREATE DATABASE kerno_bench_wide"
  pg -d kerno_bench_wide <"$scratch/wide-pg.sql"
  my -e "DROP DATABASE IF EXISTS kerno_bench_wide; CREATE DATABASE kerno_bench_wide"
  my kerno_bench_wide <"$scratch/wide-my.sql"
  measure wide-pg scroll wide-pg "SELECT * FROM wide" "$pg_password"
  measure wide-my scroll wide-my "SELECT * FROM wide" "$my_password"
  pg -d kerno -c "DROP DATABASE kerno_bench_wide WITH (FORCE)"
  my -e "DROP DATABASE kerno_bench_wide"
fi

# Beekeeper #135: 200 schemas, one of them with 1,200 tables (MySQL: a
# database of 1,200 tables), opened in the navigator. Created statement by
# statement, not in one transaction: the shared server's lock table stays
# small.
if want navigator; then
  {
    for s in $(seq -f %03g 1 200); do echo "CREATE SCHEMA s$s;"; done
    for t in $(seq -f %04g 1 1200); do
      echo "CREATE TABLE s001.t$t (id int PRIMARY KEY, name text, qty int, price numeric(10,2), at timestamp);"
    done
  } >"$scratch/many-pg.sql"
  for t in $(seq -f %04g 1 1200); do
    echo "CREATE TABLE t$t (id int PRIMARY KEY, name text, qty int, price decimal(10,2), at datetime);"
  done >"$scratch/many-my.sql"
  pg -d kerno -c "DROP DATABASE IF EXISTS kerno_bench_many WITH (FORCE)" -c "CREATE DATABASE kerno_bench_many"
  pg -d kerno_bench_many <"$scratch/many-pg.sql"
  my -e "DROP DATABASE IF EXISTS kerno_bench_many; CREATE DATABASE kerno_bench_many"
  my kerno_bench_many <"$scratch/many-my.sql"
  KERNO_BENCH_DATABASE=kerno_bench_many KERNO_BENCH_SCHEMA=s001 KERNO_BENCH_TABLE=t0001 \
    measure navigator-pg navigator postgres "" "$pg_password"
  KERNO_BENCH_DATABASE=kerno_bench_many KERNO_BENCH_TABLE=t0001 \
    measure navigator-my navigator mysql "" "$my_password"
  pg -d kerno -c "DROP DATABASE kerno_bench_many WITH (FORCE)"
  my -e "DROP DATABASE kerno_bench_many"
fi

# Beekeeper #702, #2325: a 200 MB .sql file of INSERTs run with Execute
# script, then again cancelled at 30 %.
if want import; then
  pg -d kerno -c "DROP DATABASE IF EXISTS kerno_bench_import WITH (FORCE)" -c "CREATE DATABASE kerno_bench_import"
  pg -d kerno_bench_import -c "CREATE TABLE items (id bigint, name text, qty int, price numeric(10,2), note text)"
  python3 -I - "$scratch/import.sql" <<'PY'
import sys
target = 200 * 1000 * 1000
with open(sys.argv[1], "w") as out:
    row, written = 0, 0
    while written < target:
        values = []
        for _ in range(1000):
            row += 1
            values.append(f"({row}, 'item {row}', {row % 97}, {row % 10000 / 100:.2f}, 'note for item {row}, imported')")
        statement = "INSERT INTO items VALUES " + ",\n".join(values) + ";\n"
        out.write(statement)
        written += len(statement)
PY
  KERNO_BENCH_DATABASE=kerno_bench_import KERNO_BENCH_FILE="$scratch/import.sql" \
    measure import import import-pg "" "$pg_password"
  KERNO_BENCH_DATABASE=kerno_bench_import KERNO_BENCH_FILE="$scratch/import.sql" KERNO_BENCH_CANCEL_AT=30 \
    measure import-cancel import import-pg "" "$pg_password"
  pg -d kerno -c "DROP DATABASE kerno_bench_import WITH (FORCE)"
fi

# DBeaver #6903: the server goes away and comes back; how long until a
# statement runs again. A server of the step's own, broken three ways:
# killed and started again (the connection is cut), frozen for 15 s
# (nothing answers, nothing closes), and its network detached for 10 s
# (informative: OrbStack keeps the connection through it).
if want reconnect; then
  bench_password="bench-$RANDOM-$RANDOM"
  docker rm -f kerno-bench-pg >/dev/null 2>&1 || true
  docker network rm kerno-bench-net >/dev/null 2>&1 || true
  docker network create kerno-bench-net >/dev/null
  docker run -d --name kerno-bench-pg --network kerno-bench-net -p 127.0.0.1:55440:5432 \
    -e POSTGRES_USER=kerno -e POSTGRES_DB=kerno -e POSTGRES_PASSWORD="$bench_password" \
    postgres:17 >/dev/null
  accepting() { docker exec kerno-bench-pg pg_isready -h 127.0.0.1 -U kerno >/dev/null 2>&1; }
  for _ in $(seq 1 60); do accepting && break; sleep 1; done
  epoch_ms() { perl -MTime::HiRes=time -e 'printf "%.0f", time * 1000'; }
  # Each breaks the server and mends it, then prints the instant it is
  # back: accepting connections again, or unfrozen, or reattached.
  kill_and_start() {
    docker kill kerno-bench-pg >/dev/null
    docker start kerno-bench-pg >/dev/null
    until accepting; do sleep 0.1; done
    epoch_ms
  }
  freeze_15s() {
    docker pause kerno-bench-pg >/dev/null
    sleep 15
    docker unpause kerno-bench-pg >/dev/null
    epoch_ms
  }
  detach_10s() {
    docker network disconnect kerno-bench-net kerno-bench-pg
    sleep 10
    docker network connect kerno-bench-net kerno-bench-pg
    epoch_ms
  }
  reconnect_case() {
    local label="$1" breaker="$2" i out app back recovered
    for i in $(seq 1 "$runs"); do
      wait_for_quiet
      echo "$label $i/$runs (load $(load1))" >&2
      printf '%s\t0\t{"bench":"load","load1":%s}\n' "$label" "$(load1)" >>"$raw"
      out="$scratch/$label-$i.tsv"
      : >"$out"
      KERNO_BENCH_PASSWORD="$bench_password" run_app "$label" KERNO_DATA_DIR="$(fresh "$label-$i")" \
        KERNO_BENCH=reconnect KERNO_BENCH_CONNECTION=reconnect KERNO_BENCH_SQL="SELECT 1" >"$out" &
      app=$!
      for _ in $(seq 1 120); do grep -q '"phase":"ready"' "$out" && break; sleep 0.5; done
      if ! grep -q '"phase":"ready"' "$out"; then
        echo "$label run $i: Kerno never ran its first statement" >&2
        kill "$app" 2>/dev/null || true
        wait "$app" 2>/dev/null || true
        cat "$out" >>"$raw"
        continue
      fi
      sleep 1
      back="$($breaker)"
      wait "$app" || echo "$label run $i failed" >&2
      cat "$out" >>"$raw"
      recovered="$(sed -n 's/.*"recovered_epoch_ms":\([0-9]*\).*/\1/p' "$out")"
      if [ -n "$recovered" ]; then
        printf '%s\t0\t{"bench":"reconnect","usable_after_back_ms":%s}\n' "$label" \
          "$((recovered - back))" >>"$raw"
      fi
      until accepting; do sleep 0.2; done
      sleep 1
    done
  }
  reconnect_case reconnect-kill kill_and_start
  reconnect_case reconnect-freeze freeze_15s
  reconnect_case reconnect-detach detach_10s
  docker rm -f kerno-bench-pg >/dev/null
  docker network rm kerno-bench-net >/dev/null
fi

echo
echo "$(sysctl -n machdep.cpu.brand_string 2>/dev/null || uname -m), $(date '+%Y-%m-%d %H:%M'), commit $(git rev-parse --short HEAD), load $(load1), $runs runs ($startup_runs of the startup), median (min-max)"
[ "$indicative" = 1 ] && echo "INDICATIVE: measured on a busy machine (load $(load1)), not for publication"
python3 -I - "$raw" <<'PY'
import json, statistics, sys
from collections import defaultdict

values = defaultdict(list)
for row in open(sys.argv[1]):
    label, spawn_ms, line = row.rstrip("\n").split("\t", 2)
    try:
        data = json.loads(line)
    except ValueError:
        continue
    if "error" in data:
        print(f"error in {label}: {data['error']}")
        continue
    for key in ("outcome", "statuses"):
        if key in data:
            print(f"{label} {data['bench']} {key}: {data[key]}")
    name = data["bench"] + (f".{data['pass']}" if "pass" in data else "")
    if name == "startup":
        values[(label, name, "spawn_to_interactive_ms")].append(float(spawn_ms))
    for key, value in data.items():
        if isinstance(value, dict):
            for sub, number in value.items():
                values[(label, name, f"{key}.{sub}")].append(number)
        elif isinstance(value, (int, float)) and not isinstance(value, bool) \
                and not key.endswith("epoch_ms"):
            values[(label, name, key)].append(value)

targets = {
    ("startup", "spawn_to_interactive_ms"): 400,
    ("startup", "main_to_interactive_ms"): 400,
    ("first_rows", "idle_rss_mb"): 150,
    ("first_rows", "columns_to_frame_ms"): 150,
    ("scroll.vertical", "interval_ms.avg"): 16,
    ("scroll.horizontal", "interval_ms.avg"): 16,
    ("navigator", "database_ms"): 300,
    ("navigator", "schema_ms"): 500,
    ("navigator", "tables_ms"): 500,
    ("import", "cancel_ms"): 2000,
    ("reconnect", "usable_after_back_ms"): 5000,
}
loads = defaultdict(list)
for row in open(sys.argv[1]):
    label, _, line = row.rstrip("\n").split("\t", 2)
    if '"bench":"load"' in line:
        loads[label].append(f"{json.loads(line)['load1']:g}")
for label, per_run in loads.items():
    print(f"load average per {label} run: {' '.join(per_run)}")
print(f"{'run':<9} {'measure':<18} {'value':<26} {'median':>12} {'min-max':>24} {'target':>8}")
for (label, name, key), numbers in values.items():
    numbers = [n for n in numbers if n is not None]
    if not numbers:
        continue
    target = targets.get((name, key))
    median = statistics.median(numbers)
    verdict = "" if target is None else (f"<= {target} " + ("met" if median <= target else "MISSED"))
    shown = lambda n: f"{n:.0f}" if float(n).is_integer() or abs(n) >= 1e5 else f"{n:g}"
    span = f"{shown(min(numbers))}-{shown(max(numbers))}"
    print(f"{label:<9} {name:<18} {key:<26} {shown(median):>12} {span:>24} {verdict:>8}")
PY
