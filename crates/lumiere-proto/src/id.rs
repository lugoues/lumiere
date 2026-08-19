use core::fmt;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use thiserror::Error;

/// An invalid platform light identifier.
#[derive(Clone, PartialEq, Eq, Debug, Error)]
#[error("invalid {kind} light identifier: {value}")]
pub struct IdError {
    kind: &'static str,
    value: SmolStr,
}

/// A stable light identifier including its platform namespace.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LightId(SmolStr);

impl LightId {
    /// Parses and normalizes a six-octet Bluetooth MAC address.
    pub fn mac(value: &str) -> Result<Self, IdError> {
        let parts: Vec<_> = value.split(':').collect();
        if parts.len() != 6
            || parts
                .iter()
                .any(|part| part.len() != 2 || u8::from_str_radix(part, 16).is_err())
        {
            return Err(IdError {
                kind: "MAC",
                value: value.into(),
            });
        }
        Ok(Self(
            format!(
                "mac:{}",
                parts
                    .iter()
                    .map(|p| p.to_ascii_uppercase())
                    .collect::<Vec<_>>()
                    .join(":")
            )
            .into(),
        ))
    }

    /// Parses and normalizes a CoreBluetooth UUID.
    pub fn corebluetooth(value: &str) -> Result<Self, IdError> {
        let bytes = value.as_bytes();
        let valid = bytes.len() == 36
            && bytes.iter().enumerate().all(|(i, b)| {
                if matches!(i, 8 | 13 | 18 | 23) {
                    *b == b'-'
                } else {
                    b.is_ascii_hexdigit()
                }
            });
        if !valid {
            return Err(IdError {
                kind: "CoreBluetooth UUID",
                value: value.into(),
            });
        }
        Ok(Self(format!("cb:{}", value.to_ascii_lowercase()).into()))
    }

    /// Creates an identifier in the simulator namespace.
    pub fn sim(name: &str) -> Self {
        Self(format!("sim:{name}").into())
    }

    /// Returns the complete namespaced identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns MAC address bytes when this is a MAC identifier.
    pub fn mac_octets(&self) -> Option<[u8; 6]> {
        let raw = self.0.strip_prefix("mac:")?;
        let mut octets = [0; 6];
        let mut parts = raw.split(':');
        for octet in &mut octets {
            *octet = u8::from_str_radix(parts.next()?, 16).ok()?;
        }
        if parts.next().is_some() {
            None
        } else {
            Some(octets)
        }
    }
}

impl fmt::Display for LightId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifiers_are_normalized() {
        let mac = LightId::mac("aa:0b:CC:dd:ee:fF").unwrap();
        assert_eq!(mac.as_str(), "mac:AA:0B:CC:DD:EE:FF");
        assert_eq!(mac.mac_octets(), Some([0xaa, 0x0b, 0xcc, 0xdd, 0xee, 0xff]));
        let cb = LightId::corebluetooth("ABCDEF01-2345-6789-ABCD-EF0123456789").unwrap();
        assert_eq!(cb.as_str(), "cb:abcdef01-2345-6789-abcd-ef0123456789");
        assert!(LightId::mac("aa:bb").is_err());
        assert!(LightId::corebluetooth("nope").is_err());
    }
}
