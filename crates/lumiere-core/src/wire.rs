use lumiere_proto::{Capabilities, Hue, Kelvin, Mode, Percent};
use thiserror::Error;

/// One complete Neewer BLE command packet.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Packet(Vec<u8>);

impl Packet {
    /// Returns the packet bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl AsRef<[u8]> for Packet {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

/// The one or two packets needed to apply a mode.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Encoded(Vec<Packet>);

impl Encoded {
    /// Returns the packets in transmission order.
    pub fn packets(&self) -> &[Packet] {
        &self.0
    }
}

impl IntoIterator for Encoded {
    type Item = Packet;
    type IntoIter = std::vec::IntoIter<Packet>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

/// Calculates the protocol's wrapping byte-sum checksum.
pub fn checksum(bytes_without_checksum: &[u8]) -> u8 {
    bytes_without_checksum
        .iter()
        .fold(0, |sum, byte| sum.wrapping_add(*byte))
}

fn packet(opcode: u8, payload: &[u8]) -> Packet {
    let mut bytes = Vec::with_capacity(payload.len() + 4);
    bytes.extend_from_slice(&[0x78, opcode, payload.len() as u8]);
    bytes.extend_from_slice(payload);
    bytes.push(checksum(&bytes));
    Packet(bytes)
}

/// Encodes a mode as one or two device packets.
pub fn encode(mode: Mode, caps: &Capabilities) -> Encoded {
    let packets = match mode {
        Mode::On => vec![packet(0x81, &[1])],
        Mode::Off => vec![packet(0x81, &[2])],
        Mode::Cct { temp, bri } if caps.cct_split_packets => vec![
            packet(0x82, &[bri.get()]),
            packet(0x83, &[(temp.get() / 100) as u8]),
        ],
        Mode::Cct { temp, bri } => vec![packet(0x87, &[bri.get(), (temp.get() / 100) as u8])],
        Mode::Hsi { hue, sat, bri } => {
            let [low, high] = hue.get().to_le_bytes();
            vec![packet(0x86, &[low, high, sat.get(), bri.get()])]
        }
        Mode::Scene { scene, bri } => vec![packet(0x88, &[bri.get(), scene.get()])],
    };
    Encoded(packets)
}

/// A decoded command retaining the values represented on the wire.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Decoded {
    Power(bool),
    Cct { temp_hk: u8, bri: u8 },
    Hsi { hue: u16, sat: u8, bri: u8 },
    Scene { scene: u8, bri: u8 },
    BriOnly(u8),
    TempOnly(u8),
}

/// A malformed or unsupported wire packet.
#[derive(Clone, PartialEq, Eq, Debug, Error)]
pub enum DecodeError {
    #[error("packet is truncated: expected at least 4 bytes, got {actual}")]
    Truncated { actual: usize },
    #[error("invalid packet prefix 0x{actual:02x}")]
    WrongPrefix { actual: u8 },
    #[error("payload length declares {declared} bytes but packet contains {actual}")]
    WrongLength { declared: usize, actual: usize },
    #[error("checksum mismatch: expected 0x{expected:02x}, got 0x{actual:02x}")]
    BadChecksum { expected: u8, actual: u8 },
    #[error("unsupported opcode 0x{opcode:02x} with payload length {length}")]
    Unsupported { opcode: u8, length: usize },
    #[error("invalid power payload {0}")]
    InvalidPower(u8),
}

/// Validates and decodes one raw command packet.
pub fn decode(raw: &[u8]) -> Result<Decoded, DecodeError> {
    if raw.len() < 4 {
        return Err(DecodeError::Truncated { actual: raw.len() });
    }
    if raw[0] != 0x78 {
        return Err(DecodeError::WrongPrefix { actual: raw[0] });
    }
    let declared = raw[2] as usize;
    let actual = raw.len() - 4;
    if declared != actual {
        return Err(DecodeError::WrongLength { declared, actual });
    }
    let expected = checksum(&raw[..raw.len() - 1]);
    let actual_checksum = raw[raw.len() - 1];
    if expected != actual_checksum {
        return Err(DecodeError::BadChecksum {
            expected,
            actual: actual_checksum,
        });
    }
    let payload = &raw[3..raw.len() - 1];
    match (raw[1], payload) {
        (0x81, [1]) => Ok(Decoded::Power(true)),
        (0x81, [2]) => Ok(Decoded::Power(false)),
        (0x81, [value]) => Err(DecodeError::InvalidPower(*value)),
        (0x82, [bri]) => Ok(Decoded::BriOnly(*bri)),
        (0x83, [temp_hk]) => Ok(Decoded::TempOnly(*temp_hk)),
        (0x87, [bri, temp_hk]) => Ok(Decoded::Cct {
            temp_hk: *temp_hk,
            bri: *bri,
        }),
        (0x86, [low, high, sat, bri]) => Ok(Decoded::Hsi {
            hue: u16::from_le_bytes([*low, *high]),
            sat: *sat,
            bri: *bri,
        }),
        (0x88, [bri, scene]) => Ok(Decoded::Scene {
            scene: *scene,
            bri: *bri,
        }),
        (opcode, payload) => Err(DecodeError::Unsupported {
            opcode,
            length: payload.len(),
        }),
    }
}

/// Approximates an HSI color as a CCT mode for lights that cannot do color.
///
/// The reference maps hue bands into the light's temperature range: warm hues
/// (reds, oranges, magentas) sit at the warm end, greens ramp toward the
/// middle, cyans and blues ramp to the cool end. Saturation is ignored. The
/// math runs in hundreds-of-kelvin units to match the reference's integer
/// rounding exactly.
pub fn hsi_to_cct(hue: Hue, bri: Percent, caps: &Capabilities) -> Mode {
    let min = i32::from(caps.cct_min.get() / 100);
    let max = i32::from(caps.cct_max.get() / 100);
    let mid = (min + max) / 2;
    let hue = i32::from(hue.get());
    let temp = if !(61..300).contains(&hue) {
        min
    } else if hue <= 150 {
        min + (hue - 60) * (max - min) / 2 / 90
    } else if hue <= 250 {
        mid + (hue - 150) * (max - mid) / 100
    } else {
        mid
    };
    let temp = temp.clamp(min, max) as u16 * 100;
    Mode::Cct {
        temp: Kelvin::new(temp).expect("capability ranges are valid Kelvin"),
        bri,
    }
}

/// Clamps CCT modes to device limits and rounds temperature to the nearest 100 K.
pub fn clamp_to_device(mode: Mode, caps: &Capabilities) -> (Mode, bool) {
    let Mode::Cct { temp, bri } = mode else {
        return (mode, false);
    };
    let clamped = temp.get().clamp(caps.cct_min.get(), caps.cct_max.get());
    let rounded = ((u32::from(clamped) + 50) / 100 * 100) as u16;
    let adjusted = rounded.clamp(caps.cct_min.get(), caps.cct_max.get());
    let result = Mode::Cct {
        temp: Kelvin::new(adjusted).expect("capability ranges must be valid Kelvin values"),
        bri,
    };
    (result, result != mode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumiere_proto::{Hue, Percent, SceneId};
    use proptest::prelude::*;
    use serde::Deserialize;

    fn caps(split: bool) -> Capabilities {
        Capabilities {
            cct_min: Kelvin::new(2500).unwrap(),
            cct_max: Kelvin::new(10000).unwrap(),
            rgb: true,
            scenes: true,
            cct_split_packets: split,
            reports_status: true,
        }
    }

    #[derive(Deserialize)]
    struct Golden {
        mode: String,
        #[serde(default)]
        bri: u8,
        #[serde(default)]
        temp_hk: u8,
        #[serde(default)]
        hue: u16,
        #[serde(default)]
        sat: u8,
        #[serde(default)]
        scene: u8,
        #[serde(default)]
        bytes: Vec<u8>,
        #[serde(default)]
        bri_bytes: Vec<u8>,
        #[serde(default)]
        temp_bytes: Vec<u8>,
    }

    #[test]
    fn golden_vectors() {
        let source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/fixtures/wire_golden.json"
        ));
        for item in serde_json::from_str::<Vec<Golden>>(source).unwrap() {
            let (mode, split) = match item.mode.as_str() {
                "power_on" => (Mode::On, false),
                "power_off" => (Mode::Off, false),
                "cct" => (
                    Mode::Cct {
                        temp: Kelvin::new(u16::from(item.temp_hk) * 100).unwrap(),
                        bri: Percent::new(item.bri).unwrap(),
                    },
                    false,
                ),
                "cct_split" => (
                    Mode::Cct {
                        temp: Kelvin::new(u16::from(item.temp_hk) * 100).unwrap(),
                        bri: Percent::new(item.bri).unwrap(),
                    },
                    true,
                ),
                // Python emitted raw 360, while our validated hue normalizes the equivalent color to 0.
                "hsi" if item.hue == 360 => continue,
                "hsi" => (
                    Mode::Hsi {
                        hue: Hue::new(item.hue).unwrap(),
                        sat: Percent::new(item.sat).unwrap(),
                        bri: Percent::new(item.bri).unwrap(),
                    },
                    false,
                ),
                "scene" => (
                    Mode::Scene {
                        scene: SceneId::new(item.scene).unwrap(),
                        bri: Percent::new(item.bri).unwrap(),
                    },
                    false,
                ),
                other => panic!("unknown fixture mode {other}"),
            };
            let actual: Vec<Vec<u8>> = encode(mode, &caps(split))
                .packets()
                .iter()
                .map(|p| p.as_bytes().to_vec())
                .collect();
            let expected = if split {
                vec![item.bri_bytes, item.temp_bytes]
            } else {
                vec![item.bytes]
            };
            assert_eq!(actual, expected, "fixture mode {}", item.mode);
        }
    }

