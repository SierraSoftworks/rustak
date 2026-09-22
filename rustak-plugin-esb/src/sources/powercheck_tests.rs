//! The PowerCheck source against a local mock; never against ESB.

use rstest::rstest;
use rustak_client::feed::Area;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::outage::OutageKind;

const LISTING: &str = include_str!("../../tests/fixtures/listing.json");
const DETAIL: &str = include_str!("../../tests/fixtures/detail.json");

fn feed(server: &MockServer, scope: Scope) -> PowerCheckFeed {
    PowerCheckFeed::open(
        &server.uri(),
        &Secret::new("test-key"),
        scope,
        DEFAULT_POLL,
        10,
    )
    .expect("it opens")
}

async fn serve(server: &MockServer, at: &str, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(response)
        .mount(server)
        .await;
}

async fn serving_the_fixtures() -> MockServer {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/outages"))
        .and(header(KEY_HEADER, "test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_string(LISTING))
        .mount(&server)
        .await;
    serve(
        &server,
        "/outages/2826455/",
        ResponseTemplate::new(200).set_body_string(DETAIL),
    )
    .await;
    serve(&server, "/outages/2826460/", ResponseTemplate::new(404)).await;

    server
}

fn find<'a>(outages: &'a [Outage], id: &str) -> &'a Outage {
    outages.iter().find(|outage| outage.id == id).expect(id)
}

#[tokio::test]
async fn listed_outages_are_answered_and_filled_in_from_their_details() {
    let server = serving_the_fixtures().await;
    let mut feed = feed(&server, Scope::default());

    let outages = feed
        .poll()
        .await
        .expect("the key was sent, so the list answers");

    assert_eq!(outages.len(), 2);

    let fault = find(&outages, "2826455");
    assert_eq!(fault.kind, OutageKind::Fault);
    assert_eq!(fault.location.as_deref(), Some("Carrigaline"));
    assert_eq!(fault.customers, Some(412));

    let purged = find(&outages, "2826460");
    assert_eq!(
        purged.location, None,
        "a 404 detail leaves what the list said"
    );
    assert!(feed.state().is_connected());
}

