use super::*;
use std::cell::Cell;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_test::task::spawn;
use tokio_test::{assert_pending, assert_ready};

thread_local! {
    // To avoid having to add #[tokio::test] to every test, we need a way to disable calls to tokio::time::sleep(),
    // which would panic without a tokio runtime
    pub static SLEEP_ENABLED: Cell<bool> = const { Cell::new(false) };
}

fn attribute() -> Tlv {
    Tlv {
        attribute_type: 0x8022,
        value: b"Ugh!".to_vec(),
    }
}

fn ip(port: u16) -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::BROADCAST, port))
}

#[test]
fn single_outgoing_request() {
    let (egress_sink, mut egress_source) = mpsc::channel(10);
    let (ingress_sink, ingress_source) = mpsc::channel(10);
    let (req_sender, _ind_sender, _ind_receiver, processor) = setup_transactions(
        MessageChannels {
            egress_sink,
            ingress_source,
        },
        1,
        NoRetransmissionsConstTimeout::new(Duration::from_secs(1)),
    );
    let mut runner_fut = spawn(processor.run());
    assert_pending!(runner_fut.poll());

    // when
    let mut request_fut = spawn(req_sender.send_request(ip(1234), 42u16, vec![attribute()]));
    assert_pending!(request_fut.poll());
    assert!(runner_fut.is_woken());
    assert_pending!(runner_fut.poll());

    // then
    let (request, addr) = egress_source.try_recv().unwrap();
    assert_eq!(addr, ip(1234));
    assert_eq!(request.header.class, Class::Request);
    assert_eq!(request.header.method, 42u16);
    assert_eq!(request.attributes, vec![attribute()]);

    // when
    let response = Message::response(
        request.header.method,
        request.header.transaction_id,
        vec![attribute()],
    );
    ingress_sink.try_send((response, ip(1234))).unwrap();

    // then
    assert!(runner_fut.is_woken());
    assert_pending!(runner_fut.poll());
    assert!(request_fut.is_woken());
    let result = assert_ready!(request_fut.poll());
    let response = result.unwrap();
    assert!(matches!(response, Response::Success(attributes) if attributes == vec![attribute()]));
}

#[test]
fn concurrent_outgoing_requests() {
    let (egress_sink, mut egress_source) = mpsc::channel(10);
    let (ingress_sink, ingress_source) = mpsc::channel(10);
    let (req_sender, _ind_sender, _ind_receiver, processor) = setup_transactions(
        MessageChannels {
            egress_sink,
            ingress_source,
        },
        2,
        NoRetransmissionsConstTimeout::new(Duration::from_secs(1)),
    );
    let mut runner_fut = spawn(processor.run());
    assert_pending!(runner_fut.poll());

    // when
    let mut request1_fut = spawn(req_sender.send_request(ip(1111), 42u16, vec![attribute()]));
    assert_pending!(request1_fut.poll());
    let mut request2_fut = spawn(req_sender.send_request(ip(2222), 43u16, vec![attribute()]));
    assert_pending!(request2_fut.poll());

    assert!(runner_fut.is_woken());
    assert_pending!(runner_fut.poll());

    assert!(request2_fut.is_woken());
    assert_pending!(request2_fut.poll());
    assert!(runner_fut.is_woken());
    assert_pending!(runner_fut.poll());

    // then
    let (request1, addr1) = egress_source.try_recv().unwrap();
    assert_eq!(addr1, ip(1111));
    assert_eq!(request1.header.class, Class::Request);
    assert_eq!(request1.header.method, 42u16);
    assert_eq!(request1.attributes, vec![attribute()]);

    let (request2, addr2) = egress_source.try_recv().unwrap();
    assert_eq!(addr2, ip(2222));
    assert_eq!(request2.header.class, Class::Request);
    assert_eq!(request2.header.method, 43u16);
    assert_eq!(request2.attributes, vec![attribute()]);

    // when
    let response2 = Message::response(
        request2.header.method,
        request2.header.transaction_id,
        vec![attribute()],
    );
    ingress_sink.try_send((response2, ip(2222))).unwrap();

    // then
    assert!(runner_fut.is_woken());
    assert_pending!(runner_fut.poll());
    assert!(request2_fut.is_woken());
    let result = assert_ready!(request2_fut.poll());
    let response2 = result.unwrap();
    assert!(matches!(response2, Response::Success(attributes) if attributes == vec![attribute()]));

    // when
    let response1 = Message::error(
        request1.header.method,
        request1.header.transaction_id,
        vec![attribute()],
    );
    ingress_sink.try_send((response1, ip(1111))).unwrap();

    // then
    assert!(runner_fut.is_woken());
    assert_pending!(runner_fut.poll());
    assert!(request1_fut.is_woken());
    let result = assert_ready!(request1_fut.poll());
    let response1 = result.unwrap();
    assert!(matches!(response1, Response::Error(attributes) if attributes == vec![attribute()]));
}
