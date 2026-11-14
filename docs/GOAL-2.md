# /goal criteria, round 2: v1.1 (§4 J)

Build CLAUDE.md §4 J (items J1 to J19) end to end, autonomously, on top of the finished v1. The triage and the
evidence are in `docs/COMPETITOR-ISSUES.md` and `docs/research/`. Plan first (`docs/plans/M11.md`), then
implement in the order of §4 J, without stopping for approval; ask only under CLAUDE.md §8.

Machine (owner, D-052, while they are away): up to 4 sub-agents, all cores. Another agent works on the landing
page in the `kerno-site` worktree and may drive the Kerno app for captures: before you launch the app for UI
checks or captures, check that no other Kerno instance runs (`pgrep -x kerno`) and wait if one does. Never
touch the `kerno-site` branch or worktree.

The goal is reached only when ALL of these are true and verified by you:

1. Every item J1 to J19 works end to end against PostgreSQL 17, MySQL 8.4 and MariaDB 11 where it applies,
   with the tests §4 J asks for (integration tests ran with the `kerno-*` containers up, not skipped; UI tests
   for every new surface). Nothing stubbed or behind a flag that is off without the user's choice, except the
   opt-in AI assistant (J17) and vim mode (J19).
2. J2 is tested through a real jump host (a second SSH container, `kerno-ssh-jump`) and with an RSA key file; J3 is tested with a
   fake `aws` script on PATH, a `~/.pgpass` and a `~/.my.cnf` in a temporary HOME; J17 is tested against a
   local fake OpenAI-compatible server, and a test proves that nothing is sent while the assistant is off and
   that row data is never sent without the "include sample rows" tick.
3. `docs/BENCH.md` has the four new measurements of J9 with method and numbers, and the earlier targets are
   still met (re-measure cold start and idle memory on the final code).
4. `scripts/release.sh` produces the macOS bundle and the Linux x86_64 and arm64 tarballs, `.deb` and
   AppImage, with `SHA256SUMS`; each Linux package was started in a container of the oldest supported
   distribution (headless start or `--version`), and the glibc floor is written in the README.
5. `scripts/check.sh` is green: fmt, clippy `-D warnings`, `cargo deny check` with no ignored advisory, the
   whole test suite.
6. `docs/FEATURES.md` lists J1 to J19 with status, test and capture; light and dark captures of every new
   screen are in `docs/screenshots/final/` and you looked at each one; `docs/ROADMAP.md` and
   `docs/DECISIONS.md` no longer contradict §4 J; the "Not now" items of `docs/COMPETITOR-ISSUES.md` are in
   the roadmap with their reason.
7. README updated (new features, SSH through OpenSSH, password command, AI assistant and what it sends,
   Linux packages); `docs/PLAN.md`, `PROGRESS.md`, `DECISIONS.md` current; at least one commit per J group.
8. Commit locally only: no remote, no push, no tag, no release. At the end no Kerno instance, container,
   watcher or browser of yours is left running.
