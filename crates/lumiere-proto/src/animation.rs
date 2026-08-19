use std::{collections::BTreeMap, fmt, num::NonZeroU8};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use smol_str::SmolStr;

use crate::{LightId, Mode, Selector};

/// A validated, URL-safe animation identifier.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AnimationId(SmolStr);

impl AnimationId {
    /// Parses a nonempty lowercase alphanumeric-and-dash slug.
    pub fn parse(value: &str) -> Result<Self, String> {
        if !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            Ok(Self(value.into()))
        } else {
            Err(format!("invalid animation id: {value}"))
        }
    }

    /// Returns the slug text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for AnimationId {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<AnimationId> for String {
    fn from(value: AnimationId) -> Self {
        value.0.into()
    }
}

impl fmt::Display for AnimationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One animation target, either the resolved all selector or a numbered slot.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum AnimTarget {
    All,
    Slot(NonZeroU8),
}

impl Serialize for AnimTarget {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::All => serializer.serialize_str("*"),
            Self::Slot(slot) => serializer.collect_str(slot),
        }
    }
}

impl<'de> Deserialize<'de> for AnimTarget {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value == "*" {
            return Ok(Self::All);
        }
        value
            .parse::<NonZeroU8>()
            .map(Self::Slot)
            .map_err(|_| de::Error::custom(format!("invalid animation target: {value}")))
    }
}

/// A complete animation definition.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Animation {
    pub id: AnimationId,
    pub name: String,
    pub description: String,
    pub loop_default: bool,
    pub slot_count: u8,
    pub keyframes: Vec<Keyframe>,
}

impl Animation {
    /// Validates animation invariants that span multiple serialized fields.
    pub fn validate(&self) -> Result<(), String> {
        if self.keyframes.is_empty() {
            return Err("animation must contain at least one keyframe".to_owned());
        }
        let mut highest_slot = 0;
        for (index, keyframe) in self.keyframes.iter().enumerate() {
            if keyframe.hold_ms > 600_000 {
                return Err(format!("keyframe {index} hold_ms exceeds 600000"));
            }
            if keyframe.fade_ms > 600_000 {
                return Err(format!("keyframe {index} fade_ms exceeds 600000"));
            }
            let has_all = keyframe.lights.contains_key(&AnimTarget::All);
            let has_slot = keyframe
                .lights
                .keys()
                .any(|target| matches!(target, AnimTarget::Slot(_)));
            if has_all && has_slot {
                return Err(format!(
                    "keyframe {index} may not mix the all target with slots"
                ));
            }
            for target in keyframe.lights.keys() {
                if let AnimTarget::Slot(slot) = target {
                    highest_slot = highest_slot.max(slot.get());
                }
            }
        }
        if self.slot_count != highest_slot {
            return Err(format!(
                "slot_count {} does not match highest referenced slot {highest_slot}",
                self.slot_count
            ));
        }
        Ok(())
    }

    /// Returns the compact list representation of this animation.
    pub fn summary(&self) -> AnimationSummary {
        AnimationSummary {
            id: self.id.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
            keyframes: self.keyframes.len() as u32,
            loop_default: self.loop_default,
            slot_count: self.slot_count,
        }
    }
}

/// A single animation keyframe.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Keyframe {
    pub hold_ms: u32,
    pub fade_ms: u32,
    pub lights: BTreeMap<AnimTarget, Mode>,
}

/// Runtime controls for animation playback.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct PlaybackOptions {
    #[serde(default = "default_speed")]
    pub speed: f32,
    #[serde(default = "default_fps")]
    pub fps: u8,
    #[serde(default = "default_bri_scale")]
    pub bri_scale: f32,
    #[serde(default)]
    pub loop_override: Option<bool>,
    #[serde(default)]
    pub max_loops: u32,
    #[serde(default = "default_true")]
    pub revert_on_finish: bool,
}

impl PlaybackOptions {
    /// Validates all bounded playback controls.
    pub fn validate(&self) -> Result<(), String> {
        if !self.speed.is_finite() || !(0.1..=10.0).contains(&self.speed) {
            return Err("speed must be between 0.1 and 10.0".to_owned());
        }
        if !(1..=30).contains(&self.fps) {
            return Err("fps must be between 1 and 30".to_owned());
        }
        if !self.bri_scale.is_finite() || !(0.0..=1.0).contains(&self.bri_scale) {
            return Err("bri_scale must be between 0.0 and 1.0".to_owned());
        }
        Ok(())
    }
}

impl Default for PlaybackOptions {
    fn default() -> Self {
        Self {
            speed: default_speed(),
            fps: default_fps(),
            bri_scale: default_bri_scale(),
            loop_override: None,
            max_loops: 0,
            revert_on_finish: true,
        }
    }
}

const fn default_speed() -> f32 {
    1.0
}

const fn default_fps() -> u8 {
    5
}

const fn default_bri_scale() -> f32 {
    1.0
}

const fn default_true() -> bool {
    true
}

/// Resolves animation targets to the current light set at play time.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TargetBinding {
    #[serde(default)]
    pub all: Selector,
    #[serde(default)]
    pub slots: Vec<LightId>,
}

impl Default for TargetBinding {
    fn default() -> Self {
        Self {
            all: Selector::All,
            slots: Vec::new(),
        }
    }
}

/// The client-visible state of the current playback.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PlaybackStatus {
    pub animation: AnimationId,
    pub name: String,
    pub started_ms: u64,
    pub looping: bool,
}

/// Compact metadata returned when listing animations.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct AnimationSummary {
    pub id: AnimationId,
    pub name: String,
    pub description: String,
    pub keyframes: u32,
    pub loop_default: bool,
    pub slot_count: u8,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Hue, Percent};

    #[test]
    fn target_map_keys_round_trip_as_strings() {
        let lights = BTreeMap::from([(
            AnimTarget::All,
            Mode::Hsi {
                hue: Hue::new(20).unwrap(),
                sat: Percent::new(80).unwrap(),
                bri: Percent::new(60).unwrap(),
            },
        )]);
        let encoded = serde_json::to_string(&lights).unwrap();
        assert_eq!(
            encoded,
            r#"{"*":{"mode":"hsi","hue":20,"sat":80,"bri":60}}"#
        );
        assert_eq!(
            serde_json::from_str::<BTreeMap<AnimTarget, Mode>>(&encoded).unwrap(),
            lights
        );

        let slots = BTreeMap::from([(AnimTarget::Slot(NonZeroU8::new(2).unwrap()), Mode::On)]);
        let encoded = serde_json::to_string(&slots).unwrap();
        assert_eq!(encoded, r#"{"2":{"mode":"on"}}"#);
        assert_eq!(
            serde_json::from_str::<BTreeMap<AnimTarget, Mode>>(&encoded).unwrap(),
            slots
        );
    }

    #[test]
    fn playback_defaults_deserialize_from_empty_object() {
        assert_eq!(
            serde_json::from_str::<PlaybackOptions>("{}").unwrap(),
            PlaybackOptions::default()
        );
    }
}
