use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::BytesMut;
use minecraft_protocol::decoder::Decoder;
use minecraft_protocol::version::v1_14_4::handshake::Handshake;
use minecraft_protocol::version::v1_20_3::status::{
    PingRequest, PingResponse, ServerStatus, StatusRequest, StatusResponse,
};
use rand::Rng;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::time;

use crate::config::Config;
use crate::proto::client::{Client, ClientState};
use crate::proto::{packet, packets};
use crate::proxy;
use crate::server::{Server, State};

/// Monitor ping inverval in seconds.
const MONITOR_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Status request timeout in seconds.
const STATUS_TIMEOUT: u64 = 20;

/// Ping request timeout in seconds.
const PING_TIMEOUT: u64 = 10;

/// Monitor server.
pub async fn monitor_server(config: Arc<Config>, server: Arc<Server>) {
    // Server address
    let addr = config.server.address;

    let mut poll_interval = time::interval(MONITOR_POLL_INTERVAL);

    loop {
        poll_interval.tick().await;

        // Poll server state and update internal status
        trace!(target: "lazymc::monitor", "Fetching status for {} ... ", addr);
        let status = poll_server(&config, &server, addr).await;
        match status {
            // Got status, update
            Ok(Some(status)) => server.update_status(&config, Some(status)).await,

            // Error, reset status
            Err(_) => server.update_status(&config, None).await,

            // Didn't get status, but ping fallback worked
            Ok(None) => {
                // If server is starting, treat ping success as server being online
                if server.state() == State::Starting {
                    info!(target: "lazymc::monitor", "Server responded to ping while starting, marking as started");
                    server.update_state(State::Started, &config).await;
                } else {
                    debug!(target: "lazymc::monitor", "Failed to poll server status, ping fallback succeeded");

                    // Use RCON to query player count so we can keep the server
                    // alive when players are online but status polling is broken
                    #[cfg(feature = "rcon")]
                    if config.rcon.enabled {
                        let rcon_result = query_online_players_rcon(&config).await;
                        match rcon_result {
                            Ok(count) => {
                                debug!(target: "lazymc::monitor", "RCON reports {} player(s) online", count);
                                if count > 0 {
                                    server.update_last_active().await;
                                }
                            }
                            Err(err) => {
                                warn!(target: "lazymc::monitor", "RCON player count query failed: {}", err);
                            }
                        }
                    }
                }
            }
        }

        // Sleep server when it's bedtime
        if server.should_sleep(&config).await {
            info!(target: "lazymc::monitor", "Server has been idle, sleeping...");
            server.stop(&config).await;
        }

        // Check whether we should force kill server
        if server.should_kill().await {
            error!(target: "lazymc::monitor", "Force killing server, took too long to start or stop");
            if !server.force_kill().await {
                warn!(target: "lazymc", "Failed to force kill server");
            }
        }
    }
}

/// Poll server state.
///
/// Returns `Ok` if status/ping succeeded, includes server status most of the time.
/// Returns `Err` if no connection could be established or if an error occurred.
pub async fn poll_server(
    config: &Config,
    server: &Server,
    addr: SocketAddr,
) -> Result<Option<ServerStatus>, ()> {
    // Fetch status
    if let Ok(status) = fetch_status(config, addr).await {
        return Ok(Some(status));
    }

    // Try ping fallback if server is currently started or starting
    match server.state() {
        State::Started | State::Starting => {
            debug!(target: "lazymc::monitor", "Failed to get status from server, trying ping...");
            do_ping(config, addr).await?;
            return Ok(None);
        }
        _ => {}
    }

    Err(())
}

