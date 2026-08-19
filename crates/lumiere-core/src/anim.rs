use std::collections::VecDeque;

use lumiere_proto::{AnimTarget, Animation, Hue, Kelvin, Mode, Percent, PlaybackOptions};

/// A group of target updates scheduled at one playback-relative instant.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Frame {
    pub at_ms: u64,
    pub ops: Vec<(AnimTarget, Mode)>,
}

/// Builds a clock-free iterator over an entire playback.
pub fn schedule<'a>(
    anim: &'a Animation,
    opts: &PlaybackOptions,
) -> impl Iterator<Item = Frame> + 'a {
    Schedule {
        anim,
        speed: f64::from(opts.speed),
        step_ms: (1000 / u64::from(opts.fps)).max(33),
        bri_scale: opts.bri_scale,
        looping: opts.loop_override.unwrap_or(anim.loop_default),
        loop_limit: if opts.loop_override.unwrap_or(anim.loop_default) && opts.max_loops > 0 {
            Some(opts.max_loops)
        } else {
            None
        },
        loop_index: 0,
        keyframe_index: 0,
        at_ms: 0,
        pending: VecDeque::new(),
        done: false,
    }
}

/// Returns the finite playback end time, or `None` for an unbounded loop.
pub fn playback_duration(anim: &Animation, opts: &PlaybackOptions) -> Option<u64> {
    let looping = opts.loop_override.unwrap_or(anim.loop_default);
    if looping && opts.max_loops == 0 {
        return None;
    }
    let loops = if looping { opts.max_loops } else { 1 };
    let speed = f64::from(opts.speed);
    let step_ms = (1000 / u64::from(opts.fps)).max(33);
    let mut duration = 0_u64;
    for loop_index in 0..loops {
        for (keyframe_index, keyframe) in anim.keyframes.iter().enumerate() {
            let has_previous = keyframe_index > 0 || (loop_index > 0 && looping);
            let mut fade_ms = scaled_ms(keyframe.fade_ms, speed);
            if keyframe_index == 0 && loop_index > 0 && looping && fade_ms == 0 {
                let seam_fade = anim.keyframes.last().map_or(0, |last| last.fade_ms);
                if seam_fade > 0 {
                    fade_ms = scaled_ms(seam_fade, speed);
                }
            }
            if has_previous && fade_ms > 0 {
                duration += (fade_ms / step_ms).max(1) * step_ms;
            }
            duration += (scaled_ms(keyframe.hold_ms, speed).max(50) / 50).max(1) * 50;
        }
    }
    Some(duration)
}

struct Schedule<'a> {
    anim: &'a Animation,
    speed: f64,
    step_ms: u64,
    bri_scale: f32,
    looping: bool,
    loop_limit: Option<u32>,
    loop_index: u32,
    keyframe_index: usize,
    at_ms: u64,
    pending: VecDeque<Frame>,
    done: bool,
}

impl Iterator for Schedule<'_> {
    type Item = Frame;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(frame) = self.pending.pop_front() {
                return Some(frame);
            }
            if self.done || self.anim.keyframes.is_empty() {
                return None;
            }
            self.queue_keyframe();
        }
    }
}

impl Schedule<'_> {
    fn queue_keyframe(&mut self) {
        let keyframe = &self.anim.keyframes[self.keyframe_index];
        let previous_index = if self.keyframe_index > 0 {
            Some(self.keyframe_index - 1)
        } else if self.loop_index > 0 && self.looping {
            Some(self.anim.keyframes.len() - 1)
        } else {
            None
        };
        // The reference dwells in 50ms poll steps: max(1, hold // 50) * 50.
        // The shipped animations were tuned against that quantization.
        let hold_ms = (scaled_ms(keyframe.hold_ms, self.speed).max(50) / 50).max(1) * 50;
        let mut fade_ms = scaled_ms(keyframe.fade_ms, self.speed);
        if self.keyframe_index == 0
            && previous_index == Some(self.anim.keyframes.len() - 1)
            && fade_ms == 0
        {
            let seam_fade = self
                .anim
                .keyframes
                .last()
                .expect("animation is nonempty")
                .fade_ms;
            if seam_fade > 0 {
                fade_ms = scaled_ms(seam_fade, self.speed);
            }
        }

        if fade_ms > 0
            && let Some(previous_index) = previous_index
        {
            let previous = &self.anim.keyframes[previous_index];
            let steps = (fade_ms / self.step_ms).max(1);
            for step in 0..steps {
                let last = step + 1 == steps;
                let t = (step + 1) as f64 / steps as f64;
                let ops: Vec<_> = keyframe
                    .lights
                    .iter()
                    .filter_map(|(target, mode)| {
                        // The reference defaults a target missing from the previous
                        // keyframe to its own params, so it emits the constant target
                        // value on every step. The per-light write dedupe collapses
                        // the redundant sends at the actor.
                        let prior = previous.lights.get(target).copied().unwrap_or(*mode);
                        interpolate(prior, *mode, t)
                            .or_else(|| last.then_some(*mode))
                            .map(|mode| (*target, scale_brightness(mode, self.bri_scale)))
                    })
                    .collect();
                // The reference engine never sends an empty frame (cross-mode
                // fades snap on the last step and are silent before it).
                if !ops.is_empty() {
                    self.pending.push_back(Frame {
                        at_ms: self.at_ms + step * self.step_ms,
                        ops,
                    });
                }
            }
            self.at_ms += steps * self.step_ms;
        } else {
            self.pending.push_back(Frame {
                at_ms: self.at_ms,
                ops: keyframe
                    .lights
                    .iter()
                    .map(|(target, mode)| (*target, scale_brightness(*mode, self.bri_scale)))
                    .collect(),
            });
        }
        self.at_ms += hold_ms;
        self.advance_keyframe();
    }

    fn advance_keyframe(&mut self) {
        self.keyframe_index += 1;
        if self.keyframe_index < self.anim.keyframes.len() {
            return;
        }
        self.loop_index += 1;
        let reached_limit = self
            .loop_limit
            .is_some_and(|limit| self.loop_index >= limit);
        if self.looping && !reached_limit {
            self.keyframe_index = 0;
        } else {
            self.done = true;
        }
    }
}

