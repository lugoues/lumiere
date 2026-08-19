use lumiere_proto::Mode;

/// Formats a light mode for compact status and preset displays.
pub fn mode_summary(mode: Mode) -> String {
    match mode {
        Mode::Off => "Power off".into(),
        Mode::On => "Power on".into(),
        Mode::Cct { temp, bri } => format!("{} K · {}%", temp.get(), bri.get()),
        Mode::Hsi { hue, sat, bri } => {
            format!("{}° · S {}% · B {}%", hue.get(), sat.get(), bri.get())
        }
        Mode::Scene { scene, bri } => format!("Scene {} · {}%", scene.get(), bri.get()),
    }
}

#[cfg(test)]
mod tests {
    use lumiere_proto::{Hue, Kelvin, Percent, SceneId};

    use super::*;

    #[test]
    fn formats_each_mode_compactly() {
        assert_eq!(mode_summary(Mode::Off), "Power off");
        assert_eq!(mode_summary(Mode::On), "Power on");
        assert_eq!(
            mode_summary(Mode::Cct {
                temp: Kelvin::new(5_600).unwrap(),
                bri: Percent::new(75).unwrap(),
            }),
            "5600 K · 75%"
        );
        assert_eq!(
            mode_summary(Mode::Hsi {
                hue: Hue::new(240).unwrap(),
                sat: Percent::new(90).unwrap(),
                bri: Percent::new(60).unwrap(),
            }),
            "240° · S 90% · B 60%"
        );
        assert_eq!(
            mode_summary(Mode::Scene {
                scene: SceneId::new(3).unwrap(),
                bri: Percent::new(40).unwrap(),
            }),
            "Scene 3 · 40%"
        );
    }
}