/// Attemp to fetch status from server.
async fn fetch_status(config: &Config, addr: SocketAddr) -> Result<ServerStatus, ()> {
    let mut stream = TcpStream::connect(addr).await.map_err(|_| ())?;

    // Add proxy header
    if config.server.send_proxy_v2 {
        trace!(target: "lazymc::monitor", "Sending local proxy header for server connection");
        stream
            .write_all(&proxy::local_proxy_header().map_err(|_| ())?)
            .await
            .map_err(|_| ())?;
    }

    // Dummy client
    let client = Client::dummy();

    send_handshake(&client, &mut stream, config, addr).await?;
    request_status(&client, &mut stream).await?;
    wait_for_status_timeout(&client, &mut stream).await
}

/// Attemp to ping server.
async fn do_ping(config: &Config, addr: SocketAddr) -> Result<(), ()> {
    let mut stream = TcpStream::connect(addr).await.map_err(|_| ())?;

    // Add proxy header
    if config.server.send_proxy_v2 {
        trace!(target: "lazymc::monitor", "Sending local proxy header for server connection");
        stream
            .write_all(&proxy::local_proxy_header().map_err(|_| ())?)
            .await
            .map_err(|_| ())?;
    }

    // Dummy client
    let client = Client::dummy();

    send_handshake(&client, &mut stream, config, addr).await?;
    let token = send_ping(&client, &mut stream).await?;
    wait_for_ping_timeout(&client, &mut stream, token).await
}

/// Send handshake.
async fn send_handshake(
    client: &Client,
    stream: &mut TcpStream,
    config: &Config,
    addr: SocketAddr,
) -> Result<(), ()> {
    packet::write_packet(
        Handshake {
            protocol_version: config.public.protocol as i32,
            server_addr: addr.ip().to_string(),
            server_port: addr.port(),
            next_state: ClientState::Status.to_id(),
        },
        client,
        &mut stream.split().1,
    )
    .await
}

/// Send status request.
async fn request_status(client: &Client, stream: &mut TcpStream) -> Result<(), ()> {
    packet::write_packet(StatusRequest {}, client, &mut stream.split().1).await
}

/// Send status request.
async fn send_ping(client: &Client, stream: &mut TcpStream) -> Result<u64, ()> {
    let token = rand::thread_rng().gen();
    packet::write_packet(PingRequest { time: token }, client, &mut stream.split().1).await?;
    Ok(token)
}

/// Wait for a status response.
async fn wait_for_status(client: &Client, stream: &mut TcpStream) -> Result<ServerStatus, ()> {
    // Get stream reader, set up buffer
    let (mut reader, mut _writer) = stream.split();
    let mut buf = BytesMut::new();

    loop {
        // Read packet from stream
        let (packet, _raw) = match packet::read_packet(client, &mut buf, &mut reader).await {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(_) => continue,
        };

        // Catch status response
        if packet.id == packets::status::CLIENT_STATUS {
            // Try strict protocol decode first
            if let Ok(status) = StatusResponse::decode(&mut packet.data.as_slice()) {
                return Ok(status.server_status);
            }

            // Fallback: lenient JSON parse for modded servers (Forge/NeoForge/Fabric)
            // that return non-standard status responses (e.g. description as object)
            if let Ok(status) = parse_status_json(&packet.data) {
                debug!(target: "lazymc::monitor", "Used lenient JSON parser for server status");
                return Ok(status);
            }

            return Err(());
        }
    }

    // Some error occurred
    Err(())
}

/// Wait for a status response.
async fn wait_for_status_timeout(
    client: &Client,
    stream: &mut TcpStream,
) -> Result<ServerStatus, ()> {
    let status = wait_for_status(client, stream);
    tokio::time::timeout(Duration::from_secs(STATUS_TIMEOUT), status)
        .await
        .map_err(|_| ())?
}

