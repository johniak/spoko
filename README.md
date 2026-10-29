# spoko

**Your uncommitted work, on every machine you use — instantly.**

spoko syncs a developer's *work in progress* between their own computers: files you haven't
committed, the git state around them (branch, commits, staged changes, stash), `.env`,
Claude Code chats and Docker database volumes. You stop on the laptop, sit down at the
desktop, and everything is exactly where you left it — including `git status`.

It is **not** git and does not touch your history: your repository stays yours, spoko only
mirrors it. It also works in directories that are not git repositories at all.

- **Instant and transparent** — every save is pushed within ~0.3 s; the other machine
  applies it live (measured end to end, see [`docs/notes/perf.md`](docs/notes/perf.md)).
- **Git-grade merging** when machines diverge (you worked offline on both): three-way merge
  with git's own merge machinery, per file. Real conflicts keep your active version and
  record the other one (`spoko resolve` opens your `git mergetool`). Binaries / LFS are
  never merged: the newer version wins and the older stays in history.
- **Git state mirroring** — commit on `johniak`, switch to `dzban`: the other machine is on
  `dzban` with that commit, and its `git status` is clean. Nothing of spoko's is written into
  your `.git`.
- **The right files** — `.gitignore` is inherited, `.spokoconfig` adds includes (`.env`) and
  excludes. `node_modules/` is not synced, but a changed `package-lock.json` runs (or
  offers) `npm ci` on the other machine.
- **Dev environment** — Docker volumes (e.g. Postgres data) follow you on device switch, raw
  or via `pg_dump`/`pg_restore`, with architecture / image / major-version checks.
- **Claude Code chats** of the project follow it, with paths rewritten to each machine.
- **End-to-end encrypted** (XChaCha20-Poly1305, keyed BLAKE3 ids): the server stores opaque
  blobs and never sees names, contents or the key. No password at boot — the key lives in a
  0600 file, like your SSH key.
- **Fast** — zstd + content-defined chunking (a 1 MB edit to a 20 GB volume uploads ~1 MB),
  parallel encrypted transfer, a stat cache so rescans of 100k files take a fraction of a
  second.
- macOS, Linux and Windows.

> Status: **MVP**. Design: [`docs/superpowers/specs/2026-10-28-spoko-design.md`](docs/superpowers/specs/2026-10-28-spoko-design.md) (Polish),
> plan: [`docs/superpowers/plans/2026-10-28-spoko-mvp.md`](docs/superpowers/plans/2026-10-28-spoko-mvp.md).
> The CLI speaks Polish for now.

## Quick start

```sh
# build (Rust 1.95+)
cargo install --path crates/spoko          # the `spoko` CLI + daemon
cargo install --path crates/spoko-tray     # the menu-bar / tray icon (started by the daemon)
cargo install --path crates/spoko-server   # only if you host the server yourself

# 1. a server (see "Self-hosting" below) with an account for your SSH public key, then on
#    every machine that has the key (~/.ssh/id_ed25519, id_ecdsa or id_rsa, or --key <file>):
spoko login https://spoko.example.com
#   → the first machine also prints a recovery key: store it somewhere safe
#   → a key with a passphrase asks for it; the daemon then waits for `spoko unlock`

# 2. a project
cd ~/code/app && spoko init                      # prints the project id
spoko clone <project id> ~/src/app               # on the other machine (empty or existing clone)

# 3. run the daemon (and start it at login)
spoko daemon start
spoko daemon install
```

Then just work. While the daemon runs, a dot in the menu bar (tray on Windows/Linux) shows
that spoko is on: green synced, blue syncing, grey paused or locked, orange offline, red
conflict. Its menu says "spoko działa · <device> · N projects" and has the actions (sync,
pause, resolve, unlock, start the daemon). `spoko status` shows the same in the terminal.
`SPOKO_NO_TRAY=1` keeps the icon off (headless machines).

## Commands

