use std::{collections::BTreeMap, num::NonZeroU8, time::Duration};

use lumiere_core::wire::Decoded;
use lumiere_daemon::{RegistryConfig, RegistryHandle};
use lumiere_proto::{
    AnimTarget, Animation, AnimationId, ConnState, Event, Hue, Kelvin, Keyframe, LightId, Mode,
    PerLightResult, Percent, PlaybackOptions, Selector, TargetBinding,
};
use lumiere_transport::sim::{SimConfig, SimLightSpec, SimTransport};
use tokio::time::advance;

fn spec(id: &str, name: &str) -> SimLightSpec {
    SimLightSpec {
        id: LightId::sim(id),
        advertised_name: name.to_owned(),
        rssi: -40,
        connect_failures: 0,
    }
}

fn four_lights() -> Vec<SimLightSpec> {
    vec![
        spec("rgb660", "NEEWER-RGB660 PRO"),
        spec("snl660", "NEEWER-SNL660"),
        spec("rgb176", "NEEWER-RGB176"),
        spec("sl80", "NEEWER-SL80"),
    ]
}

async fn setup(lights: Vec<SimLightSpec>) -> (SimTransport, RegistryHandle) {
    let light_count = lights.len();
    let sim = SimTransport::new(SimConfig {
        lights,
        write_latency: Duration::ZERO,
        fail_every_nth_write: None,
    });
    let registry = RegistryHandle::spawn(sim.clone());
    registry.discover(Duration::from_secs(60)).await.unwrap();
    wait_until(|| {
        let world = registry.world();
        world.borrow().lights.len() == light_count
            && world
                .borrow()
                .lights
                .iter()
                .all(|light| light.conn == ConnState::Connected)
    })
    .await;
    (sim, registry)
}

async fn setup_animation(
    lights: Vec<SimLightSpec>,
    animation: &Animation,
) -> (SimTransport, RegistryHandle, tempfile::TempDir) {
    let light_count = lights.len();
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join(format!("{}.json", animation.id)),
        serde_json::to_vec(animation).unwrap(),
    )
    .unwrap();
    let sim = SimTransport::new(SimConfig {
        lights,
        write_latency: Duration::ZERO,
        fail_every_nth_write: None,
    });
    let registry = RegistryHandle::spawn_with_config(
        sim.clone(),
        RegistryConfig {
            animations_dir: Some(directory.path().to_owned()),
            ..RegistryConfig::default()
        },
    );
    registry.discover(Duration::from_secs(60)).await.unwrap();
    wait_until(|| {
        let world = registry.world();
        world.borrow().lights.len() == light_count
            && world
                .borrow()
                .lights
                .iter()
                .all(|light| light.conn == ConnState::Connected)
    })
    .await;
    (sim, registry, directory)
}

fn hsi(hue: u16) -> Mode {
    Mode::Hsi {
        hue: Hue::new(hue).unwrap(),
        sat: Percent::new(100).unwrap(),
        bri: Percent::new(100).unwrap(),
    }
}

fn test_animation(keyframes: Vec<Keyframe>, slot_count: u8) -> Animation {
    Animation {
        id: AnimationId::parse("test-animation").unwrap(),
        name: "Test Animation".to_owned(),
        description: "Registry playback fixture".to_owned(),
        loop_default: false,
        slot_count,
        keyframes,
    }
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    for _ in 0..200 {
        if condition() {
            return;
        }
        tokio::task::yield_now().await;
    }
    assert!(condition(), "condition did not become true");
}

async fn set_mode_with_time(
    registry: &RegistryHandle,
    selector: Selector,
    mode: Mode,
) -> Vec<PerLightResult> {
    let registry = registry.clone();
    let operation = tokio::spawn(async move { registry.set_mode(selector, mode, true).await });
    tokio::task::yield_now().await;
    advance(Duration::from_secs(1)).await;
    operation.await.unwrap().unwrap()
}

