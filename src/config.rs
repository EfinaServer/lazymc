use std::env;
use std::fs;
use std::io;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::PathBuf;

use clap::ArgMatches;
use serde::Deserialize;
use toml::map::Map;
use version_compare::Cmp;

use crate::proto;
use crate::util::error::{quit_error, quit_error_msg, ErrorHintsBuilder};
use crate::util::serde::to_socket_addrs;

/// Default configuration file location.
pub const CONFIG_FILE: &str = "lazymc.toml";

/// Configuration version user should be using, or warning will be shown.
const CONFIG_VERSION: &str = "0.2.8";

/// Prefix for environment variable-based configuration.
const ENV_PREFIX: &str = "LAZYMC_";

/// Section separator in environment variable names.
const ENV_SEPARATOR: &str = "__";

/// Load config from file (with optional env overrides) or purely from env vars.
/// CLI flag overrides are applied last (highest priority).
///
/// Quits with an error message on failure.
pub fn load(matches: &ArgMatches) -> Config {
    // Get config path, attempt to canonicalize
    let mut path = PathBuf::from(matches.get_one::<String>("config").unwrap());
    if let Ok(p) = path.canonicalize() {
        path = p;
    }

    let mut config = if path.is_file() {
        // Load from file, then merge env overrides
        match Config::load(path) {
            Ok(config) => config,
            Err(err) => {
                quit_error(
                    anyhow!(err).context("Failed to load config"),
                    ErrorHintsBuilder::default()
                        .config(true)
                        .config_test(true)
                        .build()
                        .unwrap(),
                );
            }
        }
    } else if has_env_config() {
        // No config file, but env vars present — build config from env
        match Config::from_env() {
            Ok(config) => config,
            Err(err) => {
                quit_error(
                    anyhow!(err).context("Failed to load config from environment variables"),
                    ErrorHintsBuilder::default().build().unwrap(),
                );
            }
        }
    } else {
        quit_error_msg(
            format!(
                "Config file does not exist: {}\n\
                 Hint: you can also configure lazymc entirely through LAZYMC_ environment variables.",
                path.to_str().unwrap_or("?")
            ),
            ErrorHintsBuilder::default()
                .config(true)
                .config_generate(true)
                .build()
                .unwrap(),
        );
    };

    // Apply CLI flag overrides (highest priority)
    apply_cli_overrides(&mut config, matches);

    config
}

/// Apply CLI flag overrides to the config. CLI flags have the highest priority.
fn apply_cli_overrides(config: &mut Config, matches: &ArgMatches) {
    if let Some(addr_str) = matches.get_one::<String>("public-address") {
        let addr = addr_str
            .to_socket_addrs()
            .ok()
            .and_then(|mut addrs| addrs.next())
            .or_else(|| addr_str.parse().ok())
            .unwrap_or_else(|| {
                quit_error_msg(
                    format!("Invalid public address: {addr_str}"),
                    ErrorHintsBuilder::default().build().unwrap(),
                );
            });
        config.public.address = addr;
    }
}

/// Check whether any `LAZYMC_` environment variables are set.
pub fn has_env_config() -> bool {
    env::vars().any(|(k, _)| k.starts_with(ENV_PREFIX))
}

/// Configuration.
#[derive(Debug, Deserialize)]
pub struct Config {
    /// Configuration path if known.
    ///
    /// Should be used as base directory for filesystem operations.
    #[serde(skip)]
    pub path: Option<PathBuf>,

    /// Public configuration.
    #[serde(default)]
    pub public: Public,

    /// Server configuration.
    pub server: Server,

    /// Time configuration.
    #[serde(default)]
    pub time: Time,

    /// MOTD configuration.
    #[serde(default)]
    pub motd: Motd,

    /// Join configuration.
    #[serde(default)]
    pub join: Join,

    /// Lockout feature.
    #[serde(default)]
    pub lockout: Lockout,

    /// RCON configuration.
    #[serde(default)]
    pub rcon: Rcon,

    /// Advanced configuration.
    #[serde(default)]
    pub advanced: Advanced,

    /// Config configuration.
    #[serde(default)]
    pub config: ConfigConfig,
}

