#!/usr/bin/env bash
# Measures Kerno against the performance targets of CLAUDE.md §5 with the
# app's bench mode (crates/kerno/src/bench.rs) and prints one table.
#
#   scripts/bench.sh [runs]        (default 5 runs of each measure)
#
# Needs the kerno-postgres and kerno-mysql containers (scripts/test-env.sh up)
# and an idle machine: it refuses to measure while the 1-minute load average
# is above 4. Builds the release binary through nice; the measured runs are
# not niced. Every run gets a fresh copy of a scratch data directory seeded
# with one connection per server and uses the in-memory secret store: the
# user's own Kerno data and keychain are never touched. The servers'
# password is read from docker-compose.yml at run time.
set -euo pipefail
cd "$(dirname "$0")/.."

runs="${1:-5}"
max_load=4
bin="$PWD/target/release/kerno"

if pgrep -x kerno >/dev/null; then
  echo "another Kerno is running; quit it first" >&2
  exit 1
fi

load1() { sysctl -n vm.loadavg 2>/dev/null | awk '{print $2}' || awk '{print $1}' /proc/loadavg; }

# Waits up to 10 minutes for the machine to calm down, else gives up.
wait_for_quiet() {
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
cleanup() {
  pkill -f "^$bin" 2>/dev/null || true
  rm -rf "$scratch"
}
trap cleanup EXIT

# Runs the app with the given environment; prints "label<TAB>ms<TAB>line"
# for each line on its stdout, ms counted from the spawn. Killed after 4
# minutes if it has not quit by itself.
run_app() {
  local label="$1"
  shift
  env -u KERNO_LOG KERNO_SECRET_STORE=memory KERNO_APPEARANCE=light "$@" \
    perl -MTime::HiRes=time -e '
      my ($label, $bin) = (shift, shift);
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
run_app seed KERNO_DATA_DIR="$template" KERNO_BENCH=seed KERNO_BENCH_CONNECTION=postgres \
  KERNO_BENCH_URL="postgres://kerno@127.0.0.1:55432/kerno?sslmode=disable" >/dev/null
run_app seed KERNO_DATA_DIR="$template" KERNO_BENCH=seed KERNO_BENCH_CONNECTION=mysql \
  KERNO_BENCH_URL="mysql://kerno@127.0.0.1:53306/kerno?ssl-mode=disabled" >/dev/null

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
  local label="$1" mode="$2" connection="$3" sql="$4" password="$5"
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

measure startup startup "" "" ""
measure postgres first_rows postgres "SELECT * FROM analytics.big_table" "$pg_password"
measure mysql first_rows mysql "SELECT * FROM kerno_analytics.big_table" "$my_password"
measure scroll scroll postgres "SELECT * FROM analytics.big_table" "$pg_password"

echo
echo "$(sysctl -n machdep.cpu.brand_string 2>/dev/null || uname -m), $(date '+%Y-%m-%d %H:%M'), commit $(git rev-parse --short HEAD), load $(load1), $runs runs, median (min-max)"
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
    name = data["bench"] + (f".{data['pass']}" if "pass" in data else "")
    if name == "startup":
        values[(label, name, "spawn_to_interactive_ms")].append(float(spawn_ms))
    for key, value in data.items():
        if isinstance(value, dict):
            for sub, number in value.items():
                values[(label, name, f"{key}.{sub}")].append(number)
        elif isinstance(value, (int, float)) and not isinstance(value, bool):
            values[(label, name, key)].append(value)

targets = {
    ("startup", "spawn_to_interactive_ms"): 400,
    ("startup", "main_to_interactive_ms"): 400,
    ("first_rows", "idle_rss_mb"): 150,
    ("first_rows", "columns_to_frame_ms"): 150,
    ("scroll.vertical", "interval_ms.avg"): 16,
    ("scroll.horizontal", "interval_ms.avg"): 16,
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
