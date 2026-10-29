# Self-hosting the server

The server is one binary (`spoko-server`) with a data directory (SQLite + blob files).

```sh
# --public-url: the URL clients log in with (login signatures are bound to it)
spoko-server serve --data /var/lib/spoko --listen 0.0.0.0:7420 --public-url https://spoko.example.com
# the account is the user's SSH public key (a key line or a path to the .pub file)
spoko-server account create jan --ssh-key ~/.ssh/id_ed25519.pub --data /var/lib/spoko
# the user lost the key: set the new one (ends every session); they log in with
# `spoko login --key <new> --recovery-key …`
spoko-server account set-key jan --ssh-key "ssh-ed25519 AAAA… new" --data /var/lib/spoko
```

Every machine with the private key logs in on its own (`spoko login <url>`), no tokens to
hand out.

Docker: `crates/spoko-server/Dockerfile` (data in the `/data` volume, port 7420).

Put TLS in front of it; clients use plain `https://` URLs (WebSockets on the same port, path
`/v1/events`). Without `--public-url` the server trusts the `Host` header, so the proxy must
pass it on (Caddy does; nginx needs `proxy_set_header Host $host;`). Example Caddyfile:

```
spoko.example.com {
    reverse_proxy 127.0.0.1:7420
}
```

Backups: stop the server (or use SQLite's online backup) and copy the data directory. All
content is end-to-end encrypted, so backups are safe to store anywhere.

Accounts are isolated from each other; multi-tenant hosting with sign-up, quotas and S3
storage is planned for stage 2.
