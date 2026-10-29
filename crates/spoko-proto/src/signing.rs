//! What the SSH key signs: bound to the purpose and to the server, so a signature made for one
//! server cannot be relayed to another, nor a login signature used as a key rotation.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignPurpose {
    Login,
    /// The account's current key approves the rotation.
    RotateOld,
    /// The new key proves it is held.
    RotateNew,
}

/// The message signed for a challenge `nonce` of the server at `server` (its URL on the client,
/// the `Host` header on the server — both go through [`authority`]).
pub fn signed_message(purpose: SignPurpose, server: &str, nonce: &[u8]) -> Vec<u8> {
    let tag = match purpose {
        SignPurpose::Login => "spoko-login v1",
        SignPurpose::RotateOld => "spoko-rotate-old v1",
        SignPurpose::RotateNew => "spoko-rotate-new v1",
    };
    let hex: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    format!("{tag}\n{}\n{hex}", authority(server)).into_bytes()
}

/// `host[:port]`, lowercased, of a URL or a `Host` header value; the default ports 80 and 443
/// are dropped (a client may spell them out, a `Host` header usually does not).
pub fn authority(url_or_host: &str) -> String {
    let rest = url_or_host.trim().split_once("://").map_or(url_or_host.trim(), |(_, r)| r);
    let auth = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let auth = auth.rsplit_once('@').map_or(auth, |(_, h)| h).to_ascii_lowercase();
    auth.strip_suffix(":80").or_else(|| auth.strip_suffix(":443")).map(str::to_owned).unwrap_or(auth)
}
