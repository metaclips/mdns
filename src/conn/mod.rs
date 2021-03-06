use crate::config::*;
use crate::errors::*;
use crate::message::name::*;
use crate::message::{header::*, parser::*, question::*, resource::a::*, resource::*, *};

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use core::sync::atomic;
use socket2::SockAddr;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::sync::Mutex;

use util::ifaces;
use util::Error;

mod conn_test;

pub const DEFAULT_DEST_ADDR: &str = "224.0.0.251:5353";

const INBOUND_BUFFER_SIZE: usize = 512;
const DEFAULT_QUERY_INTERVAL: Duration = Duration::from_secs(1);
const MAX_MESSAGE_RECORDS: usize = 3;
const RESPONSE_TTL: u32 = 120;

// Conn represents a mDNS Server
pub struct DNSConn {
    socket: Arc<UdpSocket>,
    dst_addr: SocketAddr,

    query_interval: Duration,
    queries: Arc<Mutex<Vec<Query>>>,

    is_server_closed: Arc<atomic::AtomicBool>,
    close_server: mpsc::Sender<()>,
}

struct Query {
    name_with_suffix: String,
    query_result_chan: mpsc::Sender<QueryResult>,
}

struct QueryResult {
    answer: ResourceHeader,
    addr: SocketAddr,
}

impl DNSConn {
    /// server establishes a mDNS connection over an existing connection
    pub fn server(addr: SocketAddr, config: Config) -> Result<Self, Error> {
        let socket = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )?;

        socket.set_reuse_address(true)?;

        //TODO: implement set_reuse_port for windows platform
        #[cfg(target_family = "unix")]
        socket.set_reuse_port(true)?;

        socket.set_read_timeout(Some(Duration::from_millis(100)))?;
        socket.bind(&SockAddr::from(addr))?;

        {
            let mut join_error_count = 0;
            let interfaces = match ifaces::ifaces() {
                Ok(e) => e,
                Err(e) => {
                    log::error!("Error getting interfaces: {:?}", e);
                    return Err(Error::new(e.to_string()));
                }
            };

            for interface in &interfaces {
                if let Some(SocketAddr::V4(e)) = interface.addr {
                    if let Err(e) = socket.join_multicast_v4(&Ipv4Addr::new(224, 0, 0, 251), e.ip())
                    {
                        log::error!("Error connecting multicast, error: {:?}", e);
                        join_error_count += 1;
                        continue;
                    }

                    log::trace!("Connected to interface address {:?}", e);
                }
            }

            if join_error_count >= interfaces.len() {
                return Err(ERR_JOINING_MULTICAST_GROUP.to_owned());
            }
        }

        let socket = UdpSocket::from_std(socket.into())?;

        let local_names = config
            .local_names
            .iter()
            .map(|l| l.to_string() + ".")
            .collect();

        let dst_addr: SocketAddr = DEFAULT_DEST_ADDR.parse()?;

        let is_server_closed = Arc::new(atomic::AtomicBool::new(false));

        let (close_server_send, close_server_rcv) = mpsc::channel(1);

        let c = DNSConn {
            query_interval: if config.query_interval != Duration::from_secs(0) {
                config.query_interval
            } else {
                DEFAULT_QUERY_INTERVAL
            },

            queries: Arc::new(Mutex::new(vec![])),
            socket: Arc::new(socket),
            dst_addr,
            is_server_closed: Arc::clone(&is_server_closed),
            close_server: close_server_send,
        };

        let queries = c.queries.clone();
        let socket = Arc::clone(&c.socket);

        tokio::spawn(async move {
            DNSConn::start(
                close_server_rcv,
                is_server_closed,
                socket,
                local_names,
                dst_addr,
                queries,
            )
            .await
        });

