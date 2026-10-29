Test-only SSH keys (generated with ssh-keygen for spoko's tests; never used anywhere else). id_ed25519_pass and id_ecdsa521_pass passphrase: sekret

- id_ecdsa384, id_ecdsa521: `ssh-keygen -t ecdsa -b 384/521 -N ""`; id_ecdsa521 stores its scalar in 65 bytes (leading zero byte), id_ecdsa521_pass is the same key with a passphrase
- id_rsa_pem, id_ecdsa_pkcs8: `ssh-keygen -m PEM` / `-m PKCS8` (formats spoko refuses with a hint to convert)
