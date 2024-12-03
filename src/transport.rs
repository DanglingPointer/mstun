use crate::message::Message;
use std::net::SocketAddr;
use tokio::sync::mpsc;

pub mod tcp;
pub mod udp;

#[cfg(test)]
mod testutils;

#[allow(dead_code)]
pub struct MessageChannels {
    pub(crate) egress_sink: mpsc::Sender<(Message, SocketAddr)>,
    pub(crate) ingress_source: mpsc::Receiver<(Message, SocketAddr)>,
}
