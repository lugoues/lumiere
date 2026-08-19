use std::{path::PathBuf, time::Duration};

use futures::{SinkExt, StreamExt};
use lumiere_daemon::{
    RegistryConfig, RegistryHandle,
    api::{ApiState, router},
    config::Config,
    store,
};
use lumiere_proto::{
    ClientMsg, CommandResponse, ConnState, Kelvin, LightId, Mode, PerLightResult, Percent,
    Selector, ServerMsg, WS_PROTOCOL_VERSION, WorldSnapshot,
};
use lumiere_transport::sim::{SimConfig, SimLightSpec, SimTransport};
use reqwest::{Client, StatusCode};
use tempfile::TempDir;
use tokio::task::JoinHandle;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const TOKEN: &str = "test-token";

struct TestServer {
    base: String,
    registry: RegistryHandle,
    sim: SimTransport,
    server_task: JoinHandle<()>,
    store_task: JoinHandle<Result<(), store::StoreError>>,
    _data: TempDir,
}

impl TestServer {
    async fn stop(self) {
        self.server_task.abort();
        self.registry.shutdown().await;
        self.store_task.await.unwrap().unwrap();
    }
}

async fn spawn_test_server(store_dir: Option<PathBuf>) -> Option<TestServer> {
    let data = tempfile::tempdir().unwrap();
    let store_dir = store_dir.unwrap_or_else(|| data.path().to_path_buf());
    let stored = store::load(&store_dir).unwrap();
    let (store_updates, store_task) = store::spawn(store_dir.clone(), stored.clone());
    let sim = sim_transport();
    let registry = RegistryHandle::spawn_with_config(
        sim.clone(),
        RegistryConfig {
            labels: stored.labels,
            presets: stored.presets,
            store_updates: Some(store_updates),
            animations_dir: Some(
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/animations"),
            ),
            ..RegistryConfig::default()
        },
    );
    let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("SKIPPING API TEST: sandbox denies localhost binding");
            registry.shutdown().await;
            store_task.await.unwrap().unwrap();
            return None;
        }
        Err(error) => panic!("failed to bind test server: {error}"),
    };
    let address = listener.local_addr().unwrap();
    let config = Config::for_tests(TOKEN);
    let app = router(ApiState::new(registry.clone(), &config));
    let server_task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Some(TestServer {
        base: format!("http://{address}"),
        registry,
        sim,
        server_task,
        store_task,
        _data: data,
    })
}

fn sim_transport() -> SimTransport {
    let lights = [
        "NEEWER-RGB660 PRO",
        "NEEWER-SNL660",
        "NEEWER-RGB176",
        "NEEWER-SL80",
    ]
    .into_iter()
    .enumerate()
    .map(|(index, name)| SimLightSpec {
        id: LightId::sim(&(index + 1).to_string()),
        advertised_name: name.to_owned(),
        rssi: -40 - index as i16,
        connect_failures: 0,
    })
    .collect();
    SimTransport::new(SimConfig {
        lights,
        write_latency: Duration::ZERO,
        fail_every_nth_write: None,
    })
}

