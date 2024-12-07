use futures::StreamExt;
use local_async_utils::{local_sync, sec};
use std::collections::HashSet;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, ToSocketAddrs};
use stunny::transactions::*;
use stunny::transport::tcp::{setup_tcp, Config};
use stunny::transport::udp::setup_udp;
use tokio::net::UdpSocket;
use tokio::{join, task, time};

macro_rules! local_test {
    ($($arg:tt)+) => {{
        task::LocalSet::new().run_until(time::timeout(sec!(10), async $($arg)+)).await.expect("test timeout");
    }}
}

async fn create_udp_socket() -> io::Result<UdpSocket> {
    UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).await
}

const UDP_SERVERS: [&str; 4] = [
    "stun:stunserver2024.stunprotocol.org:3478",
    "stun:stun.l.google.com:19302",
    "stun:stun2.l.google.com:19302",
    "stun:stun4.l.google.com:19302",
];

const TCP_SERVERS: [&str; 3] = [
    "stun:stunserver2024.stunprotocol.org:3478",
    "stun:stun.sipnet.net:3478",
    "stun:stun.sipnet.ru:3478",
];

fn parse_server_addrs<'a>(
    urls: impl IntoIterator<Item = &'a str> + 'a,
) -> impl Iterator<Item = SocketAddr> + 'a {
    urls.into_iter()
        .filter_map(|url| url.strip_prefix("stun:"))
        .filter_map(|arg| arg.to_socket_addrs().ok())
        .flatten()
        .filter(SocketAddr::is_ipv4)
        .collect::<HashSet<_>>()
        .into_iter()
}

async fn do_bind_request(
    request_sender: RequestSender,
    addr: SocketAddr,
    result_sender: local_sync::channel::Sender<Result<Response, TransactionError>>,
) {
    println!("Sending request to {:?}", addr);
    let result = request_sender.send_request(addr, 0x0001, vec![]).await;
    println!("Response from {}:\n{:?}", addr, result);
    result_sender.send(result);
}

#[tokio::test]
async fn send_bind_request_over_udp() {
    let _ = simple_logger::SimpleLogger::new()
        .with_level(log::LevelFilter::Trace)
        .init();
    local_test!({
        const MAX_CONCURRENT_REQUESTS: usize = 10;

        let socket = create_udp_socket().await.unwrap();

        let (message_channels, driver) = setup_udp(socket, MAX_CONCURRENT_REQUESTS);

        let (request_sender, _, _, processor) = setup_transactions(
            message_channels,
            MAX_CONCURRENT_REQUESTS,
            NoRetransmissionsConstTimeout::new(sec!(1)),
        );

        task::spawn_local(async move {
            let _result = join!(driver.run(), processor.run());
        });

        let (result_sender, result_receiver) = local_sync::channel();

        for addr in parse_server_addrs(UDP_SERVERS) {
            task::spawn_local(do_bind_request(
                request_sender.clone(),
                addr,
                result_sender.clone(),
            ));
        }
        drop(result_sender);

        let results: Vec<_> = result_receiver.collect().await;
        assert!(
            results
                .iter()
                .any(|result| matches!(result, Ok(Response::Success(_)))),
            "{results:?}"
        );
    })
}

#[tokio::test]
async fn send_bind_request_over_tcp() {
    let _ = simple_logger::SimpleLogger::new()
        .with_level(log::LevelFilter::Trace)
        .init();
    local_test!({
        const MAX_CONCURRENT_REQUESTS: usize = 10;

        let (message_channels, connection_pool) = setup_tcp(Config::default());

        let (request_sender, _, _, processor) = setup_transactions(
            message_channels,
            MAX_CONCURRENT_REQUESTS,
            NoRetransmissionsConstTimeout::new(sec!(1)),
        );

        task::spawn_local(async move {
            let _result = join!(connection_pool.run(), processor.run());
        });

        let (result_sender, result_receiver) = local_sync::channel();

        for addr in parse_server_addrs(TCP_SERVERS) {
            task::spawn_local(do_bind_request(
                request_sender.clone(),
                addr,
                result_sender.clone(),
            ));
        }
        drop(result_sender);

        let results: Vec<_> = result_receiver.collect().await;
        assert!(
            results
                .iter()
                .any(|result| matches!(result, Ok(Response::Success(_)))),
            "{results:?}"
        );
    })
}