| | |
|---|---|
| `spoko status [--json]` | projects, state, pending hooks, conflicts |
| `spoko sync` | one-off sync (current project, or all) — works without the daemon |
| `spoko resolve [file] [--done]` | list conflicts / resolve one with your merge tool or markers |
| `spoko log [file]`, `spoko restore <file> --at <commit\|10m\|2h\|unix time>` | local history |
| `spoko restore <file> --from-join` | what this machine had before it joined the project |
| `spoko pause`, `spoko resume [--confirm]` | stop/continue; `--confirm` after a mass-delete pause |
| `spoko volume push\|pull [name] [--fresh] [--force]` | Docker volumes |
| `spoko hooks run\|dismiss` | pending watch hooks (`npm ci`, …) |
| `spoko config check` | what is synced, ignored, binary, watched; largest files |
| `spoko login <server> [--key <file>] [--recovery-key …]` | log this machine in with the account's SSH key |
| `spoko unlock` | give the daemon the SSH key's passphrase (once per session) |
| `spoko key rotate --new <file>` | move the account to a new SSH key (the old one stops working) |
| `spoko device list\|remove` | devices of the account; `remove` ends that machine's session |
| `spoko daemon [run\|start\|stop\|install\|uninstall]` | the background process |

## `.spokoconfig`

Optional, in the project root (it is synced too). Everything has a default; full reference
with all keys: [`docs/configuration.md`](docs/configuration.md).

```toml
[ignore]
include = [".env", ".env.*"]      # sync these even though .gitignore ignores them
exclude = ["*.log", "tmp/"]

[[watch]]
files = ["package-lock.json"]
run = "npm ci"
mode = "ask"                      # ask | auto | notify

[[docker.volume]]
name = "pgdata"                   # compose volume name (project prefix resolved per machine)
mode = "raw"                      # raw | dump
```

## Self-hosting the server

```sh
docker build -f crates/spoko-server/Dockerfile -t spoko-server .
docker run -d -p 7420:7420 -v spoko-data:/data spoko-server
docker exec <container> spoko-server account create jan --data /data \
  --ssh-key "$(cat ~/.ssh/id_ed25519.pub)"                             # the account is this key
```

Put it behind TLS (Caddy, nginx, …). The server is a dumb store of encrypted blobs plus
compare-and-swap pointers and a WebSocket fan-out; it cannot read anything. Details:
[`docs/self-hosting.md`](docs/self-hosting.md).

## Security model

Everything hangs on your SSH key pair: the same key on all your machines logs them in (a
signed challenge) and unlocks the account's data key, which the server keeps only wrapped
with that key. No spoko secret is stored on disk; a key without a passphrase starts the
daemon without asking, a key with one asks once per session (`spoko unlock`). The server,
its operator and the network see only encrypted blobs, opaque ids, sizes and timing. Lose
the key → the server admin sets a new one and the recovery key brings the data back. Leaked
key → `spoko key rotate`. More: [`docs/security.md`](docs/security.md).

## MVP limitations

- git worktrees (`.git` file) and SHA-256 repositories: files sync, git state does not;
  submodules sync as their parent's gitlink only.
- History is kept forever (no retention/GC yet); the hosted multi-account service, S3
  storage and retention are stage 2.
- Docker volumes need a container that uses them (for the image); raw copies across CPU
  architectures are warned about (see [`docs/notes/volume-portability.md`](docs/notes/volume-portability.md)).

## Development

```sh
cargo test --workspace --exclude spoko-tray          # unit + integration + E2E
SPOKO_DOCKER_TESTS=1 cargo test -p spoko-docker      # with a Docker daemon
cargo test --release -p spoko --test perf -- --ignored --nocapture
./scripts/coverage.sh                                # line coverage gate
```

Crates: `spoko-crypto` (keys, AEAD, ids), `spoko-proto` (wire types), `spoko-server`,
`spoko-core` (scan, store, merge, gitstate, checkout — no network), `spoko-client` (transfer,
engine, daemon), `spoko-docker`, `spoko` (CLI), `spoko-tray`.

Built with [Claude Code](https://claude.com/claude-code). License: to be decided by the author.