/// Wait for a status response.
async fn wait_for_ping(client: &Client, stream: &mut TcpStream, token: u64) -> Result<(), ()> {
    // Get stream reader, set up buffer
    let (mut reader, mut _writer) = stream.split();
    let mut buf = BytesMut::new();

    loop {
        // Read packet from stream
        let (packet, _raw) = match packet::read_packet(client, &mut buf, &mut reader).await {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(_) => continue,
        };

        // Catch ping response
        if packet.id == packets::status::CLIENT_PING {
            let ping = PingResponse::decode(&mut packet.data.as_slice()).map_err(|_| ())?;

            // Ping token must match
            if ping.time == token {
                return Ok(());
            } else {
                debug!(target: "lazymc", "Got unmatched ping response when polling server status by ping");
            }
        }
    }

    // Some error occurred
    Err(())
}

/// Wait for a status response.
async fn wait_for_ping_timeout(
    client: &Client,
    stream: &mut TcpStream,
    token: u64,
) -> Result<(), ()> {
    let status = wait_for_ping(client, stream, token);
    tokio::time::timeout(Duration::from_secs(PING_TIMEOUT), status)
        .await
        .map_err(|_| ())?
}

/// Leniently parse a server status JSON from raw packet data.
///
/// This handles modded servers (Forge/NeoForge/Fabric) that return non-standard status
/// responses, e.g. `description` as a Chat Component object instead of a plain string.
/// The packet data is: [var-int string length] [UTF-8 JSON bytes].
fn parse_status_json(data: &[u8]) -> Result<ServerStatus, ()> {
    use minecraft_protocol::version::v1_20_3::status::ServerStatus as StrictStatus;
    use serde_json::Value;

    // Read var-int string length prefix, then extract JSON bytes
    let (prefix_len, str_len) = crate::types::read_var_int(data)?;
    let json_bytes = data
        .get(prefix_len..prefix_len + str_len as usize)
        .ok_or(())?;
    let json_str = std::str::from_utf8(json_bytes).map_err(|_| ())?;

    // Try strict serde first on the raw JSON string (handles edge cases where
    // the var-int decode differed but JSON is actually valid for the struct)
    if let Ok(status) = serde_json::from_str::<StrictStatus>(json_str) {
        return Ok(status);
    }

    // Parse as generic JSON value
    let root: Value = serde_json::from_str(json_str).map_err(|_| ())?;

    // Extract version
    let version_obj = root.get("version");
    let version_name = version_obj
        .and_then(|v| v.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();
    let version_protocol = version_obj
        .and_then(|v| v.get("protocol"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    // Extract players
    let players_obj = root.get("players");
    let players_online = players_obj
        .and_then(|v| v.get("online"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let players_max = players_obj
        .and_then(|v| v.get("max"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    // Extract description: may be a plain string or a Chat Component object
    let description = match root.get("description") {
        Some(Value::String(s)) => s.clone(),
        Some(obj) => serde_json::to_string(obj).unwrap_or_default(),
        None => String::new(),
    };

    // Extract favicon
    let favicon = root
        .get("favicon")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Ok(ServerStatus {
        version: minecraft_protocol::data::server_status::ServerVersion {
            name: version_name,
            protocol: version_protocol,
        },
        players: minecraft_protocol::data::server_status::OnlinePlayers {
            online: players_online,
            max: players_max,
            sample: vec![],
        },
        description,
        favicon,
    })
}

/// Query online player count via RCON `list` command.
///
/// Parses the response from the Minecraft `list` command which typically looks like:
/// "There are X of a max of Y players online: ..."
#[cfg(feature = "rcon")]
async fn query_online_players_rcon(config: &Config) -> Result<u32, String> {
    use crate::mc::rcon::Rcon;

    let mut rcon = Rcon::connect_config(config)
        .await
        .map_err(|e| e.to_string())?;
    let response = rcon.cmd("list").await.map_err(|e| e.to_string())?;
    rcon.close().await;

    // Parse "There are X of a max of Y players online: ..."
    // Also handles variations like "There are X/Y players online"
    let count = response
        .split_whitespace()
        .flat_map(|w| w.parse::<u32>())
        .next()
        .unwrap_or(0);

    Ok(count)
}