impl Config {
    /// Load configuration from file, with env var overrides merged in.
    pub fn load(path: PathBuf) -> Result<Self, io::Error> {
        let data = fs::read_to_string(&path)?;
        let mut file_value: toml::Value = toml::from_str(&data).map_err(io::Error::other)?;

        // Merge env var overrides on top of file config
        let env_value = collect_env_config();
        if env_value.as_table().map_or(false, |t| !t.is_empty()) {
            file_value = deep_merge(file_value, env_value);
        }

        Self::from_value(file_value, Some(path))
    }

    /// Build configuration purely from environment variables and serde defaults.
    pub fn from_env() -> Result<Self, io::Error> {
        let env_value = collect_env_config();
        Self::from_value(env_value, None)
    }

    /// Shared deserialization, version check, and path assignment.
    fn from_value(value: toml::Value, path: Option<PathBuf>) -> Result<Self, io::Error> {
        let mut config: Config = value.try_into().map_err(io::Error::other)?;

        // Show warning if config version is problematic
        match &config.config.version {
            None => warn!(target: "lazymc::config", "Config version unknown, it may be outdated"),
            Some(version) => match version_compare::compare_to(version, CONFIG_VERSION, Cmp::Ge) {
                Ok(false) => {
                    warn!(target: "lazymc::config", "Config is for older lazymc version, you may need to update it")
                }
                Err(_) => {
                    warn!(target: "lazymc::config", "Config version is invalid, you may need to update it")
                }
                Ok(true) => {}
            },
        }

        if let Some(p) = path {
            config.path.replace(p);
        }

        Ok(config)
    }
}

/// Public configuration.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Public {
    /// Public address.
    #[serde(deserialize_with = "to_socket_addrs")]
    pub address: SocketAddr,

    /// Minecraft protocol version name hint.
    pub version: String,

    /// Minecraft protocol version hint.
    pub protocol: u32,
}

impl Default for Public {
    fn default() -> Self {
        Self {
            address: "0.0.0.0:25565".parse().unwrap(),
            version: proto::PROTO_DEFAULT_VERSION.to_string(),
            protocol: proto::PROTO_DEFAULT_PROTOCOL,
        }
    }
}

/// Server configuration.
#[derive(Debug, Deserialize)]
pub struct Server {
    /// Server directory.
    ///
    /// Private because you should use `Server::server_directory()` instead.
    #[serde(default = "option_pathbuf_dot")]
    directory: Option<PathBuf>,

    /// Start command.
    pub command: String,

    /// Server address.
    #[serde(
        deserialize_with = "to_socket_addrs",
        default = "server_address_default"
    )]
    pub address: SocketAddr,

    /// Freeze the server process instead of restarting it when no players online, making it start up faster.
    /// Only works on Unix (Linux or MacOS)
    #[serde(default = "bool_true")]
    pub freeze_process: bool,

    /// Immediately wake server when starting lazymc.
    #[serde(default)]
    pub wake_on_start: bool,

    /// Immediately wake server after crash.
    #[serde(default)]
    pub wake_on_crash: bool,

    /// Probe required server details when starting lazymc, wakes server on start.
    #[serde(default)]
    pub probe_on_start: bool,

    /// Whether this server runs forge.
    #[serde(default)]
    pub forge: bool,

    /// Server starting timeout. Force kill server process if it takes longer.
    #[serde(default = "u32_300")]
    pub start_timeout: u32,

    /// Server stopping timeout. Force kill server process if it takes longer.
    #[serde(default = "u32_150")]
    pub stop_timeout: u32,

    /// To wake server, user must be in server whitelist if enabled on server.
    #[serde(default = "bool_true")]
    pub wake_whitelist: bool,

    /// Block banned IPs as listed in banned-ips.json in server directory.
    #[serde(default = "bool_true")]
    pub block_banned_ips: bool,

    /// Drop connections from banned IPs.
    #[serde(default)]
    pub drop_banned_ips: bool,

    /// Add HAProxy v2 header to proxied connections.
    #[serde(default)]
    pub send_proxy_v2: bool,
}