fn scaled_ms(value: u32, speed: f64) -> u64 {
    (f64::from(value) / speed) as u64
}

fn interpolate(start: Mode, end: Mode, t: f64) -> Option<Mode> {
    match (start, end) {
        (
            Mode::Hsi {
                hue: start_hue,
                sat: start_sat,
                bri: start_bri,
            },
            Mode::Hsi {
                hue: end_hue,
                sat: end_sat,
                bri: end_bri,
            },
        ) => {
            let start_hue = i32::from(start_hue.get());
            let end_hue = i32::from(end_hue.get());
            let difference = (end_hue - start_hue + 540).rem_euclid(360) - 180;
            Some(Mode::Hsi {
                hue: Hue::wrapping(
                    (f64::from(start_hue) + f64::from(difference) * t).round() as i32
                ),
                sat: percent_lerp(start_sat, end_sat, t),
                bri: percent_lerp(start_bri, end_bri, t),
            })
        }
        (
            Mode::Cct {
                temp: start_temp,
                bri: start_bri,
            },
            Mode::Cct {
                temp: end_temp,
                bri: end_bri,
            },
        ) => {
            let start_hk = start_temp.get() / 100;
            let end_hk = end_temp.get() / 100;
            let temp_hk = lerp(f64::from(start_hk), f64::from(end_hk), t).round() as u16;
            Some(Mode::Cct {
                temp: Kelvin::new(temp_hk * 100).expect("interpolated Kelvin remains in range"),
                bri: percent_lerp(start_bri, end_bri, t),
            })
        }
        _ => None,
    }
}

fn percent_lerp(start: Percent, end: Percent, t: f64) -> Percent {
    let value = lerp(f64::from(start.get()), f64::from(end.get()), t).round() as u8;
    Percent::new(value).expect("interpolated percent remains in range")
}

fn lerp(start: f64, end: f64, t: f64) -> f64 {
    start + (end - start) * t
}