#[tokio::test(start_paused = true)]
async fn discovers_connects_and_resolves_capabilities() {
    let sim = SimTransport::new(SimConfig {
        lights: four_lights(),
        write_latency: Duration::ZERO,
        fail_every_nth_write: None,
    });
    let registry = RegistryHandle::spawn(sim);
    let mut events = registry.events();
    registry.discover(Duration::from_secs(60)).await.unwrap();
    wait_until(|| {
        let world = registry.world();
        world.borrow().lights.len() == 4
            && world
                .borrow()
                .lights
                .iter()
                .all(|light| light.conn == ConnState::Connected)
    })
    .await;

    let world = registry.world();
    let world = world.borrow();
    let rgb = world
        .lights
        .iter()
        .find(|light| light.id == LightId::sim("rgb660"))
        .unwrap();
    assert!(rgb.caps.rgb);
    let snl = world
        .lights
        .iter()
        .find(|light| light.id == LightId::sim("snl660"))
        .unwrap();
    assert!(!snl.caps.rgb);
    assert!(snl.caps.cct_split_packets);
    drop(world);

    let mut discovered_events = 0;
    while let Ok(event) = events.try_recv() {
        if matches!(event.event, Event::Light { light } if light.conn == ConnState::Discovered) {
            discovered_events += 1;
        }
    }
    assert_eq!(discovered_events, 4);
    registry.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn applies_cct_to_all_connected_lights() {
    let (sim, registry) = setup(four_lights()).await;
    let mode = Mode::Cct {
        temp: Kelvin::new(4200).unwrap(),
        bri: Percent::new(70).unwrap(),
    };
    let results = set_mode_with_time(&registry, Selector::All, mode).await;
    assert_eq!(results.len(), 4);
    assert!(results.iter().all(|result| matches!(
        result,
        PerLightResult::Applied { mode: applied, .. } if *applied == mode
    )));

    for id in ["rgb660", "rgb176", "sl80"] {
        assert!(matches!(
            sim.light(&LightId::sim(id)).timeline().as_slice(),
            [(
                _,
                Decoded::Cct {
                    temp_hk: 42,
                    bri: 70
                }
            )]
        ));
    }
    assert!(matches!(
        sim.light(&LightId::sim("snl660")).timeline().as_slice(),
        [(_, Decoded::BriOnly(70)), (_, Decoded::TempOnly(42))]
    ));
    assert!(
        registry
            .world()
            .borrow()
            .lights
            .iter()
            .all(|light| light.desired == Some(mode) && light.confirmed == Some(mode))
    );
    registry.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn converts_hsi_to_cct_on_bicolor_lights() {
    let (sim, registry) = setup(four_lights()).await;
    let id = LightId::sim("snl660");
    let mode = Mode::Hsi {
        hue: Hue::new(120).unwrap(),
        sat: Percent::new(80).unwrap(),
        bri: Percent::new(60).unwrap(),
    };
    let results = registry
        .set_mode(
            Selector::Ids {
                ids: vec![id.clone()],
            },
            mode,
            true,
        )
        .await
        .unwrap();
    // Green (hue 120) lands mid-ramp in the 3200 to 5600 K range: 4000 K.
    let expected = Mode::Cct {
        temp: Kelvin::new(4000).unwrap(),
        bri: Percent::new(60).unwrap(),
    };
    assert_eq!(
        results,
        vec![PerLightResult::Adapted {
            id: id.clone(),
            requested: mode,
            applied: expected,
        }]
    );
    // The SNL660 needs split packets: brightness then temperature.
    let timeline = sim.light(&id).timeline();
    assert_eq!(
        timeline.iter().map(|(_, d)| *d).collect::<Vec<_>>(),
        vec![Decoded::BriOnly(60), Decoded::TempOnly(40)]
    );
    registry.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn adapts_cct_to_the_device_range() {
    let (sim, registry) = setup(four_lights()).await;
    let id = LightId::sim("rgb660");
    let requested = Mode::Cct {
        temp: Kelvin::new(9000).unwrap(),
        bri: Percent::new(50).unwrap(),
    };
    let applied = Mode::Cct {
        temp: Kelvin::new(5600).unwrap(),
        bri: Percent::new(50).unwrap(),
    };
    let results = set_mode_with_time(
        &registry,
        Selector::Ids {
            ids: vec![id.clone()],
        },
        requested,
    )
    .await;
    assert_eq!(
        results,
        vec![PerLightResult::Adapted {
            id: id.clone(),
            requested,
            applied,
        }]
    );
    assert!(matches!(
        sim.light(&id).timeline().as_slice(),
        [(
            _,
            Decoded::Cct {
                temp_hk: 56,
                bri: 50
            }
        )]
    ));
    registry.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn desired_watch_coalesces_bursts() {
    let light = spec("rgb660", "NEEWER-RGB660 PRO");
    let id = light.id.clone();
    let sim = SimTransport::new(SimConfig {
        lights: vec![light],
        write_latency: Duration::ZERO,
        fail_every_nth_write: None,
    });
    let registry = RegistryHandle::spawn_with_config(
        sim.clone(),
        RegistryConfig {
            min_write_interval: Duration::from_millis(50),
            ..RegistryConfig::default()
        },
    );
    registry.discover(Duration::from_secs(60)).await.unwrap();
    wait_until(|| {
        registry
            .world()
            .borrow()
            .lights
            .first()
            .is_some_and(|light| light.conn == ConnState::Connected)
    })
    .await;

    for hue in 0..10 {
        let result = registry
            .set_mode(
                Selector::All,
                Mode::Hsi {
                    hue: Hue::new(hue).unwrap(),
                    sat: Percent::new(100).unwrap(),
                    bri: Percent::new(50).unwrap(),
                },
                false,
            )
            .await
            .unwrap();
        assert!(result.is_empty());
    }
    advance(Duration::from_secs(1)).await;
    wait_until(|| {
        matches!(
            sim.light(&id).last(),
            Some((_, Decoded::Hsi { hue: 9, .. }))
        )
    })
    .await;
    let timeline = sim.light(&id).timeline();
    assert!(timeline.len() < 10, "timeline was {timeline:?}");
    assert!(matches!(
        timeline.last(),
        Some((_, Decoded::Hsi { hue: 9, .. }))
    ));
    registry.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn reconnects_and_replays_strictly_sequenced_events() {
    let (sim, registry) = setup(four_lights()).await;
    let id = LightId::sim("rgb660");
    let mut events = registry.events();
    sim.light(&id).force_disconnect();
    wait_until(|| {
        registry
            .world()
            .borrow()
            .lights
            .iter()
            .find(|light| light.id == id)
            .is_some_and(|light| matches!(light.conn, ConnState::Reconnecting { .. }))
    })
    .await;
    let mid = registry.world().borrow().seq;
    advance(Duration::from_millis(250)).await;
    wait_until(|| {
        registry
            .world()
            .borrow()
            .lights
            .iter()
            .find(|light| light.id == id)
            .is_some_and(|light| light.conn == ConnState::Connected)
    })
    .await;

    let replay = registry.events_since(mid).unwrap();
    assert!(!replay.is_empty());
    assert_eq!(replay[0].seq, mid + 1);
    assert_eq!(replay.last().unwrap().seq, registry.world().borrow().seq);
    let mut observed = Vec::new();
    while let Ok(event) = events.try_recv() {
        observed.push(event.seq);
    }
    assert!(observed.windows(2).all(|pair| pair[0] < pair[1]));
    registry.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn shutdown_disconnects_every_link() {
    let (sim, registry) = setup(four_lights()).await;
    registry.shutdown().await;
    for id in ["rgb660", "snl660", "rgb176", "sl80"] {
        assert!(!sim.light(&LightId::sim(id)).is_connected());
    }
}

/// A wait=true command must survive a reconnect: the rewrite after the link
/// comes back has to replay the acked mode, not an older desired value.
#[tokio::test(start_paused = true)]
async fn reconnect_replays_the_acked_mode_not_a_stale_one() {
    let (sim, registry) = setup(four_lights()).await;
    let id = LightId::sim("rgb660");
    let handle = sim.light(&id);
    let selector = Selector::Ids {
        ids: vec![id.clone()],
    };
    let hsi = |hue: u16| Mode::Hsi {
        hue: Hue::new(hue).unwrap(),
        sat: Percent::new(100).unwrap(),
        bri: Percent::new(100).unwrap(),
    };

    registry
        .set_mode(selector.clone(), hsi(10), false)
        .await
        .unwrap();
    wait_until(|| {
        handle
            .last()
            .is_some_and(|(_, d)| matches!(d, Decoded::Hsi { hue: 10, .. }))
    })
    .await;

    let results = set_mode_with_time(&registry, selector, hsi(200)).await;
    assert!(matches!(results[0], PerLightResult::Applied { .. }));
    let writes_after_ack = handle.timeline().len();

    // The acked write must not be duplicated by the desired-watch echo.
    advance(Duration::from_secs(1)).await;
    assert_eq!(handle.timeline().len(), writes_after_ack);

    handle.force_disconnect();
    wait_until(|| {
        registry
            .world()
            .borrow()
            .lights
            .iter()
            .find(|light| light.id == id)
            .is_some_and(|light| matches!(light.conn, ConnState::Reconnecting { .. }))
    })
    .await;
    advance(Duration::from_millis(250)).await;
    wait_until(|| handle.timeline().len() > writes_after_ack).await;

    let (_, last) = handle.last().unwrap();
    let timeline: Vec<_> = handle.timeline().into_iter().map(|(_, d)| d).collect();
    assert!(
        matches!(last, Decoded::Hsi { hue: 200, .. }),
        "reconnect replayed a stale mode: {last:?}; full timeline: {timeline:?}"
    );
    registry.shutdown().await;
}

/// A light that is unreachable at discovery time must recover on its own via
/// the same backoff used for dropped links, without a manual reconnect.
#[tokio::test(start_paused = true)]
async fn initial_connect_failure_retries_until_connected() {
    let mut lights = four_lights();
    lights[0].connect_failures = 2;
    let sim = SimTransport::new(SimConfig {
        lights,
        write_latency: Duration::ZERO,
        fail_every_nth_write: None,
    });
    let registry = RegistryHandle::spawn(sim.clone());
    registry.discover(Duration::from_secs(60)).await.unwrap();
    let id = LightId::sim("rgb660");

    let conn_of = |registry: &RegistryHandle| {
        registry
            .world()
            .borrow()
            .lights
            .iter()
            .find(|light| light.id == id)
            .map(|light| light.conn.clone())
    };

    wait_until(|| {
        matches!(
            conn_of(&registry),
            Some(ConnState::Reconnecting { attempt: 1 })
        )
    })
    .await;
    advance(Duration::from_millis(250)).await;
    wait_until(|| {
        matches!(
            conn_of(&registry),
            Some(ConnState::Reconnecting { attempt: 2 })
        )
    })
    .await;
    advance(Duration::from_millis(500)).await;
    wait_until(|| matches!(conn_of(&registry), Some(ConnState::Connected))).await;
    assert!(sim.light(&id).is_connected());
    registry.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn animation_playback_updates_all_lights_and_world_state() {
    let animation = test_animation(
        vec![
            Keyframe {
                hold_ms: 100,
                fade_ms: 0,
                lights: BTreeMap::from([(AnimTarget::All, hsi(0))]),
            },
            Keyframe {
                hold_ms: 50,
                fade_ms: 400,
                lights: BTreeMap::from([(AnimTarget::All, hsi(100))]),
            },
        ],
        0,
    );
    let lights = (1..=4)
        .map(|index| spec(&index.to_string(), "NEEWER-RGB660 PRO"))
        .collect();
    let (sim, registry, _directory) = setup_animation(lights, &animation).await;
    let mut events = registry.events();
    let status = registry
        .play(
            animation.id.clone(),
            PlaybackOptions {
                revert_on_finish: false,
                ..PlaybackOptions::default()
            },
            TargetBinding::default(),
        )
        .await
        .unwrap();
    assert_eq!(registry.world().borrow().playback.as_ref(), Some(&status));

    wait_until(|| sim.light(&LightId::sim("1")).timeline().len() == 1).await;
    advance(Duration::from_millis(100)).await;
    wait_until(|| sim.light(&LightId::sim("1")).timeline().len() == 2).await;
    advance(Duration::from_millis(200)).await;
    wait_until(|| sim.light(&LightId::sim("1")).timeline().len() == 3).await;
    advance(Duration::from_millis(250)).await;
    wait_until(|| registry.world().borrow().playback.is_none()).await;

    for index in 1..=4 {
        let hues = sim
            .light(&LightId::sim(&index.to_string()))
            .timeline()
            .into_iter()
            .map(|(_, mode)| match mode {
                Decoded::Hsi { hue, .. } => hue,
                other => panic!("unexpected animation write: {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(hues, [0, 50, 100]);
    }
    let playback_events = std::iter::from_fn(|| events.try_recv().ok())
        .filter(|event| matches!(event.event, Event::Playback { .. }))
        .collect::<Vec<_>>();
    assert!(matches!(
        playback_events.as_slice(),
        [
            lumiere_proto::SeqEvent {
                event: Event::Playback { playback: Some(_) },
                ..
            },
            lumiere_proto::SeqEvent {
                event: Event::Playback { playback: None },
                ..
            }
        ]
    ));
    registry.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn stop_mid_fade_reverts_and_prevents_late_frames() {
    let animation = test_animation(
        vec![
            Keyframe {
                hold_ms: 50,
                fade_ms: 0,
                lights: BTreeMap::from([(AnimTarget::All, hsi(0))]),
            },
            Keyframe {
                hold_ms: 50,
                fade_ms: 1_000,
                lights: BTreeMap::from([(AnimTarget::All, hsi(100))]),
            },
        ],
        0,
    );
    let id = LightId::sim("one");
    let (sim, registry, _directory) =
        setup_animation(vec![spec("one", "NEEWER-RGB660 PRO")], &animation).await;
    registry
        .set_mode(Selector::All, hsi(200), false)
        .await
        .unwrap();
    wait_until(|| sim.light(&id).timeline().len() == 1).await;
    advance(Duration::from_millis(50)).await;
    registry
        .play(
            animation.id.clone(),
            PlaybackOptions::default(),
            TargetBinding::default(),
        )
        .await
        .unwrap();
    wait_until(|| sim.light(&id).timeline().len() == 2).await;
    advance(Duration::from_millis(250)).await;
    wait_until(|| sim.light(&id).timeline().len() >= 3).await;
    let stopped_at = tokio::time::Instant::now();
    assert!(registry.stop_playback().await.unwrap());
    advance(Duration::from_millis(200)).await;
    wait_until(|| {
        matches!(
            sim.light(&id).last(),
            Some((_, Decoded::Hsi { hue: 200, .. }))
        )
    })
    .await;
    let writes_after_revert = sim.light(&id).timeline().len();
    assert!(
        sim.light(&id)
            .timeline()
            .iter()
            .all(|(at, _)| *at <= stopped_at + Duration::from_millis(200))
    );
    advance(Duration::from_millis(1_800)).await;
    assert_eq!(sim.light(&id).timeline().len(), writes_after_revert);
    assert!(!registry.stop_playback().await.unwrap());
    registry.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn manual_mode_preempts_playback_once_and_wins() {
    let animation = test_animation(
        vec![
            Keyframe {
                hold_ms: 50,
                fade_ms: 0,
                lights: BTreeMap::from([(AnimTarget::All, hsi(0))]),
            },
            Keyframe {
                hold_ms: 50,
                fade_ms: 2_000,
                lights: BTreeMap::from([(AnimTarget::All, hsi(100))]),
            },
        ],
        0,
    );
    let id = LightId::sim("one");
    let (sim, registry, _directory) =
        setup_animation(vec![spec("one", "NEEWER-RGB660 PRO")], &animation).await;
    registry
        .set_mode(Selector::All, hsi(200), false)
        .await
        .unwrap();
    wait_until(|| sim.light(&id).timeline().len() == 1).await;
    advance(Duration::from_millis(50)).await;
    let mut events = registry.events();
    registry
        .play(
            animation.id.clone(),
            PlaybackOptions::default(),
            TargetBinding::default(),
        )
        .await
        .unwrap();
    wait_until(|| sim.light(&id).timeline().len() == 2).await;
    registry
        .set_mode(Selector::All, hsi(300), false)
        .await
        .unwrap();
    advance(Duration::from_secs(1)).await;
    wait_until(|| {
        matches!(
            sim.light(&id).last(),
            Some((_, Decoded::Hsi { hue: 300, .. }))
        )
    })
    .await;
    assert!(registry.world().borrow().playback.is_none());
    let stopped = std::iter::from_fn(|| events.try_recv().ok())
        .filter(|event| matches!(event.event, Event::Playback { playback: None }))
        .count();
    assert_eq!(stopped, 1);
    registry.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn slot_bindings_are_specific_and_fallback_wraps_round_robin() {
    let slot = |number| AnimTarget::Slot(NonZeroU8::new(number).unwrap());
    let animation = test_animation(
        vec![Keyframe {
            hold_ms: 50,
            fade_ms: 0,
            lights: BTreeMap::from([(slot(1), hsi(10)), (slot(2), hsi(20)), (slot(5), hsi(50))]),
        }],
        5,
    );
    let (sim, registry, _directory) = setup_animation(
        vec![
            spec("one", "NEEWER-RGB660 PRO"),
            spec("two", "NEEWER-RGB660 PRO"),
            spec("three", "NEEWER-RGB660 PRO"),
        ],
        &animation,
    )
    .await;
    registry
        .play(
            animation.id.clone(),
            PlaybackOptions {
                revert_on_finish: false,
                ..PlaybackOptions::default()
            },
            TargetBinding {
                all: Selector::All,
                slots: vec![LightId::sim("two"), LightId::sim("three")],
            },
        )
        .await
        .unwrap();
    advance(Duration::from_millis(100)).await;
    wait_until(|| registry.world().borrow().playback.is_none()).await;
    assert!(sim.light(&LightId::sim("one")).timeline().is_empty());
    assert!(matches!(
        sim.light(&LightId::sim("two")).last(),
        Some((_, Decoded::Hsi { hue: 50, .. }))
    ));
    assert!(matches!(
        sim.light(&LightId::sim("three")).last(),
        Some((_, Decoded::Hsi { hue: 20, .. }))
    ));
    registry.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn preset_capture_recall_rename_and_delete_round_trip() {
    let (sim, registry) = setup(vec![spec("one", "NEEWER-RGB660 PRO")]).await;
    let id = LightId::sim("one");
    let captured_mode = hsi(42);
    set_mode_with_time(&registry, Selector::All, captured_mode).await;

    let preset = registry
        .save_preset("My Look".to_owned(), Selector::All)
        .await
        .unwrap();
    assert_eq!(preset.id.as_str(), "my-look");
    assert_eq!(preset.entries.len(), 1);
    set_mode_with_time(&registry, Selector::All, hsi(180)).await;

    let recall_registry = registry.clone();
    let preset_id = preset.id.clone();
    let recall = tokio::spawn(async move { recall_registry.recall_preset(preset_id, true).await });
    tokio::task::yield_now().await;
    advance(Duration::from_secs(1)).await;
    let results = recall.await.unwrap().unwrap();
    assert!(matches!(
        results.as_slice(),
        [PerLightResult::Applied { id: result_id, mode }] if result_id == &id && *mode == captured_mode
    ));
    assert!(matches!(
        sim.light(&id).last(),
        Some((_, Decoded::Hsi { hue: 42, .. }))
    ));

    registry
        .rename_preset(preset.id.clone(), "Renamed".to_owned())
        .await
        .unwrap();
    assert_eq!(registry.list_presets().await.unwrap()[0].name, "Renamed");
    registry.delete_preset(preset.id).await.unwrap();
    assert!(registry.list_presets().await.unwrap().is_empty());
    registry.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn preset_recall_preempts_playback_once() {
    let animation = test_animation(
        vec![
            Keyframe {
                hold_ms: 50,
                fade_ms: 0,
                lights: BTreeMap::from([(AnimTarget::All, hsi(0))]),
            },
            Keyframe {
                hold_ms: 50,
                fade_ms: 2_000,
                lights: BTreeMap::from([(AnimTarget::All, hsi(100))]),
            },
        ],
        0,
    );
    let id = LightId::sim("one");
    let (sim, registry, _directory) =
        setup_animation(vec![spec("one", "NEEWER-RGB660 PRO")], &animation).await;
    set_mode_with_time(&registry, Selector::All, hsi(250)).await;
    let preset = registry
        .save_preset("Saved".to_owned(), Selector::All)
        .await
        .unwrap();
    let mut events = registry.events();
    registry
        .play(
            animation.id,
            PlaybackOptions::default(),
            TargetBinding::default(),
        )
        .await
        .unwrap();
    wait_until(|| sim.light(&id).timeline().len() >= 2).await;
    registry.recall_preset(preset.id, false).await.unwrap();
    advance(Duration::from_secs(1)).await;
    wait_until(|| {
        matches!(
            sim.light(&id).last(),
            Some((_, Decoded::Hsi { hue: 250, .. }))
        )
    })
    .await;
    let stopped = std::iter::from_fn(|| events.try_recv().ok())
        .filter(|event| matches!(event.event, Event::Playback { playback: None }))
        .count();
    assert_eq!(stopped, 1);
    registry.shutdown().await;
}
