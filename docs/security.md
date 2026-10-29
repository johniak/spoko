# Security model

**Threat model.** The server, its operator, its storage and the network are untrusted. Your
own unlocked devices are trusted: someone who has your laptop has your source code anyway.

**What the server sees.** Opaque 32-byte object ids, encrypted blobs and their sizes, pointer
versions, device names and public keys, and when devices are online. It never sees file
names, contents, branch names, commit messages or the data key.

**Cryptography** (no custom primitives):

- Data: XChaCha20-Poly1305 with a random 192-bit nonce per object, the object id as
  associated data (blobs cannot be swapped). Content is zstd-compressed before encryption.
- Ids: keyed BLAKE3 of the content id, so equal content is deduplicated without revealing
  git object ids.
- Keyring: versioned data keys; revoking a device rotates to a new key version, old data stays
  readable by remaining devices.
- Delivery to a new device: X25519 sealed box (`crypto_box`) to that device's public key,
  after you compare its fingerprint during `spoko device approve`.
- Recovery key: 256-bit, shown once as base32 groups; it wraps the keyring stored on the server.
- At rest on a device: `$SPOKO_HOME/identity.bin`, permissions 0600 (ACL on Windows).

**Integrity.** Every downloaded object is checked against its git hash; a tampered or
substituted blob fails decryption or the hash check and the sync stops.

**Revocation.** `spoko device revoke <name>` removes the device's token, rotates the data key,
re-wraps it for every remaining device and prints a new recovery key. A revoked device keeps
what it already downloaded.
