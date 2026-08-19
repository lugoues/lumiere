use std::time::Duration;

use lumiere_core::wire::Decoded;
use lumiere_daemon::{RegistryConfig, RegistryHandle};
use lumiere_proto::{
    ConnState, Event, Hue, Kelvin, LightId, Mode, PerLightResult, Percent, Selector, SkipReason,
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
async fn rejects_unsupported_hsi_without_writing() {
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
    assert_eq!(
        results,
        vec![PerLightResult::Skipped {
            id: id.clone(),
            reason: SkipReason::UnsupportedMode,
        }]
    );
    assert!(sim.light(&id).timeline().is_empty());
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
