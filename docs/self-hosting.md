# Self-hosting the server

The server is one binary (`spoko-server`) with a data directory (SQLite + blob files).

```sh
spoko-server serve --data /var/lib/spoko --listen 0.0.0.0:7420
spoko-server account create jan --data /var/lib/spoko   # bootstrap token for the first device
spoko-server account token jan --data /var/lib/spoko    # another token (adding a device without pairing / recovery)
```

Docker: `crates/spoko-server/Dockerfile` (data in the `/data` volume, port 7420).

Put TLS in front of it; clients use plain `https://` URLs (WebSockets on the same port, path
`/v1/events`). Example Caddyfile:

```
spoko.example.com {
    reverse_proxy 127.0.0.1:7420
}
```

Backups: stop the server (or use SQLite's online backup) and copy the data directory. All
content is end-to-end encrypted, so backups are safe to store anywhere.

Accounts are isolated from each other; multi-tenant hosting with sign-up, quotas and S3
storage is planned for stage 2.
