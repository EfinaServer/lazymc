[![Build status on GitLab CI][gitlab-ci-master-badge]][gitlab-ci-link]
[![Project license][license-badge]](LICENSE)

[gitlab-ci-link]: https://gitlab.com/timvisee/lazymc/pipelines
[gitlab-ci-master-badge]: https://gitlab.com/timvisee/lazymc/badges/master/pipeline.svg
[license-badge]: https://img.shields.io/github/license/timvisee/lazymc

# lazymc

`lazymc` puts your Minecraft server to rest when idle, and wakes it up when
players connect.

Some Minecraft servers (especially modded) use an insane amount of resources
when nobody is playing. lazymc helps by stopping your server when idle, until a
player connects again.

lazymc functions as proxy between clients and the server. It handles all
incoming status connections until the server is started and then transparently
relays/proxies the rest. All without them noticing.

https://user-images.githubusercontent.com/856222/141378688-882082be-9efa-4cfe-81cc-5a7ab8b8e86b.mp4


<details><summary>Click to see screenshots</summary>
<p>

![Sleeping server](./res/screenshot/sleeping.png)
![Join sleeping server](./res/screenshot/join.png)
![Starting server](./res/screenshot/starting.png)
![Started server](./res/screenshot/started.png)

</p>
</details>

## Features

- Very efficient, lightweight & low-profile (~3KB RAM)
- Supports Minecraft Java Edition 1.20.3+
- Modded server support (Forge, NeoForge, Fabric) with lenient status parsing
- Configure joining client occupation methods:
  - Hold: hold clients when server starts, relay when ready, without them noticing
  - Kick: kick clients when server starts, with a starting message
  - Forward: forward client to another IP when server starts
  - _Lobby: keep client in emulated server with lobby world, teleport to real server when ready ([experimental*](./docs/join-method-lobby.md))_
- Customizable MOTD and login messages
- Automatically manages `server.properties` (host, port and RCON settings)
- Automatically block banned IPs from server within lazymc
- Graceful server sleep/shutdown through stdin `stop` command, RCON or `SIGTERM`
- RCON player count fallback when status polling fails
- Real client IP on Minecraft server with `PROXY` header ([usage](./docs/proxy-ip.md))
- Restart server on crash
- Lockout mode

## Requirements

- Linux, macOS or Windows
- Minecraft Java Edition 1.6+
- On Windows: RCON (automatically managed)

Build requirements:

- Rust 1.74 (MSRV)

_Note: You must have access to the system to run the `lazymc` binary. If you're
using a Minecraft shared hosting provider with a custom dashboard, you likely
won't be able to set this up._

## Usage

_Note: these instructions are for Linux & macOS, for Windows look
[here](./docs/usage-windows.md)._