    proptest! {
        #[test]
        fn encoded_packets_decode(hue in 0u16..360, sat in 0u8..=100, bri in 0u8..=100, temp_hk in 25u8..=100, split in any::<bool>()) {
            let modes = [Mode::On, Mode::Off, Mode::Cct { temp: Kelvin::new(u16::from(temp_hk) * 100).unwrap(), bri: Percent::new(bri).unwrap() }, Mode::Hsi { hue: Hue::new(hue).unwrap(), sat: Percent::new(sat).unwrap(), bri: Percent::new(bri).unwrap() }, Mode::Scene { scene: SceneId::new((bri % 9) + 1).unwrap(), bri: Percent::new(bri).unwrap() }];
            for mode in modes {
                let packets = encode(mode, &caps(split)).packets().to_vec();
                for packet in &packets {
                    let bytes = packet.as_bytes();
                    prop_assert_eq!(bytes[bytes.len()-1], checksum(&bytes[..bytes.len()-1]));
                }
                // Decoded wire values must round-trip to what was encoded.
                let decoded: Vec<Decoded> = packets.iter().map(|p| decode(p.as_bytes()).unwrap()).collect();
                let expected = match mode {
                    Mode::On => vec![Decoded::Power(true)],
                    Mode::Off => vec![Decoded::Power(false)],
                    Mode::Cct { temp, bri } if split => vec![
                        Decoded::BriOnly(bri.get()),
                        Decoded::TempOnly((temp.get() / 100) as u8),
                    ],
                    Mode::Cct { temp, bri } => vec![Decoded::Cct { temp_hk: (temp.get() / 100) as u8, bri: bri.get() }],
                    Mode::Hsi { hue, sat, bri } => vec![Decoded::Hsi { hue: hue.get(), sat: sat.get(), bri: bri.get() }],
                    Mode::Scene { scene, bri } => vec![Decoded::Scene { scene: scene.get(), bri: bri.get() }],
                };
                prop_assert_eq!(decoded, expected);
            }
        }
    }

