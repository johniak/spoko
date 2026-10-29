# `.spokoconfig` reference

TOML file in the project root. It is synchronized like any other file, so all machines share
it. Unknown keys are an error (`spoko config check` reports it).

```toml
[ignore]
inherit_gitignore = true          # use .gitignore files + .git/info/exclude (default true)
include = [".env", ".env.*"]      # sync even if ignored (gitignore syntax)
exclude = ["*.log", "coverage/"]  # never sync (wins over include)

[binary]
patterns = ["*.psd", "media/**"]  # treat as binary: never text-merged, newer wins
size_threshold = "10MB"           # bigger files are binary too (B, kB, MB, GB, KiB, MiB, GiB)

[[watch]]
files = ["package-lock.json", "pnpm-lock.yaml"]
run = "npm ci"                    # default command
run_macos = "npm ci"              # optional per-OS overrides
run_linux = "npm ci"
run_windows = "npm ci"
mode = "ask"                      # ask: queue + notify (spoko hooks run) | auto: run now | notify: only tell

[[docker.volume]]
name = "pgdata"                   # logical (compose) name; `<compose project>_pgdata` is found by label
mode = "raw"                      # raw: copy files (container stopped) | dump: run dump/restore inside it
stop_container = true             # raw: stop containers using the volume while copying
dump = "pg_dump -U postgres -Fc postgres"
restore = "pg_restore -U postgres -d postgres --clean --if-exists"
on_mismatch = "warn"              # warn: ask before restoring across arch/image/major | dump: use the dump instead

[external.claude_code]
enabled = true                    # sync this project's Claude Code chats (paths rewritten per machine)

[[external.custom]]
name = "notes"
path = "{home}/notes/app"         # a directory outside the project
rewrite_paths = false             # rewrite the project root inside .jsonl files
```

Always ignored: `.git/`, `.DS_Store`, `Thumbs.db`, editor swap/backup files, spoko's own
`.spoko-tmp-*` files.

Machine-local git settings (`core.filemode`, `core.autocrlf`, credentials, gpg program, …)
are never copied between machines; the rest of `.git/config` (remotes, branch tracking,
aliases) is.