async fn scan_and_wait(client: &Client, server: &TestServer) -> WorldSnapshot {
    let response = client
        .post(format!("{}/api/v1/scan", server.base))
        .bearer_auth(TOKEN)
        .json(&serde_json::json!({"duration_ms": 500}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let world = client
                .get(format!("{}/api/v1/lights", server.base))
                .bearer_auth(TOKEN)
                .send()
                .await
                .unwrap()
                .json::<WorldSnapshot>()
                .await
                .unwrap();
            if world.lights.len() == 4
                && world
                    .lights
                    .iter()
                    .all(|light| light.conn == ConnState::Connected)
            {
                return world;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("simulated lights did not connect")
}

fn cct_command(wait: bool) -> serde_json::Value {
    serde_json::to_value(lumiere_proto::CommandRequest {
        selector: Selector::All,
        mode: Mode::Cct {
            temp: Kelvin::new(5600).unwrap(),
            bri: Percent::new(50).unwrap(),
        },
        wait,
    })
    .unwrap()
}

#[tokio::test]
async fn auth_scan_and_command_cover_the_sim_world() {
    let Some(server) = spawn_test_server(None).await else {
        return;
    };
    let client = Client::new();
    assert_eq!(
        client
            .get(format!("{}/healthz", server.base))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        client
            .get(format!("{}/api/v1/lights", server.base))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(format!("{}/api/v1/lights", server.base))
            .bearer_auth("wrong")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    scan_and_wait(&client, &server).await;
    let response = client
        .post(format!("{}/api/v1/command", server.base))
        .bearer_auth(TOKEN)
        .json(&cct_command(true))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = response.json::<CommandResponse>().await.unwrap();
    assert_eq!(response.results.len(), 4);
    assert!(response.results.iter().all(|result| matches!(
        result,
        PerLightResult::Applied { .. } | PerLightResult::Adapted { .. }
    )));
    for index in 1..=4 {
        assert!(
            !server
                .sim
                .light(&LightId::sim(&index.to_string()))
                .timeline()
                .is_empty()
        );
    }
    server.stop().await;
}

#[tokio::test]
async fn animation_list_play_and_stop_round_trip() {
    let Some(server) = spawn_test_server(None).await else {
        return;
    };
    let client = Client::new();
    scan_and_wait(&client, &server).await;
    let response = client
        .get(format!("{}/api/v1/animations", server.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let animations = response
        .json::<Vec<lumiere_proto::AnimationSummary>>()
        .await
        .unwrap();
    assert_eq!(animations.len(), 101);
    assert!(
        animations
            .iter()
            .any(|animation| animation.id.as_str() == "ambulance")
    );

    let response = client
        .get(format!("{}/api/v1/animations/ambulance", server.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .json::<lumiere_proto::Animation>()
            .await
            .unwrap()
            .id
            .as_str(),
        "ambulance"
    );

    let response = client
        .post(format!("{}/api/v1/animations/ambulance/play", server.base))
        .bearer_auth(TOKEN)
        .json(&serde_json::json!({
            "options": {"loop_override": false, "revert_on_finish": false}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let status = response
        .json::<lumiere_proto::PlaybackStatus>()
        .await
        .unwrap();
    assert_eq!(status.animation.as_str(), "ambulance");

    let response = client
        .post(format!("{}/api/v1/playback/stop", server.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap(),
        serde_json::json!({"stopped": true})
    );
    server.stop().await;
}

#[tokio::test]
async fn labels_are_persisted_and_restored() {
    let shared_data = tempfile::tempdir().unwrap();
    let dir = shared_data.path().to_path_buf();
    let path = dir.join("lights.toml");
    let Some(first) = spawn_test_server(Some(dir.clone())).await else {
        return;
    };
    let client = Client::new();
    scan_and_wait(&client, &first).await;
    let response = client
        .patch(format!("{}/api/v1/lights/sim%3A1", first.base))
        .bearer_auth(TOKEN)
        .json(&serde_json::json!({"label": "Key"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    tokio::time::timeout(Duration::from_secs(2), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let persisted = store::load(&dir).unwrap();
    assert_eq!(
        persisted.labels.get(&LightId::sim("1")).map(String::as_str),
        Some("Key")
    );
    first.stop().await;

    let Some(second) = spawn_test_server(Some(dir)).await else {
        return;
    };
    let world = scan_and_wait(&client, &second).await;
    assert_eq!(
        world
            .lights
            .iter()
            .find(|light| light.id == LightId::sim("1"))
            .map(|light| light.label.as_str()),
        Some("Key")
    );
    second.stop().await;
}

#[tokio::test]
async fn preset_factory_capture_recall_rename_and_delete_round_trip() {
    let Some(server) = spawn_test_server(None).await else {
        return;
    };
    let client = Client::new();
    let initial = client
        .get(format!("{}/api/v1/presets", server.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(initial.status(), StatusCode::OK);
    let factory = initial.json::<Vec<lumiere_proto::Preset>>().await.unwrap();
    assert_eq!(factory.len(), 8);
    assert_eq!(factory[0].name, "Daylight");
    assert!(server._data.path().join("presets.toml").exists());

    scan_and_wait(&client, &server).await;
    let response = client
        .post(format!("{}/api/v1/command", server.base))
        .bearer_auth(TOKEN)
        .json(&cct_command(true))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let captured = client
        .post(format!("{}/api/v1/presets", server.base))
        .bearer_auth(TOKEN)
        .json(&serde_json::json!({"name": "Studio Look", "selector": {"kind": "all"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(captured.status(), StatusCode::CREATED);
    let captured = captured.json::<lumiere_proto::Preset>().await.unwrap();
    assert_eq!(captured.id.as_str(), "studio-look");
    assert_eq!(captured.entries.len(), 4);

    client
        .post(format!("{}/api/v1/command", server.base))
        .bearer_auth(TOKEN)
        .json(
            &serde_json::to_value(lumiere_proto::CommandRequest {
                selector: Selector::All,
                mode: Mode::Off,
                wait: true,
            })
            .unwrap(),
        )
        .send()
        .await
        .unwrap();
    let recalled = client
        .post(format!(
            "{}/api/v1/presets/{}/recall",
            server.base, captured.id
        ))
        .bearer_auth(TOKEN)
        .json(&serde_json::json!({"wait": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(recalled.status(), StatusCode::OK);
    assert_eq!(
        recalled
            .json::<CommandResponse>()
            .await
            .unwrap()
            .results
            .len(),
        4
    );

    let renamed = client
        .patch(format!("{}/api/v1/presets/{}", server.base, captured.id))
        .bearer_auth(TOKEN)
        .json(&serde_json::json!({"name": "Portrait"}))
        .send()
        .await
        .unwrap();
    assert_eq!(renamed.status(), StatusCode::OK);
    assert_eq!(
        renamed.json::<lumiere_proto::Preset>().await.unwrap().name,
        "Portrait"
    );
    let deleted = client
        .delete(format!("{}/api/v1/presets/{}", server.base, captured.id))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    server.stop().await;
}

#[tokio::test]
async fn captured_presets_persist_across_registries() {
    let shared_data = tempfile::tempdir().unwrap();
    let dir = shared_data.path().to_path_buf();
    let Some(first) = spawn_test_server(Some(dir.clone())).await else {
        return;
    };
    let client = Client::new();
    scan_and_wait(&client, &first).await;
    client
        .post(format!("{}/api/v1/command", first.base))
        .bearer_auth(TOKEN)
        .json(&cct_command(true))
        .send()
        .await
        .unwrap();
    let response = client
        .post(format!("{}/api/v1/presets", first.base))
        .bearer_auth(TOKEN)
        .json(&serde_json::json!({"name": "Persistent", "selector": {"kind": "all"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    first.stop().await;

    let Some(second) = spawn_test_server(Some(dir)).await else {
        return;
    };
    let presets = client
        .get(format!("{}/api/v1/presets", second.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json::<Vec<lumiere_proto::Preset>>()
        .await
        .unwrap();
    assert!(presets.iter().any(|preset| preset.name == "Persistent"));
    second.stop().await;
}

#[tokio::test]
async fn websocket_snapshot_events_ping_and_replay() {
    let Some(server) = spawn_test_server(None).await else {
        return;
    };
    let client = Client::new();
    scan_and_wait(&client, &server).await;
    let ticket = new_ticket(&client, &server).await;
    let (mut socket, _) = connect_async(server.base.replace("http://", "ws://") + "/api/v1/events")
        .await
        .unwrap();
    socket
        .send(json_message(&ClientMsg::Hello {
            protocol_version: WS_PROTOCOL_VERSION,
            ticket,
            last_seq: None,
        }))
        .await
        .unwrap();
    let welcome = next_server_message(&mut socket).await;
    let initial_seq = match welcome {
        ServerMsg::Welcome {
            seq,
            snapshot: Some(_),
            ..
        } => seq,
        message => panic!("unexpected message: {message:?}"),
    };

    client
        .post(format!("{}/api/v1/command", server.base))
        .bearer_auth(TOKEN)
        .json(&cct_command(false))
        .send()
        .await
        .unwrap();
    assert!(matches!(
        next_server_message(&mut socket).await,
        ServerMsg::Patch { events } if !events.is_empty()
    ));
    socket
        .send(json_message(&ClientMsg::Ping { nonce: 42 }))
        .await
        .unwrap();
    // Patches from the in-flight command may interleave before the Pong.
    let pong = loop {
        match next_server_message(&mut socket).await {
            ServerMsg::Patch { .. } => continue,
            message => break message,
        }
    };
    assert_eq!(pong, ServerMsg::Pong { nonce: 42 });
    socket.close(None).await.unwrap();

    let ticket = new_ticket(&client, &server).await;
    let (mut resumed, _) =
        connect_async(server.base.replace("http://", "ws://") + "/api/v1/events")
            .await
            .unwrap();
    resumed
        .send(json_message(&ClientMsg::Hello {
            protocol_version: WS_PROTOCOL_VERSION,
            ticket,
            last_seq: Some(initial_seq),
        }))
        .await
        .unwrap();
    assert!(matches!(
        next_server_message(&mut resumed).await,
        ServerMsg::Welcome { snapshot: None, .. }
    ));
    assert!(matches!(
        next_server_message(&mut resumed).await,
        ServerMsg::Patch { events } if !events.is_empty()
    ));
    server.stop().await;
}

#[tokio::test]
async fn websocket_rejects_a_bad_ticket() {
    let Some(server) = spawn_test_server(None).await else {
        return;
    };
    let (mut socket, _) = connect_async(server.base.replace("http://", "ws://") + "/api/v1/events")
        .await
        .unwrap();
    socket
        .send(json_message(&ClientMsg::Hello {
            protocol_version: WS_PROTOCOL_VERSION,
            ticket: "bad".to_owned(),
            last_seq: None,
        }))
        .await
        .unwrap();
    assert!(matches!(
        next_server_message(&mut socket).await,
        ServerMsg::Error { .. }
    ));
    assert!(matches!(
        socket.next().await,
        Some(Ok(Message::Close(_))) | None
    ));
    server.stop().await;
}

async fn new_ticket(client: &Client, server: &TestServer) -> String {
    client
        .post(format!("{}/api/v1/ws-ticket", server.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()["ticket"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn json_message(message: &ClientMsg) -> Message {
    Message::Text(serde_json::to_string(message).unwrap().into())
}

async fn next_server_message<S>(socket: &mut tokio_tungstenite::WebSocketStream<S>) -> ServerMsg
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        match socket.next().await.unwrap().unwrap() {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            Message::Ping(payload) => socket.send(Message::Pong(payload)).await.unwrap(),
            message => panic!("unexpected WebSocket frame: {message:?}"),
        }
    }
}

/// A client reconnecting after a daemon restart carries a last_seq from the
/// old process; the server must respond with a full snapshot, not an empty replay.
#[tokio::test]
async fn stale_future_last_seq_gets_a_snapshot() {
    let Some(server) = spawn_test_server(None).await else {
        return;
    };
    let client = reqwest::Client::new();
    let ticket = new_ticket(&client, &server).await;
    let (mut socket, _) = connect_async(server.base.replace("http://", "ws://") + "/api/v1/events")
        .await
        .unwrap();
    socket
        .send(json_message(&ClientMsg::Hello {
            protocol_version: WS_PROTOCOL_VERSION,
            ticket,
            last_seq: Some(1_000_000),
        }))
        .await
        .unwrap();
    match next_server_message(&mut socket).await {
        ServerMsg::Welcome { snapshot, .. } => {
            assert!(snapshot.is_some(), "future last_seq must force a snapshot")
        }
        message => panic!("unexpected message: {message:?}"),
    }
    socket.close(None).await.unwrap();
    server.stop().await;
}

/// --disable-authentication serves every route without a token, including the
/// WebSocket ticket handshake.
#[tokio::test]
async fn disabled_authentication_opens_every_route() {
    let data = tempfile::tempdir().unwrap();
    let stored = store::load(data.path()).unwrap();
    let (store_updates, store_task) = store::spawn(data.path().to_path_buf(), stored.clone());
    let registry = RegistryHandle::spawn_with_config(
        SimTransport::new(SimConfig {
            lights: vec![SimLightSpec {
                id: LightId::sim("1"),
                advertised_name: "NEEWER-SL80".to_owned(),
                rssi: -40,
                connect_failures: 0,
            }],
            write_latency: Duration::ZERO,
            fail_every_nth_write: None,
        }),
        RegistryConfig {
            labels: stored.labels.clone(),
            store_updates: Some(store_updates),
            ..RegistryConfig::default()
        },
    );
    let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("SKIPPING API TEST: sandbox denies localhost binding");
            registry.shutdown().await;
            store_task.await.unwrap().unwrap();
            return;
        }
        Err(error) => panic!("failed to bind test server: {error}"),
    };
    let base = format!("http://{}", listener.local_addr().unwrap());
    let config = Config::for_tests(TOKEN);
    let app = router(ApiState::new(registry.clone(), &config).without_authentication());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = Client::new();
    let response = client
        .get(format!("{base}/api/v1/lights"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let ticket = client
        .post(format!("{base}/api/v1/ws-ticket"))
        .send()
        .await
        .unwrap();
    assert_eq!(ticket.status(), StatusCode::OK);

    server.abort();
    registry.shutdown().await;
    store_task.await.unwrap().unwrap();
}
