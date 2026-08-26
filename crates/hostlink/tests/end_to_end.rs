//! Exercises the real socket path: upgrade, version and token checks, and a
//! full request/response round trip through `pump`.

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use hostlink::server::{Config, Identity, Server};
use serde_json::Value;
use tokio_tungstenite::tungstenite;

fn config(label: &str) -> Config {
    let dir = std::env::temp_dir().join(format!("hostlink-e2e-{}-{label}", std::process::id()));
    Config {
        session_dir: Some(dir),
        ..Config::loopback(Identity::new("figma", label))
    }
}

async fn connect(
    port: u16,
    query: &str,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tungstenite::Error,
> {
    let (stream, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/link?{query}")).await?;
    Ok(stream)
}

#[tokio::test]
async fn request_and_response_round_trip_over_a_real_socket() {
    let server = Server::start(config("round-trip")).await.expect("start");
    let mut host = connect(server.port, "v=1").await.expect("connect");

    // Give the upgrade a moment to register the outbound channel.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(server.link.is_connected().await);

    let link = server.link.clone();
    let caller = tokio::spawn(async move {
        link.request("figma/getSelection", None, Duration::from_secs(5))
            .await
    });

    let raw = host.next().await.expect("frame").expect("ok");
    let req: Value = serde_json::from_str(raw.to_text().unwrap()).unwrap();
    assert_eq!(req["method"], "figma/getSelection");
    assert_eq!(req["jsonrpc"], "2.0");
    let id = req["id"].as_u64().unwrap();

    host.send(tungstenite::Message::Text(
        format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"count":2}}}}"#).into(),
    ))
    .await
    .expect("send");

    let payload = caller.await.unwrap().expect("ok");
    assert_eq!(payload.get(), r#"{"count":2}"#);
}

#[tokio::test]
async fn progress_then_result_survives_a_short_deadline() {
    let server = Server::start(config("progress")).await.expect("start");
    let mut host = connect(server.port, "v=1").await.expect("connect");
    tokio::time::sleep(Duration::from_millis(50)).await;

    let link = server.link.clone();
    // A deadline shorter than the work, kept alive purely by progress.
    let caller =
        tokio::spawn(async move { link.request("slow", None, Duration::from_millis(400)).await });

    let raw = host.next().await.expect("frame").expect("ok");
    let id = serde_json::from_str::<Value>(raw.to_text().unwrap()).unwrap()["id"]
        .as_u64()
        .unwrap();

    for pct in [25, 50, 75] {
        tokio::time::sleep(Duration::from_millis(250)).await;
        host.send(tungstenite::Message::Text(
            format!(
                r#"{{"jsonrpc":"2.0","method":"$/progress","params":{{"id":{id},"pct":{pct}}}}}"#
            )
            .into(),
        ))
        .await
        .expect("progress");
    }

    host.send(tungstenite::Message::Text(
        format!(r#"{{"jsonrpc":"2.0","id":{id},"result":"finished"}}"#).into(),
    ))
    .await
    .expect("result");

    assert_eq!(caller.await.unwrap().expect("ok").get(), r#""finished""#);
}

#[tokio::test]
async fn dropping_the_host_fails_requests_in_flight() {
    let server = Server::start(config("drop")).await.expect("start");
    let mut host = connect(server.port, "v=1").await.expect("connect");
    tokio::time::sleep(Duration::from_millis(50)).await;

    let link = server.link.clone();
    let caller =
        tokio::spawn(async move { link.request("x", None, Duration::from_secs(30)).await });
    let _ = host.next().await;

    host.close(None).await.ok();
    drop(host);

    let err = caller.await.unwrap().unwrap_err();
    assert_eq!(err.code, hostlink::codes::DISCONNECTED);
}

#[tokio::test]
async fn an_unsupported_protocol_version_is_refused() {
    let server = Server::start(config("version")).await.expect("start");
    assert!(connect(server.port, "v=99").await.is_err());
    assert!(
        connect(server.port, "").await.is_err(),
        "missing ?v= must also be refused"
    );
}

#[tokio::test]
async fn a_token_is_enforced_when_one_is_configured() {
    let token = hostlink::generate_token();
    let cfg = Config {
        bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
        token: Some(token.clone()),
        ..config("token")
    };
    let server = Server::start(cfg).await.expect("start");

    assert!(
        connect(server.port, "v=1").await.is_err(),
        "no token must be refused"
    );
    assert!(
        connect(server.port, "v=1&token=wrong").await.is_err(),
        "bad token must be refused"
    );
    assert!(
        connect(server.port, &format!("v=1&token={token}"))
            .await
            .is_ok()
    );
}

/// §5: the newest connection wins, and the displaced one's work fails cleanly.
#[tokio::test]
async fn a_second_connection_replaces_the_first() {
    let server = Server::start(config("replace")).await.expect("start");
    let mut first = connect(server.port, "v=1").await.expect("first");
    tokio::time::sleep(Duration::from_millis(50)).await;

    let link = server.link.clone();
    let caller =
        tokio::spawn(async move { link.request("x", None, Duration::from_secs(30)).await });
    let _ = first.next().await;

    let _second = connect(server.port, "v=1").await.expect("second");
    tokio::time::sleep(Duration::from_millis(50)).await;

    let err = caller.await.unwrap().unwrap_err();
    assert_eq!(err.code, hostlink::codes::DISCONNECTED);
    assert!(
        server.link.is_connected().await,
        "the replacement stays connected"
    );
}
