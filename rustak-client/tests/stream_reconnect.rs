//! Reconnection, over a real socket.
//!
//! The duplex tests in `stream_client.rs` cannot lose a connection — dropping a
//! `DuplexStream` ends the pairing for good — so this one uses a real listener
//! that accepts, says something, and hangs up. That is the sequence a server
//! restart produces, and the one the on-connect hook exists for: a reconnected
//! client is a new subscription, and has to re-introduce itself.
//!
//! Plain TCP is what makes this a test rather than a fixture with a CA in it;
//! it is gated on the same `insecure-tcp` feature that lets the client dial one
//! at all.
#![cfg(feature = "insecure-tcp")]

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use rustak_client::stream::{ConnectHook, Endpoint, Reconnecting, StreamConfig};
use rustak_cot::codec::{EncodedEvent, Frame, Mode, TakCodec};
use rustak_cot::detail::Contact;
use rustak_cot::{Event, xml};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::codec::Framed;

fn sa(uid: &str, callsign: &str) -> Event {
    Event::builder("a-f-G-U-C", uid)
        .how("m-g")
        .point(51.5074, -0.1278)
        .typed(&Contact::new(callsign).with_endpoint("*:-1:stcp"))
        .build()
}

/// A server that greets each connection, reads one message from it, and hangs
/// up — twice.
async fn flaky_server(listener: TcpListener, seen: mpsc::UnboundedSender<String>) {
    for round in 0..2 {
        let Ok((socket, _)) = listener.accept().await else {
            return;
        };
        let mut framed = Framed::new(socket, TakCodec::new(Mode::Xml));

        framed
            .send(&EncodedEvent::new(sa(&format!("SERVER-{round}"), "SERVER")))
            .await
            .expect("the server can greet");

        if let Some(Ok(Frame::Xml(bytes))) = framed.next().await {
            let event = xml::parse(&bytes).expect("the client writes parseable XML");
            let _ = seen.send(event.uid);
        }

        drop(framed);
    }
}

#[tokio::test]
async fn a_dropped_connection_is_reopened_and_the_hook_runs_again() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a free port");
    let port = listener.local_addr().unwrap().port();
    let (seen, mut introductions) = mpsc::unbounded_channel();

    let server = tokio::spawn(flaky_server(listener, seen));

    let hook: ConnectHook = Arc::new(|mut stream| {
        Box::pin(async move {
            stream.send(sa("SERVICE-ADSB", "ADSB")).await?;
            stream.settle(Duration::from_millis(100)).await?;

            Ok(stream)
        })
    });

    let config = StreamConfig::new(Endpoint::new("127.0.0.1", port, false), "SERVICE-ADSB");
    let mut client = Reconnecting::new(config).with_hook(hook);

    // First connection: the greeting arrives, and the hook has introduced us.
    let first = client.next().await.expect("the first connection delivers");
    assert_eq!(first.uid, "SERVER-0");
    assert_eq!(
        client.attempts(),
        0,
        "a connection that is up has not failed at anything",
    );
    assert_eq!(client.connects(), 1, "and this process has connected once");
    assert!(client.is_connected());
    assert_eq!(
        client.stream().and_then(|stream| stream.server_version()),
        None,
        "this server never offers a protocol",
    );

    // The server hangs up; the client waits out its backoff and comes back.
    let second = tokio::time::timeout(Duration::from_secs(5), client.next())
        .await
        .expect("the client reconnects inside the backoff")
        .expect("and the second connection delivers");

    assert_eq!(second.uid, "SERVER-1");
    assert_eq!(
        client.attempts(),
        0,
        "the reconnect succeeded, so nothing is outstanding",
    );
    assert_eq!(
        client.connects(),
        2,
        "the lifetime figure is what says the stream has been up twice",
    );

    server.await.expect("the server finishes");

    assert_eq!(
        introductions.recv().await.as_deref(),
        Some("SERVICE-ADSB"),
        "the hook introduced us on the first connection",
    );
    assert_eq!(
        introductions.recv().await.as_deref(),
        Some("SERVICE-ADSB"),
        "and again on the second",
    );
}

#[tokio::test]
async fn a_server_that_is_not_there_is_retried_rather_than_reported() {
    // Nothing is listening on this port; the wrapper's job is to keep trying
    // quietly rather than to hand the caller an error it can only log.
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap().port()
    };

    let config = StreamConfig::new(Endpoint::new("127.0.0.1", port, false), "SERVICE-ADSB");
    let mut client = Reconnecting::new(config);

    let outcome = tokio::time::timeout(Duration::from_millis(500), client.next()).await;

    assert!(outcome.is_err(), "nothing is ever delivered");
    assert!(!client.is_connected());
    assert!(client.attempts() >= 1, "it did try");
    assert!(
        client.backoff() > rustak_client::stream::MIN_BACKOFF,
        "and backed off"
    );
}