Make sure you meet all [requirements](#requirements).

Download the appropriate binary for your system from the [latest
release][latest-release] page. On macOS you must [compile from
source](#compile-from-source).

Place the binary in your Minecraft server directory, rename it if you like.
Open a terminal, go to the directory, and make sure you can invoke it:

```bash
chmod a+x ./lazymc
./lazymc --help
```

When lazymc is set-up, change into your server directory if you haven't already.
Then set up the [configuration](./res/lazymc.toml) and start it up:

```bash
# Change into your server directory (if you haven't already)
cd server

# Generate lazymc configuration
lazymc config generate

# Edit configuration
# Set the correct server address, directory and start command
nano lazymc.toml

# Start lazymc
lazymc start
```

Please see [extras](./docs/extras.md) for recommendations and additional things
to set up (e.g. how to fix incorrect client IPs and IP banning on your server).

After you've read through the [extras](./docs/extras.md), everything should now
be ready to go! Connect with your Minecraft client to wake your server up!

## Environment variables

You can configure lazymc entirely through environment variables, without a
config file at all. This is ideal for Docker and CI/CD deployments.

Use the `LAZYMC_` prefix with `__` (double underscore) as a section separator.
Variable names are uppercased; they map to the corresponding lowercase TOML
keys.

| Environment variable | Config equivalent |
|---|---|
| `LAZYMC_SERVER__COMMAND` | `server.command` |
| `LAZYMC_SERVER__ADDRESS` | `server.address` |
| `LAZYMC_PUBLIC__ADDRESS` | `public.address` |
| `LAZYMC_SERVER__FREEZE_PROCESS` | `server.freeze_process` |
| `LAZYMC_RCON__PASSWORD` | `rcon.password` |
| `LAZYMC_JOIN__KICK__STARTING` | `join.kick.starting` |
| `LAZYMC_JOIN__METHODS` | `join.methods` (comma-separated: `hold,kick`) |

For array values, comma-separated strings are split automatically (e.g.
`hold,kick`). To specify a single-element array, wrap the value in brackets
(e.g. `[kick]`), otherwise a lone value like `kick` is interpreted as a plain
string and will fail to deserialize into an array field.

Values are automatically inferred: `true`/`false` become booleans, numeric
strings become integers, and comma-separated values become arrays. Escape
sequences (`\n`, `\t`, `\\`) in string values are interpreted as their
actual characters, so MOTD and messages work correctly with panels like
Pterodactyl.

When both a config file and `LAZYMC_` env vars are present, the env vars
override the file values.

**Docker example:**

```bash
docker run -e LAZYMC_SERVER__COMMAND="java -jar server.jar" \
           -e LAZYMC_SERVER__DIRECTORY="." \
           -e LAZYMC_PUBLIC__ADDRESS="0.0.0.0:25565" \
           -e LAZYMC_RCON__PASSWORD="s3cr3t" \
           lazymc start
```

## Modded server support

lazymc works with modded servers (Forge, NeoForge, Fabric, etc.) out of the
box. Modded servers sometimes return non-standard status responses that differ
from vanilla Minecraft (e.g. the `description` field as a Chat Component object
instead of a plain string). lazymc handles this transparently with a multi-layer
detection strategy:

1. **Strict protocol decode** — standard Minecraft status response parsing
2. **Lenient JSON parser** — fallback that extracts player count, version, MOTD
   and other fields from any valid JSON status response, regardless of format
3. **Ping fallback** — confirms the server is alive when status parsing fails
   entirely
4. **RCON player count query** — when RCON is enabled, queries online players
   via the `list` command as a last resort to prevent premature server shutdown

This ensures the server is correctly detected as online and won't be shut down
while players are connected, even if the status response format is non-standard.

### Graceful shutdown

When putting the server to sleep, lazymc uses multiple shutdown methods in order
of preference:

1. **Freeze** (Unix, if enabled) — suspends the process with `SIGSTOP` for fast resume
2. **RCON `stop`** (if enabled) — sends the `stop` command over RCON
3. **stdin `stop`** — writes `stop` to the server console, triggering Minecraft's
   built-in shutdown. This works reliably on all server types without requiring RCON
4. **`SIGTERM`** (Unix) — sends a termination signal as a last resort

Console commands typed into lazymc's terminal are forwarded to the server
process, so server administration works as expected.

_Note: If a binary for your system isn't provided, please [compile from
source](#compile-from-source). Installation options are limited at this moment. More will be added
later._

[latest-release]: https://github.com/timvisee/lazymc/releases/latest

## Compile from source

Make sure you meet all [requirements](#requirements).

To compile from source you need Rust, install it through `rustup`: https://rustup.rs/

When Rust is installed, compile and install `lazymc` from this git repository
directly:

```bash
# Compile and install lazymc from source
cargo install -f --git https://github.com/timvisee/lazymc

# Ensure lazymc works
lazymc --help
```

Or clone the repository and build it yourself:

```bash
# Clone repository
git clone https://github.com/timvisee/lazymc
cd lazymc

# Compile
cargo build --release

# Run lazymc
./target/release/lazymc --help
```

## Third-party usage & implementations

A list of third-party implementations, projects using lazymc, that you might
find useful:

- Docker: [crbanman/papermc-lazymc](https://hub.docker.com/r/crbanman/papermc-lazymc) _(PaperMC with lazymc in Docker)_

## License

This project is released under the GNU GPL-3.0 license.
Check out the [LICENSE](LICENSE) file for more information.
