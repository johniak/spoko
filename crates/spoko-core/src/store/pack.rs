//! Writing many small objects as one git packfile instead of one loose file each.

use std::io::Write;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use git2::{ObjectType, Odb};
use sha1::{Digest, Sha1};

fn type_code(kind: ObjectType) -> u8 {
    match kind {
        ObjectType::Commit => 1,
        ObjectType::Tree => 2,
        ObjectType::Tag => 4,
        _ => 3,
    }
}

/// Pack v2 stream (non-delta objects) for `objects`.
pub fn encode<'a>(objects: impl ExactSizeIterator<Item = (ObjectType, &'a [u8])>) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"PACK");
    out.extend_from_slice(&2u32.to_be_bytes());
    out.extend_from_slice(&u32::try_from(objects.len()).expect("< 4G objects").to_be_bytes());
    for (kind, data) in objects {
        let mut size = data.len();
        let mut byte = (type_code(kind) << 4) | u8::try_from(size & 0x0f).expect("masked");
        size >>= 4;
        while size > 0 {
            out.push(byte | 0x80);
            byte = u8::try_from(size & 0x7f).expect("masked");
            size >>= 7;
        }
        out.push(byte);
        let mut z = ZlibEncoder::new(Vec::with_capacity(data.len() / 2 + 16), Compression::fast());
        z.write_all(data).expect("in-memory write");
        out.extend_from_slice(&z.finish().expect("in-memory write"));
    }
    let sum = Sha1::digest(&out);
    out.extend_from_slice(&sum);
    out
}

/// Indexes and stores the pack in `odb` (`objects/pack/pack-*.{pack,idx}`).
pub fn write(odb: &Odb<'_>, pack: &[u8]) -> Result<(), git2::Error> {
    let mut w = odb.packwriter()?;
    w.write_all(pack).map_err(super::io_err)?;
    w.commit()?;
    Ok(())
}