    #[test]
    fn hsi_to_cct_matches_the_reference_band_math() {
        // Oracle values from executing the Python hsiToCCTByteVal with the
        // GL1 PRO range (2900 to 7000 K); temps in hundreds of kelvin.
        let mut caps = caps(false);
        caps.cct_min = Kelvin::new(2900).unwrap();
        caps.cct_max = Kelvin::new(7000).unwrap();
        let oracle = [
            (0, 29),
            (30, 29),
            (60, 29),
            (61, 29),
            (90, 35),
            (120, 42),
            (150, 49),
            (151, 49),
            (200, 59),
            (250, 70),
            (251, 49),
            (280, 49),
            (299, 49),
            (300, 29),
            (330, 29),
            (359, 29),
        ];
        for (hue, temp_hk) in oracle {
            let mode = hsi_to_cct(Hue::new(hue).unwrap(), Percent::new(50).unwrap(), &caps);
            assert_eq!(
                mode,
                Mode::Cct {
                    temp: Kelvin::new(temp_hk * 100).unwrap(),
                    bri: Percent::new(50).unwrap(),
                },
                "hue {hue}"
            );
        }
    }

    #[test]
    fn decode_rejects_bad_envelopes() {
        assert!(matches!(
            decode(&[0x78]),
            Err(DecodeError::Truncated { .. })
        ));
        assert!(matches!(
            decode(&[0, 0x81, 1, 1, 0]),
            Err(DecodeError::WrongPrefix { .. })
        ));
        assert!(matches!(
            decode(&[0x78, 0x81, 1, 1, 0]),
            Err(DecodeError::BadChecksum { .. })
        ));
    }
}
