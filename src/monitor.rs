use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

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

/// Minimum interval between RCON player-count queries.
/// Prevents opening a new TCP + RCON handshake every MONITOR_POLL_INTERVAL (2 s).
#[cfg(feature = "rcon")]
const RCON_CHECK_INTERVAL: Duration = Duration::from_secs(10);

/// Timeout for a single RCON player-count query.
/// Prevents the monitor loop from stalling if the server is unresponsive.
#[cfg(feature = "rcon")]
const RCON_QUERY_TIMEOUT: Duration = Duration::from_secs(10);

/// Monitor server.
pub async fn monitor_server(config: Arc<Config>, server: Arc<Server>) {
    // Server address
    let addr = config.server.address;

    let mut poll_interval = time::interval(MONITOR_POLL_INTERVAL);

    // Remember which status parser last succeeded so we try it first next time.
    // Avoids repeatedly failing the strict decode for modded servers.
    let mut use_lenient = false;

    // Throttle RCON cross-checks so we don't open a connection every 2 s
    #[cfg(feature = "rcon")]
    let mut last_rcon_check: Option<Instant> = None;

    // Track consecutive RCON failures so we can escalate the log level
    #[cfg(feature = "rcon")]
    let mut rcon_fail_streak: u32 = 0;

    // Track last known player count so we only log when it changes
    let mut last_player_count: Option<u32> = None;

    loop {
        poll_interval.tick().await;

        // Poll server state and update internal status
        trace!(target: "lazymc::monitor", "Fetching status for {} ... ", addr);
        let status = poll_server(&config, &server, addr, &mut use_lenient).await;
        match status {
            // Got status, update
            Ok(Some(status)) => {
                // If status reports 0 players, the server may be hiding its real
                // player count (common with plugins like TAB, ProtocolLib, or
                // hide-online-players=true). Double-check via RCON so we don't
                // put the server to sleep while players are actually online.
                let reported_online = status.players.online;
                let reported_max = status.players.max;

                // Log player count changes at debug level
                if last_player_count != Some(reported_online) {
                    debug!(
                        target: "lazymc::monitor",
                        "Player count changed: {} → {}/{}",
                        last_player_count
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "?".into()),
                        reported_online,
                        reported_max,
                    );
                    last_player_count = Some(reported_online);
                }

                server.update_status(&config, Some(status)).await;

                #[cfg(feature = "rcon")]
                if reported_online == 0
                    && config.rcon.enabled
                    && config.rcon.player_count_cross_check
                    && last_rcon_check.map_or(true, |t| t.elapsed() >= RCON_CHECK_INTERVAL)
                {
                    last_rcon_check = Some(Instant::now());

                    match time::timeout(RCON_QUERY_TIMEOUT, query_online_players_rcon(&config)).await {
                        Ok(Ok(count)) => {
                            rcon_fail_streak = 0;
                            debug!(target: "lazymc::monitor", "Status reports 0 players; RCON reports {} player(s) online", count);
                            if count > 0 {
                                server.update_last_active().await;
                            }
                        }
                        Ok(Err(err)) => {
                            rcon_fail_streak += 1;
                            if rcon_fail_streak >= 3 {
                                error!(
                                    target: "lazymc::monitor",
                                    "RCON cross-check failed {} times in a row ({}). \
                                     Server may sleep even with players online!",
                                    rcon_fail_streak, err,
                                );
                            } else {
                                warn!(target: "lazymc::monitor", "RCON player count query failed (status=0 cross-check): {}", err);
                            }
                        }
                        Err(_) => {
                            rcon_fail_streak += 1;
                            warn!(
                                target: "lazymc::monitor",
                                "RCON player count query timed out after {}s",
                                RCON_QUERY_TIMEOUT.as_secs(),
                            );
                        }
                    }
                }
            }

            // Error, reset status
            Err(_) => {
                // For servers like Folia that never respond to status/ping probes,
                // use RCON as the health signal before transitioning state.
                //
                // - Starting: a successful RCON handshake is enough to mark the
                //   server online (list command may not be available yet).
                // - Started: run `list` via RCON — this both confirms the server
                //   is alive and gives the real player count so the sleep timer
                //   resets correctly when players are connected.
                #[cfg(feature = "rcon")]
                let rcon_alive = if config.rcon.enabled
                    && matches!(server.state(), State::Starting | State::Started)
                {
                    match server.state() {
                        State::Starting => {
                            use crate::mc::rcon::Rcon;
                            let result = time::timeout(
                                RCON_QUERY_TIMEOUT,
                                async {
                                    Rcon::connect_config(&config)
                                        .await
                                        .map_err(|e| e.to_string())
                                },
                            )
                            .await;
                            match result {
                                Ok(Ok(rcon)) => {
                                    rcon.close().await;
                                    true
                                }
                                _ => false,
                            }
                        }
                        State::Started => {
                            // Use list command: health check + player count in one shot.
                            // Not throttled — status/ping are unavailable so this is
                            // the only crash-detection signal; we need it every poll.
                            match time::timeout(
                                RCON_QUERY_TIMEOUT,
                                query_online_players_rcon(&config),
                            )
                            .await
                            {
                                Ok(Ok(count)) => {
                                    rcon_fail_streak = 0;
                                    debug!(
                                        target: "lazymc::monitor",
                                        "Status/ping unavailable; RCON reports {} player(s) online",
                                        count,
                                    );
                                    if count > 0 {
                                        server.update_last_active().await;
                                    }
                                    true
                                }
                                Ok(Err(err)) => {
                                    rcon_fail_streak += 1;
                                    if rcon_fail_streak >= 3 {
                                        error!(
                                            target: "lazymc::monitor",
                                            "RCON health check failed {} times in a row ({}). \
                                             Server may be offline!",
                                            rcon_fail_streak, err,
                                        );
                                    } else {
                                        debug!(
                                            target: "lazymc::monitor",
                                            "RCON health check failed: {}",
                                            err,
                                        );
                                    }
                                    false
                                }
                                Err(_) => {
                                    rcon_fail_streak += 1;
                                    warn!(
                                        target: "lazymc::monitor",
                                        "RCON health check timed out after {}s",
                                        RCON_QUERY_TIMEOUT.as_secs(),
                                    );
                                    false
                                }
                            }
                        }
                        _ => false,
                    }
                } else {
                    false
                };

                #[cfg(not(feature = "rcon"))]
                let rcon_alive = false;

                if rcon_alive {
                    // RCON confirms the server is up; handle state transitions.
                    #[cfg(feature = "rcon")]
                    if server.state() == State::Starting {
                        info!(
                            target: "lazymc::monitor",
                            "RCON connected while server is starting (status/ping unavailable); \
                             marking server as started",
                        );
                        server.update_state(State::Started, &config).await;
                    }
                } else {
                    // RCON also failed (or not enabled) — server is genuinely offline.
                    server.update_status(&config, None).await;
                }
            }

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
                    if config.rcon.enabled
                        && config.rcon.player_count_cross_check
                        && last_rcon_check.map_or(true, |t| t.elapsed() >= RCON_CHECK_INTERVAL)
                    {
                        last_rcon_check = Some(Instant::now());

                        match time::timeout(RCON_QUERY_TIMEOUT, query_online_players_rcon(&config)).await {
                            Ok(Ok(count)) => {
                                rcon_fail_streak = 0;
                                debug!(target: "lazymc::monitor", "RCON reports {} player(s) online", count);
                                if count > 0 {
                                    server.update_last_active().await;
                                }
                            }
                            Ok(Err(err)) => {
                                rcon_fail_streak += 1;
                                if rcon_fail_streak >= 3 {
                                    error!(
                                        target: "lazymc::monitor",
                                        "RCON cross-check failed {} times in a row ({}). \
                                         Server may sleep even with players online!",
                                        rcon_fail_streak, err,
                                    );
                                } else {
                                    warn!(target: "lazymc::monitor", "RCON player count query failed: {}", err);
                                }
                            }
                            Err(_) => {
                                rcon_fail_streak += 1;
                                warn!(
                                    target: "lazymc::monitor",
                                    "RCON player count query timed out after {}s",
                                    RCON_QUERY_TIMEOUT.as_secs(),
                                );
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
    use_lenient: &mut bool,
) -> Result<Option<ServerStatus>, ()> {
    // Fetch status
    if let Ok(status) = fetch_status(config, addr, use_lenient).await {
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
async fn fetch_status(config: &Config, addr: SocketAddr, use_lenient: &mut bool) -> Result<ServerStatus, ()> {
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
    wait_for_status_timeout(&client, &mut stream, use_lenient).await
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
///
/// `use_lenient` remembers which parser last succeeded. When `true` the lenient
/// JSON parser is tried first (avoids a guaranteed-to-fail strict decode every
/// poll for modded servers). The flag is updated on parser switches.
async fn wait_for_status(
    client: &Client,
    stream: &mut TcpStream,
    use_lenient: &mut bool,
) -> Result<ServerStatus, ()> {
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
            return if *use_lenient {
                parse_lenient_first(&packet.data, use_lenient)
            } else {
                parse_strict_first(&packet.data, use_lenient)
            };
        }
    }

    // Some error occurred
    Err(())
}

/// Try strict protocol decode first, fall back to lenient JSON.
fn parse_strict_first(data: &[u8], use_lenient: &mut bool) -> Result<ServerStatus, ()> {
    // Try strict protocol decode
    let mut slice = data;
    match StatusResponse::decode(&mut slice) {
        Ok(resp) => {
            trace!(target: "lazymc::monitor", "Status parsed (strict)");
            return Ok(resp.server_status);
        }
        Err(err) => {
            debug!(
                target: "lazymc::monitor",
                "Strict status decode failed ({:?}), falling back to lenient JSON parser",
                err
            );
        }
    }

    // Fallback: lenient JSON parse
    match parse_status_json(data) {
        Ok(status) => {
            *use_lenient = true;
            info!(
                target: "lazymc::monitor",
                "Switching to lenient JSON parser for status (modded/non-standard server detected)"
            );
            log_parsed_status(&status);
            Ok(status)
        }
        Err(_) => {
            debug!(target: "lazymc::monitor", "Both strict and lenient status parsing failed, dropping packet");
            Err(())
        }
    }
}

/// Try lenient JSON parse first, fall back to strict protocol decode.
fn parse_lenient_first(data: &[u8], use_lenient: &mut bool) -> Result<ServerStatus, ()> {
    // Try lenient JSON parse (cached preference)
    if let Ok(status) = parse_status_json(data) {
        trace!(
            target: "lazymc::monitor",
            "Status parsed (lenient): players {}/{}",
            status.players.online,
            status.players.max,
        );
        return Ok(status);
    }

    // Lenient failed unexpectedly — maybe server changed? Try strict.
    let mut slice = data;
    match StatusResponse::decode(&mut slice) {
        Ok(resp) => {
            *use_lenient = false;
            info!(
                target: "lazymc::monitor",
                "Switching back to strict status parser (server now sends standard status)"
            );
            Ok(resp.server_status)
        }
        Err(_) => {
            debug!(target: "lazymc::monitor", "Both lenient and strict status parsing failed, dropping packet");
            Err(())
        }
    }
}

/// Log details of a successfully parsed status (used on parser switches to avoid spam).
fn log_parsed_status(status: &ServerStatus) {
    let desc: String = status
        .description
        .trim()
        .chars()
        .map(|c| if c == '\n' { ' ' } else { c })
        .collect();
    let desc_preview = {
        let mut chars = desc.chars();
        let truncated: String = chars.by_ref().take(60).collect();
        if chars.next().is_some() {
            format!("{truncated}…")
        } else {
            truncated
        }
    };
    debug!(
        target: "lazymc::monitor",
        "Status: version {} (protocol {}), players: {}/{}, description: \"{}\"",
        status.version.name,
        status.version.protocol,
        status.players.online,
        status.players.max,
        desc_preview,
    );
}

/// Wait for a status response.
async fn wait_for_status_timeout(
    client: &Client,
    stream: &mut TcpStream,
    use_lenient: &mut bool,
) -> Result<ServerStatus, ()> {
    let status = wait_for_status(client, stream, use_lenient);
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

    // Strip Minecraft formatting codes (§X where X is any char) so that
    // colored RCON responses like "§c3" are parsed correctly as "3".
    let clean = strip_minecraft_formatting(&response);

    // Parse player count from the `list` response, which may look like:
    //   "There are 3 of a max of 20 players online: player1, player2"
    //   "There are 3/20 players online: player1, player2"   (some plugins)
    // Strategy: walk tokens and try to parse the first one that is (or starts with) a number.
    // Splitting on '/' handles the "X/Y" form — we only want X (the online count).
    let count = clean
        .split_whitespace()
        .find_map(|word| {
            // Handle plain number ("3") and slash-separated fraction ("3/20")
            let numeric_part = word.split('/').next().unwrap_or(word);
            numeric_part.parse::<u32>().ok()
        })
        .unwrap_or(0);

    debug!(
        target: "lazymc::monitor",
        "RCON list response: {:?} → parsed {} player(s) online",
        response.trim(),
        count,
    );

    Ok(count)
}

/// Strip Minecraft formatting codes (`§` followed by a single character).
///
/// Some servers / plugins return colored RCON output, e.g.
/// `"§6There are §c3 §6of a max of §c20 §6players online:"`.
/// Stripping these lets the numeric parser work on clean text.
#[cfg(feature = "rcon")]
fn strip_minecraft_formatting(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '§' {
            // Skip the formatting character that follows §
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}
