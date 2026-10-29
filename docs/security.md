# Security model

**Threat model.** The server, its operator, its storage and the network are untrusted. Your
own unlocked devices are trusted: someone who has your laptop has your source code anyway.

**One SSH key per account.** The account *is* an SSH public key (Ed25519, ECDSA or RSA, in
OpenSSH format). The same private key is on every machine of the user; spoko reads it from
`~/.ssh/id_ed25519`, `id_ecdsa` or `id_rsa` (the first that exists) or from `--key`.
ssh-agent is not used: spoko needs the key's secret part to derive the wrapping key, which an
agent never gives out.

**What the server sees.** Opaque 32-byte object ids, encrypted blobs and their sizes, pointer
versions, the account's SSH public key, device names, and when devices are online. It never
sees file names, contents, branch names, commit messages or the data key.

**Logging in.** The server issues a random 32-byte challenge, single-use, valid for 120 s and
bound to the public key it was issued for. The client signs it (sshsig, namespace `spoko`, so
the signature is worthless to git or ssh) and gets a session token for this device. Unknown
keys get a challenge too and fail only at login, so the endpoint does not reveal which keys
have accounts.

**Cryptography** (no custom primitives):

- Data: XChaCha20-Poly1305 with a random 192-bit nonce per object, the object id as
  associated data (blobs cannot be swapped). Content is zstd-compressed before encryption.
- Ids: keyed BLAKE3 of the content id, so equal content is deduplicated without revealing
  git object ids.
- Keyring: random, versioned data keys; a key rotation adds a version, old data stays readable.
- Wrapping: the keyring is stored on the server sealed with
  `BLAKE3-derive-key("spoko ssh master v1", <secret part of the SSH key>)` — the Ed25519 seed,
  the ECDSA scalar or the RSA private exponent. The comment and the file's encoding do not
  change it. Using the signing key as KDF input as well is a deliberate choice: one key pair
  does everything.
- Recovery key: 256-bit, shown once (first login of the account) as base32 groups; it wraps
  the same keyring, stored on the server. It only decrypts — it cannot log in.
- At rest on a device: `$SPOKO_HOME/identity.bin` (0600, ACL on Windows) holds the device id,
  server, session token, key path and fingerprint and the *SSH-wrapped* keyring — nothing that
  opens data without the SSH key.

**Passphrases.** A key without a passphrase: the daemon starts without asking. A key with one:
the daemon starts locked (status and tray show it, nothing syncs) until `spoko unlock` sends
the passphrase over the local socket (owner-only, mode 0600; on Windows the pipe accepts
writes only from its creator). The decrypted key lives only in the daemon's memory. CLI
commands that run without the daemon ask for the passphrase themselves.

**Integrity.** Every downloaded object is checked against its git hash; a tampered or
substituted blob fails decryption or the hash check and the sync stops.

**Lost key.** The server admin sets the account's new public key
(`spoko-server account set-key`, which also ends every session); then
`spoko login --key <new> --recovery-key …` opens the old keyring with the recovery key and
wraps it for the new SSH key. The server accepts a new wrapped keyring only while the stored
one is not for the account's current key, so a stolen session token cannot replace it.

**Leaked key.** `spoko key rotate --new <file>` proves possession of the new key (it signs a
challenge issued for it), moves the account to it, adds a new data-key version, prints a new
recovery key (the old one opens only data from before) and ends every other session; the
other machines log in again with the new key. The old key cannot log in any more.

**Removing a device.** `spoko device remove <name>` ends that machine's session. It is not a
security boundary: a machine that has the SSH key can log in again. To cut a machine off for
good, rotate the key.
