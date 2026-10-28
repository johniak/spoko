//! Serde helpers for `git2::Oid` (hex strings).

use git2::Oid;
use serde::{Deserialize, Deserializer, Serializer};

pub fn serialize<S: Serializer>(oid: &Oid, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(oid)
}

pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Oid, D::Error> {
    let s = String::deserialize(d)?;
    Oid::from_str(&s).map_err(serde::de::Error::custom)
}

pub mod opt {
    use git2::Oid;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(oid: &Option<Oid>, s: S) -> Result<S::Ok, S::Error> {
        match oid {
            Some(o) => s.collect_str(o),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Oid>, D::Error> {
        Option::<String>::deserialize(d)?.map(|s| Oid::from_str(&s).map_err(serde::de::Error::custom)).transpose()
    }
}