impl Server {
    /// Get the server directory.
    ///
    /// This does not check whether it exists.
    pub fn server_directory(config: &Config) -> Option<PathBuf> {
        // Get directory, relative to config directory if known
        match config.path.as_ref().and_then(|p| p.parent()) {
            Some(config_dir) => Some(config_dir.join(config.server.directory.as_ref()?)),
            None => config.server.directory.clone(),
        }
    }
}

/// Time configuration.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Time {
    /// Sleep after number of seconds.
    pub sleep_after: u32,

    /// Minimum time in seconds to stay online when server is started.
    #[serde(default, alias = "minimum_online_time")]
    pub min_online_time: u32,
}

impl Default for Time {
    fn default() -> Self {
        Self {
            sleep_after: 60,
            min_online_time: 60,
        }
    }
}

/// MOTD configuration.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Motd {
    /// MOTD when server is sleeping.
    pub sleeping: String,

    /// MOTD when server is starting.
    pub starting: String,

    /// MOTD when server is stopping.
    pub stopping: String,

    /// Use MOTD from Minecraft server once known.
    pub from_server: bool,
}

impl Default for Motd {
    fn default() -> Self {
        Self {
            sleeping: "☠ Server is sleeping\n§2☻ Join to start it up".into(),
            starting: "§2☻ Server is starting...\n§7⌛ Please wait...".into(),
            stopping: "☠ Server going to sleep...\n⌛ Please wait...".into(),
            from_server: false,
        }
    }
}

/// Join method types.
#[derive(Debug, Deserialize, Copy, Clone, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Method {
    /// Kick client with message.
    Kick,

    /// Hold client connection until server is ready.
    Hold,

    /// Forward connection to another host.
    Forward,

    /// Keep client in temporary fake lobby until server is ready.
    Lobby,
}

/// Join configuration.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Join {
    /// Join methods.
    pub methods: Vec<Method>,

    /// Join kick configuration.
    #[serde(default)]
    pub kick: JoinKick,

    /// Join hold configuration.
    #[serde(default)]
    pub hold: JoinHold,

    /// Join forward configuration.
    #[serde(default)]
    pub forward: JoinForward,

    /// Join lobby configuration.
    #[serde(default)]
    pub lobby: JoinLobby,
}

impl Default for Join {
    fn default() -> Self {
        Self {
            methods: vec![Method::Hold, Method::Kick],
            kick: Default::default(),
            hold: Default::default(),
            forward: Default::default(),
            lobby: Default::default(),
        }
    }
}

/// Join kick configuration.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct JoinKick {
    /// Kick message when server is starting.
    pub starting: String,

    /// Kick message when server is stopping.
    pub stopping: String,
}

impl Default for JoinKick {
    fn default() -> Self {
        Self {
            starting: "Server is starting... §c♥§r\n\nThis may take some time.\n\nPlease try to reconnect in a minute.".into(),
            stopping: "Server is going to sleep... §7☠§r\n\nPlease try to reconnect in a minute to wake it again.".into(),
        }
    }
}

/// Join hold configuration.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct JoinHold {
    /// Hold client for number of seconds on connect while server starts.
    pub timeout: u32,
}

impl Default for JoinHold {
    fn default() -> Self {
        Self { timeout: 25 }
    }
}

/// Join forward configuration.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct JoinForward {
    /// IP and port to forward to.
    #[serde(deserialize_with = "to_socket_addrs")]
    pub address: SocketAddr,

    /// Add HAProxy v2 header to proxied connections.
    #[serde(default)]
    pub send_proxy_v2: bool,
}

impl Default for JoinForward {
    fn default() -> Self {
        Self {
            address: "127.0.0.1:25565".parse().unwrap(),
            send_proxy_v2: false,
        }
    }
}
/// Join lobby configuration.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct JoinLobby {
    /// Hold client in lobby for number of seconds on connect while server starts.
    pub timeout: u32,

    /// Message banner in lobby shown to client.
    pub message: String,

    /// Sound effect to play when server is ready.
    pub ready_sound: Option<String>,
}

impl Default for JoinLobby {
    fn default() -> Self {
        Self {
            timeout: 10 * 60,
            message: "§2Server is starting\n§7⌛ Please wait...".into(),
            ready_sound: Some("block.note_block.chime".into()),
        }
    }
}