fn scale_brightness(mode: Mode, scale: f32) -> Mode {
    let scale = |bri: Percent| {
        let value = (f32::from(bri.get()) * scale).round().clamp(0.0, 100.0) as u8;
        Percent::new(value).expect("scaled percent remains in range")
    };
    match mode {
        Mode::Cct { temp, bri } => Mode::Cct {
            temp,
            bri: scale(bri),
        },
        Mode::Hsi { hue, sat, bri } => Mode::Hsi {
            hue,
            sat,
            bri: scale(bri),
        },
        Mode::Scene { scene, bri } => Mode::Scene {
            scene,
            bri: scale(bri),
        },
        Mode::On | Mode::Off => mode,
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, num::NonZeroU8};

    use lumiere_proto::{AnimationId, Keyframe};

    use super::*;

    fn hsi(hue: u16, bri: u8) -> Mode {
        Mode::Hsi {
            hue: Hue::new(hue).unwrap(),
            sat: Percent::new(100).unwrap(),
            bri: Percent::new(bri).unwrap(),
        }
    }

    fn animation(keyframes: Vec<Keyframe>, looping: bool) -> Animation {
        Animation {
            id: AnimationId::parse("test").unwrap(),
            name: "Test".to_owned(),
            description: String::new(),
            loop_default: looping,
            slot_count: 0,
            keyframes,
        }
    }

    fn keyframe(hold_ms: u32, fade_ms: u32, mode: Mode) -> Keyframe {
        Keyframe {
            hold_ms,
            fade_ms,
            lights: BTreeMap::from([(AnimTarget::All, mode)]),
        }
    }

    #[test]
    fn hold_only_frames_start_after_each_hold() {
        let anim = animation(
            vec![
                keyframe(100, 0, hsi(0, 100)),
                keyframe(200, 0, hsi(90, 100)),
            ],
            false,
        );
        let frames = schedule(&anim, &PlaybackOptions::default()).collect::<Vec<_>>();
        assert_eq!(
            frames.iter().map(|frame| frame.at_ms).collect::<Vec<_>>(),
            [0, 100]
        );
    }

    #[test]
    fn fade_steps_use_expected_times_and_fractions() {
        let anim = animation(
            vec![
                keyframe(100, 0, hsi(0, 0)),
                keyframe(50, 1000, hsi(100, 100)),
            ],
            false,
        );
        let frames = schedule(&anim, &PlaybackOptions::default()).collect::<Vec<_>>();
        assert_eq!(
            frames.iter().map(|frame| frame.at_ms).collect::<Vec<_>>(),
            [0, 100, 300, 500, 700, 900]
        );
        let hues = frames[1..]
            .iter()
            .map(|frame| match frame.ops[0].1 {
                Mode::Hsi { hue, .. } => hue.get(),
                _ => unreachable!(),
            })
            .collect::<Vec<_>>();
        assert_eq!(hues, [20, 40, 60, 80, 100]);
    }

    #[test]
    fn hue_takes_shortest_path_across_zero() {
        let anim = animation(
            vec![
                keyframe(50, 0, hsi(350, 100)),
                keyframe(50, 400, hsi(10, 100)),
            ],
            false,
        );
        let frames = schedule(&anim, &PlaybackOptions::default()).collect::<Vec<_>>();
        let hues = frames[1..]
            .iter()
            .map(|frame| match frame.ops[0].1 {
                Mode::Hsi { hue, .. } => hue.get(),
                _ => unreachable!(),
            })
            .collect::<Vec<_>>();
        assert_eq!(hues, [0, 10]);
    }

    #[test]
    fn cross_mode_snaps_only_on_last_step() {
        let anim = animation(
            vec![
                keyframe(50, 0, hsi(10, 100)),
                keyframe(
                    50,
                    400,
                    Mode::Cct {
                        temp: Kelvin::new(5000).unwrap(),
                        bri: Percent::new(80).unwrap(),
                    },
                ),
            ],
            false,
        );
        let frames = schedule(&anim, &PlaybackOptions::default()).collect::<Vec<_>>();
        // A 400ms fade at 200ms steps is 2 steps, but only the final snap is
        // emitted for a cross-mode transition; silent steps produce no frame.
        assert_eq!(frames.len(), 2);
        assert!(matches!(frames[1].ops[0].1, Mode::Cct { .. }));
        assert_eq!(frames[1].at_ms, 50 + 200);
    }

    #[test]
    fn loop_seam_inherits_last_fade() {
        let anim = animation(
            vec![
                keyframe(50, 0, hsi(0, 100)),
                keyframe(50, 400, hsi(100, 100)),
            ],
            true,
        );
        let opts = PlaybackOptions {
            max_loops: 2,
            ..PlaybackOptions::default()
        };
        let frames = schedule(&anim, &opts).collect::<Vec<_>>();
        let seam = frames.iter().position(|frame| frame.at_ms == 500).unwrap();
        assert_eq!(frames[seam].ops[0].1, hsi(50, 100));
        assert_eq!(frames[seam + 1].ops[0].1, hsi(0, 100));
    }

    #[test]
    fn brightness_speed_and_loop_limit_apply() {
        let anim = animation(
            vec![keyframe(100, 0, hsi(0, 75)), keyframe(100, 0, hsi(20, 75))],
            true,
        );
        let opts = PlaybackOptions {
            speed: 2.0,
            bri_scale: 0.5,
            max_loops: 3,
            ..PlaybackOptions::default()
        };
        let frames = schedule(&anim, &opts).collect::<Vec<_>>();
        assert_eq!(frames.len(), 3 * 2);
        assert_eq!(
            frames.iter().map(|frame| frame.at_ms).collect::<Vec<_>>(),
            [0, 50, 100, 150, 200, 250]
        );
        assert!(
            frames
                .iter()
                .all(|frame| matches!(frame.ops[0].1, Mode::Hsi { bri, .. } if bri.get() == 38))
        );
    }

    #[test]
    fn newly_appearing_slot_snaps_on_last_step() {
        let slot = AnimTarget::Slot(NonZeroU8::new(1).unwrap());
        let anim = animation(
            vec![
                keyframe(50, 0, hsi(0, 100)),
                Keyframe {
                    hold_ms: 50,
                    fade_ms: 400,
                    lights: BTreeMap::from([(slot, hsi(90, 100))]),
                },
            ],
            false,
        );
        let frames = schedule(&anim, &PlaybackOptions::default()).collect::<Vec<_>>();
        // A slot missing from the previous keyframe fades from itself, exactly
        // like the reference: the constant target value on every step.
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[1].ops, vec![(slot, hsi(90, 100))]);
        assert_eq!(frames[2].ops, vec![(slot, hsi(90, 100))]);
    }
}
