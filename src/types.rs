//! Newtypes for the three kinds of identifiers the proxy juggles, so they
//! cannot be mixed up at compile time.

use serde::Serialize;
use std::fmt;

/// Identity of an upstream server, assigned by
/// [`ServerRegistry`](crate::servers::ServerRegistry) per server *name*:
/// config order at startup, then appended as servers are added at runtime.
/// Never reused or renumbered while the process runs (a removed server keeps
/// its id, and gets it back if it is re-added).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ServerId(pub usize);

impl ServerId {
    pub fn index(self) -> usize {
        self.0
    }
}

impl fmt::Display for ServerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Normalise a Jellyfin id (32 hex chars, dashed GUID or not) to lowercase
/// hex without dashes.
pub fn normalize_hex_id(s: &str) -> Option<String> {
    let t: String = s
        .chars()
        .filter(|c| *c != '-' && *c != '{' && *c != '}')
        .collect();
    if t.len() == 32 && t.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(t.to_ascii_lowercase())
    } else {
        None
    }
}

macro_rules! hex_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Parse any accepted spelling (dashed or not, any case).
            pub fn parse(s: &str) -> Option<Self> {
                normalize_hex_id(s).map($name)
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                $name::parse(&s)
                    .ok_or_else(|| serde::de::Error::custom(format!("invalid id {s:?}")))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl From<$name> for String {
            fn from(v: $name) -> String {
                v.0
            }
        }
    };
}

hex_id!(
    /// An id the proxy hands to clients (32 lowercase hex chars).
    ProxyItemId
);
hex_id!(
    /// An id as it exists on one upstream server (normalised hex).
    UpstreamItemId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_normalise() {
        let a = UpstreamItemId::parse("A1B2C3D4-0000-0000-0000-00000000000F").unwrap();
        assert_eq!(a.as_str(), "a1b2c3d400000000000000000000000f");
        assert!(ProxyItemId::parse("nope").is_none());
        assert_eq!(ServerId(2).to_string(), "#2");
    }
}
