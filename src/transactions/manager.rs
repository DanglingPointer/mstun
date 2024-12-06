use super::*;
use rand::random;
use std::cmp::Ordering;
use std::collections::binary_heap::PeekMut;
use std::collections::hash_map::Entry;
use std::collections::{BinaryHeap, HashMap};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

pub(super) struct Request {
    destination_addr: SocketAddr,
    method: u16,
    attributes: Vec<Tlv>,
    response_sink: oneshot::Sender<Result<Response, TransactionError>>,
    attempts_made: usize,
    start_time: Instant,
}

impl Request {
    pub(super) fn new(
        destination_addr: SocketAddr,
        method: u16,
        attributes: Vec<Tlv>,
        response_sink: oneshot::Sender<Result<Response, TransactionError>>,
    ) -> Self {
        Self {
            destination_addr,
            method,
            attributes,
            response_sink,
            attempts_made: 0,
            start_time: Instant::now(),
        }
    }
}

type TransactionId = [u8; 12];

struct PendingTimeout {
    timeout_at: Instant,
    tid: TransactionId,
}

pub(super) struct Manager<P> {
    pending_timeouts: BinaryHeap<PendingTimeout>,
    outstanding_requests: HashMap<TransactionId, Request>,
    egress_sink: mpsc::Sender<(Message, SocketAddr)>,
    incoming_indications_sink: mpsc::Sender<Indication>,
    rto: P,
}

impl<P: RtoPolicy> Manager<P> {
    pub(super) fn new(
        rto_policy: P,
        egress_sink: mpsc::Sender<(Message, SocketAddr)>,
        incoming_indications_sink: mpsc::Sender<Indication>,
    ) -> Self {
        Self {
            pending_timeouts: Default::default(),
            outstanding_requests: Default::default(),
            egress_sink,
            incoming_indications_sink,
            rto: rto_policy,
        }
    }

    pub(super) fn next_timeout(&self) -> Option<Instant> {
        self.pending_timeouts.peek().map(|pt| pt.timeout_at)
    }

    pub(super) async fn handle_timeouts(&mut self) -> Result<(), TransactionError> {
        loop {
            // extract the earliest timeout, exit if it's in the future
            let mut timeout = match self.pending_timeouts.peek_mut() {
                Some(timeout) if timeout.timeout_at <= Instant::now() => PeekMut::pop(timeout),
                _ => break,
            };
            // fetch the corresponding request entry
            let mut outstanding = match self.outstanding_requests.entry(timeout.tid) {
                Entry::Occupied(occupied_entry) => occupied_entry,
                Entry::Vacant(_) => unreachable!("no request for pending timeout"),
            };
            let request = outstanding.get();
            match self.rto.calculate_rto(
                request.destination_addr,
                request.attempts_made,
                request.start_time,
            ) {
                None => {
                    // erase entry and invoke callback with error
                    let _ = outstanding
                        .remove()
                        .response_sink
                        .send(Err(TransactionError::Timeout));
                }
                Some(next_rto) => {
                    let request = outstanding.get_mut();
                    // retransmit request
                    let msg =
                        Message::request(request.method, timeout.tid, request.attributes.clone());
                    self.egress_sink
                        .send((msg, request.destination_addr))
                        .await?;
                    // schedule next timeout
                    request.attempts_made += 1;
                    timeout.timeout_at = Instant::now() + next_rto;
                    self.pending_timeouts.push(timeout);
                }
            }
        }
        Ok(())
    }

    pub(super) async fn handle_outgoing_indication(
        &mut self,
        indication: Indication,
    ) -> Result<(), TransactionError> {
        let tid = random::<TransactionId>();
        let msg = Message::indication(indication.method, tid, indication.attributes.clone());
        self.egress_sink.send((msg, indication.farend_addr)).await?;
        Ok(())
    }

    pub(super) async fn handle_outgoing_request(
        &mut self,
        mut request: Request,
    ) -> Result<(), TransactionError> {
        let tid = random::<TransactionId>();
        let msg = Message::request(request.method, tid, request.attributes.clone());
        match self.egress_sink.send((msg, request.destination_addr)).await {
            Ok(_) => {
                let now = Instant::now();

                let initial_rto = self
                    .rto
                    .calculate_rto(request.destination_addr, 0, now)
                    .unwrap_or(DEFAULT_RTO);
                self.pending_timeouts.push(PendingTimeout {
                    timeout_at: now + initial_rto,
                    tid,
                });

                request.attempts_made = 1;
                request.start_time = now;
                self.outstanding_requests.insert(tid, request);
                Ok(())
            }
            Err(e) => {
                let _ = request
                    .response_sink
                    .send(Err(TransactionError::ChannelClosed));
                Err(e.into())
            }
        }
    }

    pub(super) async fn handle_incoming_message(
        &mut self,
        (message, source_addr): (Message, SocketAddr),
    ) -> Result<(), TransactionError> {
        match message.header.class {
            Class::Request => {
                log::error!("Ignoring incoming request: handling of requests is not supported");
            }
            Class::Indication => {
                self.incoming_indications_sink
                    .send(Indication {
                        farend_addr: source_addr,
                        method: message.header.method,
                        attributes: message.attributes,
                    })
                    .await?;
            }
            Class::Response | Class::Error => {
                let request = match self
                    .outstanding_requests
                    .remove(&message.header.transaction_id)
                {
                    Some(request) if request.destination_addr == source_addr => request,
                    Some(_) => {
                        log::warn!("Received response from unexpected source");
                        return Ok(());
                    }
                    None => {
                        log::warn!("Received orphaned response from {source_addr}");
                        return Ok(());
                    }
                };

                if request.attempts_made == 1 {
                    self.rto
                        .submit_rtt(source_addr, request.start_time.elapsed());
                }

                let request_method = request.method;
                let response_method = message.header.method;

                let result = if request_method != response_method {
                    Err(TransactionError::MethodMismatch {
                        request_method,
                        response_method,
                    })
                } else {
                    match message.header.class {
                        Class::Response => Ok(Response::Success(message.attributes)),
                        Class::Error => Ok(Response::Error(message.attributes)),
                        Class::Request | Class::Indication => unreachable!(),
                    }
                };
                let _ = request.response_sink.send(result);
                self.pending_timeouts
                    .retain(|pt| pt.tid != message.header.transaction_id);
            }
        }
        Ok(())
    }
}

const DEFAULT_RTO: Duration = Duration::from_millis(1500);

// fn calculate_msg_len<'t>(attributes: impl IntoIterator<Item = &'t Tlv>) -> u16 {
//     let len: usize = attributes.into_iter().map(|tlv| tlv.value.len()).sum();
//     (len as u16 + 3) & !0x3
// }

impl PartialEq for PendingTimeout {
    fn eq(&self, other: &Self) -> bool {
        self.timeout_at == other.timeout_at
    }
}

impl Eq for PendingTimeout {}

impl PartialOrd for PendingTimeout {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PendingTimeout {
    fn cmp(&self, other: &Self) -> Ordering {
        // reverse order for min-BinaryHeap
        other.timeout_at.cmp(&self.timeout_at)
    }
}