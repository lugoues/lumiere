use serde::{Deserialize, Serialize};
use thiserror::Error;

/// An error caused by a numeric value outside its type's permitted range.
#[derive(Clone, PartialEq, Eq, Debug, Error)]
#[error("{field} value {value} is outside the allowed range {min}..={max}")]
pub struct RangeError {
    pub field: &'static str,
    pub value: i64,
    pub min: i64,
    pub max: i64,
}

macro_rules! bounded_type {
    ($name:ident, $inner:ty, $input:ty, $input_name:literal, $field:literal, $min:expr, $max:expr, $($derive:ident),*) => {
        #[doc = concat!("A validated ", $field, " value.")]
        #[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, $($derive),*)]
        #[serde(try_from = $input_name, into = $input_name)]
        pub struct $name($inner);

        impl $name {
            /// Constructs a value after validating its range.
            pub fn new(value: $inner) -> Result<Self, RangeError> {
                Self::try_from(value as $input)
            }

            /// Returns the underlying numeric value.
            pub const fn get(self) -> $inner { self.0 }
        }

        impl TryFrom<$input> for $name {
            type Error = RangeError;
            fn try_from(value: $input) -> Result<Self, Self::Error> {
                let comparable = value as i64;
                if comparable < $min as i64 || comparable > $max as i64 {
                    Err(RangeError { field: $field, value: comparable, min: $min as i64, max: $max as i64 })
                } else {
                    Ok(Self(value as $inner))
                }
            }
        }

        impl From<$name> for $input {
            fn from(value: $name) -> Self { value.0 as $input }
        }
    };
}

bounded_type!(
    Percent, u8, u16, "u16", "percent", 0, 100, PartialOrd, Ord, Hash
);
bounded_type!(Hue, u16, i32, "i32", "hue", 0, 359,);
bounded_type!(
    Kelvin, u16, i32, "i32", "kelvin", 2500, 10000, PartialOrd, Ord, Hash
);
bounded_type!(SceneId, u8, i32, "i32", "scene", 1, 9,);

impl Hue {
    /// Wraps any integer angle into the range 0 through 359 degrees.
    pub fn wrapping(value: i32) -> Self {
        Self(value.rem_euclid(360) as u16)
    }
}

/// A requested operating mode for a light.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Mode {
    Off,
    On,
    Cct {
        temp: Kelvin,
        bri: Percent,
    },
    Hsi {
        hue: Hue,
        sat: Percent,
        bri: Percent,
    },
    Scene {
        scene: SceneId,
        bri: Percent,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, to_value};

    #[test]
    fn mode_json_shapes_round_trip() {
        let modes = [
            (Mode::Off, json!({"mode":"off"})),
            (Mode::On, json!({"mode":"on"})),
            (
                Mode::Cct {
                    temp: Kelvin::new(5600).unwrap(),
                    bri: Percent::new(50).unwrap(),
                },
                json!({"mode":"cct","temp":5600,"bri":50}),
            ),
            (
                Mode::Hsi {
                    hue: Hue::new(120).unwrap(),
                    sat: Percent::new(70).unwrap(),
                    bri: Percent::new(40).unwrap(),
                },
                json!({"mode":"hsi","hue":120,"sat":70,"bri":40}),
            ),
            (
                Mode::Scene {
                    scene: SceneId::new(3).unwrap(),
                    bri: Percent::new(80).unwrap(),
                },
                json!({"mode":"scene","scene":3,"bri":80}),
            ),
        ];
        for (mode, expected) in modes {
            assert_eq!(to_value(mode).unwrap(), expected);
            assert_eq!(serde_json::from_value::<Mode>(expected).unwrap(), mode);
        }
    }

    #[test]
    fn deserialization_rejects_invalid_ranges() {
        assert!(serde_json::from_str::<Hue>("360").is_err());
        assert!(serde_json::from_str::<Percent>("101").is_err());
        assert!(serde_json::from_str::<Kelvin>("2400").is_err());
    }
}
