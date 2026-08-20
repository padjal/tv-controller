# Endpoint test scripts

These drive a **running** server or agent over HTTP with `curl` and check the
responses. They are not a substitute for `cargo test` — they exist because the
interesting failures in this system are integration failures: an agent that
answers but renders nothing, a Range request a browser handles differently than
a test client, an SSE stream that dies under a slow consumer.

Everything here needs `curl` and `jq`. Run them from a machine that can reach
the host you are testing — Git Bash on Windows, a terminal on macOS or Linux,
or the Pi itself.

## Against a Pi agent

| Script | What it does |
| --- | --- |
| `pi-agent/smoke.sh [PI_HOST] [VIDEO_PATH]` | The full sequence: health → play → pause → resume → stop |
| `pi-agent/play.sh [PI_HOST] [VIDEO_PATH]` | Start playback and stop there |
| `pi-agent/status.sh [PI_HOST]` | Print what the agent thinks it is doing |

`VIDEO_PATH` is a path or URL the agent hands to mpv, so it has to be reachable
*from the Pi*, not from wherever you are running the script.

## Against the server

| Script | What it does |
| --- | --- |
| `server/devices.sh [SERVER_HOST]` | Register twice (idempotency) → list → get → 404 → 400 → delete |
| `server/videos.sh [SERVER_HOST]` | List → metadata → download → Range → 416 → 404 |
| `server/playback.sh [SERVER_HOST]` | play → pause → resume → stop → play-all, plus the 400/404/502 paths |
| `server/events.sh [SERVER_HOST]` | Tail the SSE stream, one line per event |

`videos.sh` needs at least one file in the server's `VIDEOS_DIR`; it skips the
file tests if the library is empty.

`playback.sh` needs a registered device that actually answers. A real Pi is
ideal, but anything serving `POST /play`, `/pause`, `/resume` and `/stop` on
port 8080 will do — the server does not care what is behind them.

`events.sh` blocks by design. Run it in one terminal and drive the server from
another; every device change, playback command and library scan should show up
in it.

## Defaults

| Variable | Default |
| --- | --- |
| `PI_HOST` | `192.168.1.11` |
| `VIDEO_PATH` | `/tmp/test.mp4` |
| `SERVER_HOST` | `127.0.0.1` (port 8000) |

Override with an environment variable or a positional argument:

```bash
SERVER_HOST=10.0.0.5 ./scripts/test/server/playback.sh
./scripts/test/pi-agent/smoke.sh tv-01.local http://10.0.0.5:8000/videos/clip.mp4
```

## Adding to these

A new endpoint should come with a script here that exercises its success path
and its error codes — the status codes carry meaning in this API (`api.ts`
branches on them), so a script that only checks the happy path misses the part
most likely to regress.