        Ok(c)
    }

    /// Close closes the mDNS Conn
    pub async fn close(&self) -> Result<(), Error> {
        {
            log::info!("Closing connection");
            if self.is_server_closed.load(atomic::Ordering::SeqCst) {
                return Err(ERR_CONNECTION_CLOSED.to_owned());
            }
        }

        log::info!("Sending close command to server");
        match self.close_server.send(()).await {
            Ok(_) => Ok(()),
            Err(e) => {
                log::warn!("error sending close command to server: {:?}", e);
                Err(ERR_CONNECTION_CLOSED.to_owned())
            }
        }
    }

    /// Query sends mDNS Queries for the following name until
    /// either there's a close signalling or we get a result
    pub async fn query(
        &self,
        name: &str,
        mut close_query_signal: mpsc::Receiver<()>,
    ) -> Result<(ResourceHeader, SocketAddr), Error> {
        {
            if self.is_server_closed.load(atomic::Ordering::SeqCst) {
                return Err(ERR_CONNECTION_CLOSED.to_owned());
            }
        }

        let name_with_suffix = name.to_owned() + ".";

        let (query_tx, mut query_rx) = mpsc::channel(1);
        {
            let mut queries = self.queries.lock().await;
            queries.push(Query {
                name_with_suffix: name_with_suffix.clone(),
                query_result_chan: query_tx,
            });
        }

        log::trace!("Sending query");
        self.send_question(&name_with_suffix).await;

        loop {
            tokio::select! {
                _ = tokio::time::sleep(self.query_interval) => {
                    log::trace!("Sending query");
                    self.send_question(&name_with_suffix).await
                },

                _ = close_query_signal.recv() => {
                    log::info!("Query close signal received.");
                    return Err(ERR_CONNECTION_CLOSED.to_owned())
                },

                res_opt = query_rx.recv() =>{
                    log::info!("Received query result");
                    if let Some(res) = res_opt{
                        return Ok((res.answer, res.addr));
                    }
                }
            }
        }
    }

    async fn send_question(&self, name: &str) {
        let packed_name = match Name::new(name) {
            Ok(pn) => pn,
            Err(err) => {
                log::warn!("Failed to construct mDNS packet: {}", err);
                return;
            }
        };

        let raw_query = {
            let mut msg = Message {
                header: Header::default(),
                questions: vec![Question {
                    typ: DNSType::A,
                    class: DNSCLASS_INET,
                    name: packed_name,
                }],
                ..Default::default()
            };

            match msg.pack() {
                Ok(v) => v,
                Err(err) => {
                    log::error!("Failed to construct mDNS packet {}", err);
                    return;
                }
            }
        };

        log::trace!("{:?} sending {:?}...", self.socket.local_addr(), raw_query);
        if let Err(err) = self.socket.send_to(&raw_query, self.dst_addr).await {
            log::error!("Failed to send mDNS packet {}", err);
        }
    }

    async fn start(
        mut closed_rx: mpsc::Receiver<()>,
        close_server: Arc<atomic::AtomicBool>,
        socket: Arc<UdpSocket>,
        local_names: Vec<String>,
        dst_addr: SocketAddr,
        queries: Arc<Mutex<Vec<Query>>>,
    ) -> Result<(), Error> {
        log::info!("enter loop and listening {:?}", socket.local_addr());

        let mut b = vec![0u8; INBOUND_BUFFER_SIZE];
        let (mut n, mut src);

        loop {
            tokio::select! {
                _ = closed_rx.recv() => {
                    log::info!("Closing server connection");
                    close_server.store(true, atomic::Ordering::SeqCst);

                    return Ok(());
                }

                result = socket.recv_from(&mut b) => {
                    match result{
                        Ok((len, addr)) => {
                            n = len;
                            src = addr;
                            log::info!("Received new connection from {:?}", addr);
                        },

                        Err(err) => {
                            log::error!("Error receiving from socket connection: {:?}", err);
                            return Err(Error::new(err.to_string()))
                        },
                    }
                }
            }

            log::trace!("recv bytes {:?} from {}", &b[..n], src);

            let mut p = Parser::default();
            if let Err(err) = p.start(&b[..n]) {
                log::error!("Failed to parse mDNS packet {}", err);
                continue;
            }

            run(&mut p, &socket, &local_names, src, dst_addr, &queries).await
        }
    }
}

async fn run(
    p: &mut Parser<'_>,
    socket: &Arc<UdpSocket>,
    local_names: &[String],
    src: SocketAddr,
    dst_addr: SocketAddr,
    queries: &Arc<Mutex<Vec<Query>>>,
) {
    for _ in 0..=MAX_MESSAGE_RECORDS {
        let q = match p.question() {
            Ok(q) => q,
            Err(err) => {
                if err == *ERR_SECTION_DONE {
                    log::trace!("Parsing has completed");
                    break;
                } else {
                    log::error!("Failed to parse mDNS packet {}", err);
                    return;
                }
            }
        };

        for local_name in local_names {
            if local_name == &q.name.data {
                log::trace!("Found local name: {} to send answer", local_name);
                if let Err(e) = send_answer(socket, &q.name.data, src.ip(), dst_addr).await {
                    log::error!("Error sending answer to client: {:?}", e);
                    continue;
                };

                log::trace!(
                    "Sent answer to local name: {} to dst addr {:?}",
                    local_name,
                    dst_addr
                );
            }
        }
    }

    for _ in 0..=MAX_MESSAGE_RECORDS {
        let a = match p.answer_header() {
            Ok(a) => a,
            Err(err) => {
                if err == *ERR_SECTION_DONE {
                    return;
                } else {
                    log::warn!("Failed to parse mDNS packet {}", err);
                    return;
                }
            }
        };

        if a.typ != DNSType::A && a.typ != DNSType::AAAA {
            continue;
        }

        let mut qs = queries.lock().await;
        for j in (0..qs.len()).rev() {
            if qs[j].name_with_suffix == a.name.data {
                let _ = qs[j]
                    .query_result_chan
                    .send(QueryResult {
                        answer: a.clone(),
                        addr: src,
                    })
                    .await;
                qs.remove(j);
            }
        }
    }
}

async fn interface_for_remote(remote: String) -> Result<std::net::IpAddr, Error> {
    let conn = UdpSocket::bind(remote).await?;
    let local_addr = conn.local_addr()?;

    Ok(local_addr.ip())
}

async fn send_answer(
    socket: &Arc<UdpSocket>,
    name: &str,
    dst: IpAddr,
    dst_addr: SocketAddr,
) -> Result<(), Error> {
    let raw_answer = {
        let mut msg = Message {
            header: Header {
                response: true,
                authoritative: true,
                ..Default::default()
            },

            answers: vec![Resource {
                header: ResourceHeader {
                    typ: DNSType::A,
                    class: DNSCLASS_INET,
                    name: Name::new(name)?,
                    ttl: RESPONSE_TTL,
                    ..Default::default()
                },
                body: Some(Box::new(AResource {
                    a: match dst {
                        IpAddr::V4(ip) => ip.octets(),
                        IpAddr::V6(_) => return Err(Error::new("unexpected IpV6 addr".to_owned())),
                    },
                })),
            }],
            ..Default::default()
        };

        msg.pack()?
    };

    socket.send_to(&raw_answer, dst_addr).await?;
    log::trace!("sent answer from {} to {}", dst, dst_addr);

    Ok(())
}