#[tokio::test]
async fn an_outage_outside_the_area_costs_esb_no_detail_request() {
    let server = serving_the_fixtures().await;
    Mock::given(method("GET"))
        .and(path("/outages/2826460/"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let cork = Area::Circle {
        lat: 51.9,
        lon: -8.47,
        radius_km: 50.0,
    };
    let mut feed = feed(&server, Scope::new(cork, OutageKind::ALL.to_vec()));

    let outages = feed.poll().await.expect("a poll");

    assert_eq!(outages.len(), 1);
    assert_eq!(outages[0].id, "2826455");
}

#[tokio::test]
async fn a_detail_is_not_asked_for_again_on_the_next_tick() {
    let server = MockServer::start().await;
    serve(
        &server,
        "/outages",
        ResponseTemplate::new(200).set_body_string(LISTING),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/outages/2826455/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(DETAIL))
        .expect(1)
        .mount(&server)
        .await;
    let mut feed = feed(&server, Scope::default());

    let _ = feed.poll().await.expect("the first tick");
    let again = feed.poll().await.expect("the second tick");

    assert_eq!(
        again.len(),
        2,
        "between requests, a poll answers what is known"
    );
}

#[tokio::test]
async fn a_rate_limited_list_holds_the_details_back_too() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/outages"))
        .respond_with(ResponseTemplate::new(200).set_body_string(LISTING))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    serve(
        &server,
        "/outages",
        ResponseTemplate::new(429).insert_header("retry-after", "600"),
    )
    .await;
    serve(
        &server,
        "/outages/2826455/",
        ResponseTemplate::new(200).set_body_string(DETAIL),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/outages/2826460/"))
        .respond_with(ResponseTemplate::new(404))
        .expect(0)
        .mount(&server)
        .await;
    let mut feed = PowerCheckFeed::open(
        &server.uri(),
        &Secret::new("test-key"),
        Scope::default(),
        DEFAULT_POLL,
        1,
    )
    .expect("it opens");
    let _ = feed
        .poll()
        .await
        .expect("the list, and the one detail a tick allows");

    feed.state.due_now();
    let held = feed.poll().await.expect("a 429 is not an error");

    assert_eq!(held.len(), 2, "and the second detail was not asked for");
}

#[tokio::test]
async fn a_detail_that_fails_is_not_asked_for_again_on_the_next_tick() {
    let server = MockServer::start().await;
    serve(
        &server,
        "/outages",
        ResponseTemplate::new(200).set_body_string(LISTING),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/outages/2826455/"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/outages/2826460/"))
        .respond_with(ResponseTemplate::new(404))
        .expect(0)
        .mount(&server)
        .await;
    let mut feed = feed(&server, Scope::default());

    let first = feed
        .poll()
        .await
        .expect("a failed detail is not a failed poll");
    let second = feed.poll().await.expect("the next tick");

    assert_eq!(
        (first.len(), second.len()),
        (2, 2),
        "the markers stay, bare"
    );
}

#[tokio::test]
async fn an_outage_whose_type_changes_has_its_detail_asked_for_again() {
    // `Restored` overwrites `Fault` in the list well before DETAIL_REFRESH
    // comes round, and the restore time is only in the detail.
    const FAULT: &str =
        r#"{"outageMessage": [{"i": 2826455, "t": "Fault", "p": {"c": "51.8139,-8.3986"}}]}"#;
    const RESTORED: &str =
        r#"{"outageMessage": [{"i": 2826455, "t": "Restored", "p": {"c": "51.8139,-8.3986"}}]}"#;
    let restored_detail = DETAIL
        .replace("\"outageType\": \"Fault\"", "\"outageType\": \"Restored\"")
        .replace(
            "\"restoreTime\": \"\"",
            "\"restoreTime\": \"22/09/2026 18:10\"",
        );

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/outages"))
        .respond_with(ResponseTemplate::new(200).set_body_string(FAULT))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    serve(
        &server,
        "/outages",
        ResponseTemplate::new(200).set_body_string(RESTORED),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/outages/2826455/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(DETAIL))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    serve(
        &server,
        "/outages/2826455/",
        ResponseTemplate::new(200).set_body_string(restored_detail),
    )
    .await;
    let mut feed = feed(&server, Scope::default());
    let _ = feed.poll().await.expect("the fault, with its detail");

    feed.state.due_now();
    let outages = feed.poll().await.expect("the restoration");

    let restored = find(&outages, "2826455");
    assert_eq!(restored.kind, OutageKind::Restored);
    assert_eq!(restored.restored_at, "2026-09-22T17:10:00Z".parse().ok());
    assert!(restored.is_final(), "and it will not be asked about again");
}

#[tokio::test]
async fn an_upstream_that_stops_answering_does_not_clear_the_map() {
    let server = serving_the_fixtures().await;
    let mut feed = feed(&server, Scope::default());
    let _ = feed.poll().await.expect("the list arrives");

    server.reset().await;
    serve(&server, "/outages", ResponseTemplate::new(503)).await;
    feed.state.due_now();

    assert!(feed.poll().await.is_err(), "the failure is reported once");
    assert!(feed.state().last_error().is_some());

    let held = feed
        .poll()
        .await
        .expect("and then the last list is answered");
    assert_eq!(held.len(), 2);
}

/// A server that lists the fixtures and answers the fault's detail with
/// `response`, exactly once; the planned outage's detail must never be asked
/// for, because whatever went wrong with the first holds the rest back.
async fn serving_one_detail(response: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    serve(
        &server,
        "/outages",
        ResponseTemplate::new(200).set_body_string(LISTING),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/outages/2826455/"))
        .respond_with(response)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/outages/2826460/"))
        .respond_with(ResponseTemplate::new(404))
        .expect(0)
        .mount(&server)
        .await;

    server
}

#[rstest]
#[case::rate_limited(ResponseTemplate::new(429).insert_header("retry-after", "600"))]
#[case::rate_limited_without_saying_for_how_long(ResponseTemplate::new(429))]
#[case::refused(ResponseTemplate::new(401))]
#[tokio::test]
async fn a_detail_esb_will_not_answer_holds_the_rest_back(#[case] response: ResponseTemplate) {
    let server = serving_one_detail(response).await;
    let mut feed = feed(&server, Scope::default());

    let first = feed.poll().await.expect("not a failed poll");
    let second = feed.poll().await.expect("nor is the next tick");

    assert_eq!(
        (first.len(), second.len()),
        (2, 2),
        "the markers stay, bare"
    );
}

#[tokio::test]
async fn a_detail_we_cannot_read_leaves_the_marker_as_the_list_had_it() {
    let server = MockServer::start().await;
    serve(
        &server,
        "/outages",
        ResponseTemplate::new(200).set_body_string(LISTING),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/outages/2826455/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>maintenance</html>"))
        .expect(1)
        .mount(&server)
        .await;
    serve(&server, "/outages/2826460/", ResponseTemplate::new(404)).await;
    let mut feed = feed(&server, Scope::default());

    let _ = feed.poll().await.expect("a poll");
    let outages = feed
        .poll()
        .await
        .expect("and it is not asked for again at once");

    assert_eq!(find(&outages, "2826455").location, None);
}

#[rstest]
#[case::no_such_endpoint(ResponseTemplate::new(404))]
#[case::not_a_list(ResponseTemplate::new(200).set_body_string("<html>maintenance</html>"))]
#[tokio::test]
async fn a_list_that_is_not_there_or_not_a_list_is_a_failure_that_says_so(
    #[case] response: ResponseTemplate,
) {
    let server = MockServer::start().await;
    serve(&server, "/outages", response).await;
    let mut feed = feed(&server, Scope::default());

    let err = feed.poll().await.expect_err("nothing to put on a map");

    assert!(err.to_string().contains("list of outages"), "{err}");
    assert!(feed.state().last_error().is_some());
    assert!(!feed.state().is_connected());
}

#[tokio::test]
async fn an_upstream_that_has_been_gone_for_hours_does_clear_the_map() {
    let server = serving_the_fixtures().await;
    let mut feed = feed(&server, Scope::default());
    let _ = feed.poll().await.expect("the list arrives");

    server.reset().await;
    serve(&server, "/outages", ResponseTemplate::new(503)).await;
    feed.state.due_now();
    let _ = feed.poll().await.expect_err("the upstream has gone");
    feed.state
        .answered_at(Utc::now() - chrono::Duration::hours(3));

    let released = feed.poll().await.expect("not an error, just nothing known");

    assert!(
        released.is_empty(),
        "two hours is as long as a guess is held"
    );
}

#[tokio::test]
async fn a_rejected_key_stops_the_source_rather_than_hammering_esb() {
    let server = MockServer::start().await;
    serve(&server, "/outages", ResponseTemplate::new(401)).await;
    let mut feed = feed(&server, Scope::default());

    for _ in 0..DENIED_LIMIT {
        feed.state.due_now();
        let _ = feed.poll().await;
    }

    assert!(feed.stopped());
    assert!(
        feed.state()
            .last_error()
            .is_some_and(|error| error.contains("key")),
        "{:?}",
        feed.state().last_error(),
    );
    assert_eq!(
        server.received_requests().await.map(|all| all.len()),
        Some(3)
    );
}

#[tokio::test]
async fn being_rate_limited_is_waited_out_and_is_not_a_failure() {
    let server = MockServer::start().await;
    serve(
        &server,
        "/outages",
        ResponseTemplate::new(429).insert_header("retry-after", "600"),
    )
    .await;
    let mut feed = feed(&server, Scope::default());

    let outages = feed.poll().await.expect("not an error");

    assert!(outages.is_empty());
    assert_eq!(feed.state().last_error(), None);
    assert!(!feed.state().ready(), "ESB asked for ten minutes");
}

#[rstest]
#[case::empty(" ")]
#[case::not_a_header("two\nlines")]
fn a_key_that_cannot_work_is_refused_with_where_to_find_one(#[case] key: &str) {
    let err = PowerCheckFeed::open(
        DEFAULT_BASE_URL,
        &Secret::new(key),
        Scope::default(),
        DEFAULT_POLL,
        10,
    )
    .expect_err("no key, no source");

    assert!(
        format!("{err:?}").contains("powercheck.esbnetworks.ie"),
        "{err:?}"
    );
}

#[test]
fn a_poll_faster_than_the_floor_is_slowed_to_it() {
    let feed = PowerCheckFeed::open(
        DEFAULT_BASE_URL,
        &Secret::new("test-key"),
        Scope::default(),
        Duration::from_secs(5),
        10,
    )
    .expect("it opens");

    assert_eq!(feed.state().interval(), POLL_FLOOR);
}
