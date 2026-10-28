use spoko_crypto::ObjectId;

#[derive(Debug, thiserror::Error)]
#[error("truncated batch")]
pub struct FramingError;

/// Batch body: repeated `[32 B id][u32 LE len][bytes]`.
pub fn encode_batch<B: AsRef<[u8]>>(items: &[(ObjectId, B)]) -> Vec<u8> {
    let total: usize = items.iter().map(|(_, b)| 36 + b.as_ref().len()).sum();
    let mut out = Vec::with_capacity(total);
    for (id, bytes) in items {
        let bytes = bytes.as_ref();
        out.extend_from_slice(&id.0);
        out.extend_from_slice(&u32::try_from(bytes.len()).expect("object fits u32").to_le_bytes());
        out.extend_from_slice(bytes);
    }
    out
}

pub fn decode_batch(mut buf: &[u8]) -> Result<Vec<(ObjectId, Vec<u8>)>, FramingError> {
    let mut out = Vec::new();
    while !buf.is_empty() {
        if buf.len() < 36 {
            return Err(FramingError);
        }
        let id = ObjectId(buf[..32].try_into().expect("32 bytes"));
        let len = u32::from_le_bytes(buf[32..36].try_into().expect("4 bytes")) as usize;
        let rest = &buf[36..];
        if rest.len() < len {
            return Err(FramingError);
        }
        out.push((id, rest[..len].to_vec()));
        buf = &rest[len..];
    }
    Ok(out)
}