/// Lockout configuration.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Lockout {
    /// Enable to prevent everybody from connecting through lazymc. Instantly kicks player.
    pub enabled: bool,

    /// Kick players with following message.
    pub message: String,
}

impl Default for Lockout {
    fn default() -> Self {
        Self {
            enabled: false,
            message: "Server is closed §7☠§r\n\nPlease come back another time.".into(),
        }
    }
}

/// RCON configuration.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Rcon {
    /// Enable sleeping server through RCON.
    pub enabled: bool,

    /// Server RCON port.
    pub port: u16,

    /// Server RCON password.
    pub password: String,

    /// Randomize server RCON password on each start.
    pub randomize_password: bool,

    /// Add HAProxy v2 header to RCON connections.
    pub send_proxy_v2: bool,
}

impl Default for Rcon {
    fn default() -> Self {
        Self {
            enabled: cfg!(windows),
            port: 25575,
            password: "".into(),
            randomize_password: true,
            send_proxy_v2: false,
        }
    }
}

/// Advanced configuration.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Advanced {
    /// Rewrite server.properties.
    pub rewrite_server_properties: bool,
}

impl Default for Advanced {
    fn default() -> Self {
        Self {
            rewrite_server_properties: true,
        }
    }
}

/// Config configuration.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct ConfigConfig {
    /// Configuration for lazymc version.
    pub version: Option<String>,
}

fn option_pathbuf_dot() -> Option<PathBuf> {
    Some(".".into())
}

fn server_address_default() -> SocketAddr {
    "127.0.0.1:25566".parse().unwrap()
}

fn u32_300() -> u32 {
    300
}

fn u32_150() -> u32 {
    300
}

fn bool_true() -> bool {
    true
}

/// Collect all `LAZYMC_` environment variables into a nested TOML table.
///
/// Variable names are split on `__` (double underscore) to form nested keys.
/// For example, `LAZYMC_SERVER__ADDRESS` becomes `server.address`.
fn collect_env_config() -> toml::Value {
    let mut root = Map::new();

    for (key, value) in env::vars() {
        if let Some(suffix) = key.strip_prefix(ENV_PREFIX) {
            if suffix.is_empty() {
                continue;
            }
            let parts: Vec<String> = suffix.split(ENV_SEPARATOR).map(|s| s.to_lowercase()).collect();
            let toml_val = infer_toml_value(&value);
            insert_nested(&mut root, &parts, toml_val);
        }
    }

    toml::Value::Table(root)
}

/// Recursively insert a value into nested TOML tables given a list of key parts.
fn insert_nested(table: &mut Map<String, toml::Value>, keys: &[String], value: toml::Value) {
    match keys.len() {
        0 => {}
        1 => {
            table.insert(keys[0].clone(), value);
        }
        _ => {
            let entry = table
                .entry(keys[0].clone())
                .or_insert_with(|| toml::Value::Table(Map::new()));
            if let toml::Value::Table(ref mut sub) = entry {
                insert_nested(sub, &keys[1..], value);
            }
        }
    }
}

/// Infer the TOML type from a string value.
///
/// - Wrapped in `[`…`]` → Array (split on commas, infer each element)
/// - `"true"`/`"false"` → Boolean
/// - Parseable as `i64` → Integer
/// - Contains `.` (no `,`) and parseable as `f64` → Float
/// - Contains `,` → Array (split on commas, infer each element)
/// - Otherwise → String
fn infer_toml_value(s: &str) -> toml::Value {
    // Bracket-wrapped array: [value] or [a, b, c]
    // Allows explicit single-element arrays like [kick] that would otherwise
    // be inferred as a plain string.
    let trimmed = s.trim();
    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        let inner = &trimmed[1..trimmed.len() - 1];
        let items: Vec<toml::Value> = inner
            .split(',')
            .map(|item| item.trim())
            .filter(|item| !item.is_empty())
            .map(|item| infer_toml_value(item))
            .collect();
        return toml::Value::Array(items);
    }

    // Boolean
    if s.eq_ignore_ascii_case("true") {
        return toml::Value::Boolean(true);
    }
    if s.eq_ignore_ascii_case("false") {
        return toml::Value::Boolean(false);
    }

    // Integer
    if let Ok(i) = s.parse::<i64>() {
        return toml::Value::Integer(i);
    }

    // Float (only if contains '.' but no ',')
    if s.contains('.') && !s.contains(',') {
        if let Ok(f) = s.parse::<f64>() {
            return toml::Value::Float(f);
        }
    }

    // Comma-separated array
    if s.contains(',') {
        let items: Vec<toml::Value> = s.split(',').map(|item| infer_toml_value(item.trim())).collect();
        return toml::Value::Array(items);
    }

    // Default: String — unescape common escape sequences so that environment
    // variables work the same as TOML basic strings (e.g. literal `\n` becomes
    // a real newline). This is especially important for panels like Pterodactyl
    // that pass env var values verbatim.
    toml::Value::String(unescape_basic(s))
}

