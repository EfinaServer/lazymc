# Changelog

## 0.2.20 (2026-04-03)

- Log lazymc version on startup at INFO level
- Fix: server could go to sleep with players online when the server or a plugin
  hides the real player count in the status response (always reporting `0/X`)
- Add lenient JSON status parser for modded servers (Forge, NeoForge, Fabric)
  that return non-standard status responses (e.g. `description` as a Chat
  Component object). The chosen parser (strict/lenient) is cached and tried
  first on every poll to avoid repeated failed decode attempts
- Add `rcon.player_count_cross_check` config option (default `true`): when RCON
  is enabled, periodically verify player count via RCON `list` command whenever
  status reports 0 players, guarding against hidden player counts
- Fix RCON `list` response parser not handling the `X/Y` player count format
  used by some plugins, and not stripping Minecraft color codes (e.g. `§c3`)
  which caused the count to be read as 0
- Add timeout on RCON player count queries to prevent the monitor loop stalling
  if the server is unresponsive
- Throttle RCON player count cross-checks to at most once per 10 seconds
- Escalate RCON failure log from `WARN` to `ERROR` after 3 consecutive failures
- Improve debug/trace logging: player count changes, sleep check idle timer
  progress, and parser switches are now clearly logged

## 0.2.19 (2026-03-01)

- Fix single-value env var arrays failing to deserialize into `Vec` fields; add
  bracket syntax (`[kick]`) to force a single-element array, and auto-coerce a
  plain scalar to an array when the target config field is an array type

## 0.2.18 (2026-02-28)

- Fix Pterodactyl panel (and similar hosted-console) stdin forwarding: replace
  the per-server-invocation stdin reader with a single persistent global stdin
  reader that runs for lazymc's entire lifetime, preventing zombie threads from
  stealing console input on server restarts

## 0.2.17 (2026-02-28)

- Add stdin stop command: write `stop` to the server's stdin as the primary
  non-RCON graceful shutdown method, working reliably on all server types
  including modded servers where SIGTERM may not trigger a clean shutdown
- Forward lazymc's own stdin to the server process so server console commands
  typed in the terminal continue to work

## 0.2.16 (2026-02-28)

- Fix server not gracefully shutting down on modded servers (Forge, NeoForge,
  Fabric): add stdin-based `stop` method, send signals to the process group
  (with PID fallback for wrapper scripts), and fix `freeze_server_signal`
  masking failures and preventing fallback to other stop methods
- Shutdown method priority is now: freeze → RCON `stop` → stdin `stop` → SIGTERM

## 0.2.15 (2026-02-28)

- Add lenient JSON parser for modded server status responses; handles
  Forge/NeoForge/Fabric servers that return a `description` field as a Chat
  Component object instead of a plain string, extracting player count, version
  and MOTD from any valid JSON structure
- Use RCON to query player count when status polling fails; prevents the server
  from sleeping while players are connected when modded servers return broken
  or unparseable status responses

## 0.2.14 (2026-02-28)

- Fix modded servers (NeoForge, Forge, Fabric) getting stuck in Starting state
  when status fetches fail or time out; ping fallback now also runs during
  Starting and a successful ping triggers the Started transition
- Fix literal `\n`, `\t`, `\r`, `\\` escape sequences in MOTD and messages
  configured via environment variables (e.g. from Pterodactyl Panel)

## 0.2.13 (2026-02-28)

- Add `--public-address` CLI flag to set `public.address`; takes precedence
  over both environment variables and the config file

## 0.2.12 (2026-02-28)

- Add `LAZYMC_` environment variable configuration; configure lazymc entirely
  without a config file using the `LAZYMC_` prefix with `__` as a section
  separator (e.g. `LAZYMC_SERVER__COMMAND`); env vars override config file
  values when both are present; removes the previous `${VAR}` in-file
  substitution feature
- Add manual release workflow for automated multi-platform builds (Linux, macOS,
  Windows; x86_64 and aarch64)

## 0.2.11 (2024-03-16)

- Add support for Minecraft 1.20.3 and 1.20.4
- Improve error handling of parsing server favicon
- Fix typo in log message
- Update dependencies

## 0.2.10 (2023-02-20)

- Do not report an error when server exits with status code 143

## 0.2.9 (2023-02-14)

- Fix dropping all connections when `server.drop_banned_ips` was enabled
- Update dependencies

## 0.2.8 (2023-01-30)

- Add `freeze_process` feature on Unix platforms to freeze a sleeping server
  rather than shutting it down.
- Update default Minecraft version to 1.19.3
- Remove macOS builds from releases, users can compile from source
- Update dependencies

## 0.2.7 (2021-12-13)

- Update default Minecraft version to 1.18.1
- Update dependencies

## 0.2.6 (2021-11-28)

- Add whitelist support, use server whitelist to prevent unknown users from waking server
- Update dependencies

## 0.2.5 (2021-11-25)

- Add support Minecraft 1.16.3 to 1.17.1 with lobby join method
- Add support for Forge client/server to lobby join method (partial)
- Probe server on start with fake user to fetch server settings improving compatibility
- Improve lobby compatibility, send probed server data to client when possible
- Skip lobby join method if server probe is not yet finished
- Generate lobby dimension configuration on the fly based on server dimensions
- Fix unsupported lobby dimension configuration values for some Minecraft versions
- Demote IP ban list reload message from info to debug
- Update dependencies

## 0.2.4 (2021-11-24)

- Fix status response issues with missing server icon, fall back to default icon
- Fix incorrect UUID for players in lobby logic
- Make server directory relative to configuration file path
- Assume SIGTERM exit code for server process to be successful on Unix
- Update features in README
- Update dependencies

## 0.2.3 (2021-11-22)

- Add support for `PROXY` header to notify Minecraft server of real client IP
- Only enable RCON by default on Windows
- Update dependencies

## 0.2.2 (2021-11-18)

- Add server favicon to status response

## 0.2.1 (2021-11-17)

- Add support for using host names in config address fields
- Handle banned players within `lazymc` based on server `banned-ips.json`
- Update dependencies

## 0.2.0 (2021-11-15)

- Add lockout feature, enable to kick all connecting clients with a message
- Add option to configure list of join methods to occupy client with while server is starting (kick, hold, forward, lobby)
- Add lobby join method, keeps client in lobby world on emulated server, teleports to real server when it is ready (highly experimental)
- Add forward join method to forward (proxy) client to other host while server is starting
- Restructure `lazymc.toml` configuration
- Increase packet reading buffer size to speed things up
- Add support for Minecraft packet compression
- Show warning if config version is outdated or invalid
- Various fixes and improvements

## 0.1.3 (2021-11-15)

- Fix binary release

## 0.1.2 (2021-11-15)

- Add Linux ARMv7 and aarch64 releases
- RCON now works if server is running while server command already quit
- Various RCON tweaks in an attempt to make it more robust and reliable (cooldown, exclusive lock, invocation spacing)
- Increase server monitoring timeout to 20 seconds
- Improve waiting for server logic when holding client
- Various fixes and improvements

## 0.1.1 (2021-11-14)

- Make server sleeping errors more descriptive
- Add server quit cooldown period, intended to prevent RCON errors due to RCON
  server thread something quitting after main server
- Rewrite `enable-status = true` in `server.properties`
- Rewrite `prevent-proxy-connections = false` in `server.properties` if
  Minecraft server has non-loopback address (other public IP)
- Add compile from source instructions to README
- Add Windows instructions to README
- Update dependencies
- Various fixes and improvements

## 0.1.0 (2021-11-11)

- Initial release