/// Unescape common backslash escape sequences in a string (`\n`, `\t`, `\\`, `\r`).
fn unescape_basic(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Recursively merge two TOML values. Overlay values win; nested tables are merged.
fn deep_merge(base: toml::Value, overlay: toml::Value) -> toml::Value {
    match (base, overlay) {
        (toml::Value::Table(mut base_map), toml::Value::Table(overlay_map)) => {
            for (key, overlay_val) in overlay_map {
                let merged = match base_map.remove(&key) {
                    Some(base_val) => deep_merge(base_val, overlay_val),
                    None => overlay_val,
                };
                base_map.insert(key, merged);
            }
            toml::Value::Table(base_map)
        }
        // When the base is an array but the overlay is a scalar (e.g. env var
        // with a single value like "kick"), auto-wrap it into a single-element
        // array so it deserializes correctly into Vec<T> fields.
        (toml::Value::Array(_), overlay) if !matches!(overlay, toml::Value::Array(_)) => {
            toml::Value::Array(vec![overlay])
        }
        (_, overlay) => overlay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_infer_toml_value_boolean() {
        assert_eq!(infer_toml_value("true"), toml::Value::Boolean(true));
        assert_eq!(infer_toml_value("false"), toml::Value::Boolean(false));
        assert_eq!(infer_toml_value("TRUE"), toml::Value::Boolean(true));
        assert_eq!(infer_toml_value("False"), toml::Value::Boolean(false));
    }

    #[test]
    fn test_infer_toml_value_integer() {
        assert_eq!(infer_toml_value("42"), toml::Value::Integer(42));
        assert_eq!(infer_toml_value("0"), toml::Value::Integer(0));
        assert_eq!(infer_toml_value("-10"), toml::Value::Integer(-10));
    }

    #[test]
    fn test_infer_toml_value_float() {
        assert_eq!(infer_toml_value("3.14"), toml::Value::Float(3.14));
    }

    #[test]
    fn test_infer_toml_value_ip_address_is_string() {
        // IP addresses like 127.0.0.1:25565 should not parse as float
        assert_eq!(
            infer_toml_value("127.0.0.1:25565"),
            toml::Value::String("127.0.0.1:25565".into())
        );
    }

    #[test]
    fn test_infer_toml_value_string() {
        assert_eq!(
            infer_toml_value("hello world"),
            toml::Value::String("hello world".into())
        );
        assert_eq!(
            infer_toml_value("java -jar server.jar"),
            toml::Value::String("java -jar server.jar".into())
        );
    }

    #[test]
    fn test_infer_toml_value_comma_array() {
        let val = infer_toml_value("hold,kick");
        assert_eq!(
            val,
            toml::Value::Array(vec![
                toml::Value::String("hold".into()),
                toml::Value::String("kick".into()),
            ])
        );
    }

    #[test]
    fn test_infer_toml_value_comma_array_integers() {
        let val = infer_toml_value("1,2,3");
        assert_eq!(
            val,
            toml::Value::Array(vec![
                toml::Value::Integer(1),
                toml::Value::Integer(2),
                toml::Value::Integer(3),
            ])
        );
    }

    #[test]
    fn test_deep_merge_basic() {
        let base: toml::Value = toml::from_str(
            r#"
            [server]
            command = "java -jar server.jar"
            address = "127.0.0.1:25566"
            "#,
        )
        .unwrap();

        let overlay: toml::Value = toml::from_str(
            r#"
            [server]
            address = "127.0.0.1:25577"
            "#,
        )
        .unwrap();

        let merged = deep_merge(base, overlay);
        let table = merged.as_table().unwrap();
        let server = table["server"].as_table().unwrap();
        assert_eq!(server["command"].as_str().unwrap(), "java -jar server.jar");
        assert_eq!(server["address"].as_str().unwrap(), "127.0.0.1:25577");
    }

    #[test]
    fn test_deep_merge_adds_new_keys() {
        let base: toml::Value = toml::from_str(
            r#"
            [server]
            command = "java -jar server.jar"
            "#,
        )
        .unwrap();

        let overlay: toml::Value = toml::from_str(
            r#"
            [rcon]
            password = "secret"
            "#,
        )
        .unwrap();

        let merged = deep_merge(base, overlay);
        let table = merged.as_table().unwrap();
        assert_eq!(table["server"]["command"].as_str().unwrap(), "java -jar server.jar");
        assert_eq!(table["rcon"]["password"].as_str().unwrap(), "secret");
    }

    #[test]
    fn test_insert_nested() {
        let mut root = Map::new();
        insert_nested(
            &mut root,
            &["server".into(), "address".into()],
            toml::Value::String("127.0.0.1:25566".into()),
        );
        insert_nested(
            &mut root,
            &["server".into(), "command".into()],
            toml::Value::String("java -jar server.jar".into()),
        );
        insert_nested(
            &mut root,
            &["rcon".into(), "password".into()],
            toml::Value::String("secret".into()),
        );

        let server = root["server"].as_table().unwrap();
        assert_eq!(server["address"].as_str().unwrap(), "127.0.0.1:25566");
        assert_eq!(server["command"].as_str().unwrap(), "java -jar server.jar");
        assert_eq!(root["rcon"]["password"].as_str().unwrap(), "secret");
    }

    #[test]
    fn test_collect_env_config() {
        // Set test env vars
        env::set_var("LAZYMC_SERVER__COMMAND", "java -jar test.jar");
        env::set_var("LAZYMC_SERVER__ADDRESS", "127.0.0.1:25577");
        env::set_var("LAZYMC_RCON__ENABLED", "true");

        let value = collect_env_config();
        let table = value.as_table().unwrap();

        let server = table["server"].as_table().unwrap();
        assert_eq!(
            server["command"].as_str().unwrap(),
            "java -jar test.jar"
        );
        assert_eq!(
            server["address"].as_str().unwrap(),
            "127.0.0.1:25577"
        );
        assert_eq!(table["rcon"]["enabled"].as_bool().unwrap(), true);

        // Clean up
        env::remove_var("LAZYMC_SERVER__COMMAND");
        env::remove_var("LAZYMC_SERVER__ADDRESS");
        env::remove_var("LAZYMC_RCON__ENABLED");
    }

    #[test]
    fn test_infer_toml_value_bracket_single_element_array() {
        let val = infer_toml_value("[kick]");
        assert_eq!(
            val,
            toml::Value::Array(vec![toml::Value::String("kick".into())])
        );
    }

    #[test]
    fn test_infer_toml_value_bracket_multi_element_array() {
        let val = infer_toml_value("[hold, kick]");
        assert_eq!(
            val,
            toml::Value::Array(vec![
                toml::Value::String("hold".into()),
                toml::Value::String("kick".into()),
            ])
        );
    }

    #[test]
    fn test_infer_toml_value_bracket_empty_array() {
        let val = infer_toml_value("[]");
        assert_eq!(val, toml::Value::Array(vec![]));
    }

    #[test]
    fn test_deep_merge_scalar_into_array() {
        let base: toml::Value = toml::from_str(
            r#"
            [join]
            methods = ["hold", "kick"]
            "#,
        )
        .unwrap();

        let overlay: toml::Value = toml::from_str(
            r#"
            [join]
            methods = "kick"
            "#,
        )
        .unwrap();

        let merged = deep_merge(base, overlay);
        let methods = merged["join"]["methods"].as_array().unwrap();
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].as_str().unwrap(), "kick");
    }
}
