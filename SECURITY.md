# Security

## Threat model, stated up front

**TV Controller has no authentication of any kind.** This is a deliberate
design decision, not an oversight, and it shapes everything below:

- Anyone who can reach the server can control every TV and download every
  video in the library.
- Anyone who can reach a Pi on port 8080 can drive that TV directly, bypassing
  the server entirely.
- The dashboard, the API and the video files are all served unauthenticated
  over plain HTTP.

The system is built for an isolated LAN — a venue, an office, a signage
network — where physical access to the network is already the access control.

**Do not expose it to the internet or to an untrusted network.** Do not forward
a port to it. If you need to reach it remotely, put it behind a VPN and let the
VPN do the authentication.

Because this is documented and intended, reports that amount to "the API has no
authentication" are not treated as vulnerabilities. Reports that the system
fails to hold the line described above are.

## In scope

Things that would be genuine bugs, and are worth reporting:

- Reading files outside the configured `VIDEOS_DIR` or `THUMBNAILS_DIR` —
  path traversal, symlink escapes, encoded separators. The traversal cases we
  know about are covered by tests in `server/src/handlers/videos.rs`.
- Any way to make the server execute a command, or to inject arguments into
  the `ffprobe`/`ffmpeg` invocations in `server/src/services/video_scan.rs`,
  by controlling a filename in the video library.
- Any way to make an agent load something other than a video URL from its
  configured server, via the `pi-agent` HTTP surface or the mpv IPC socket.
- SQL injection, or crashes and panics reachable from an HTTP request. The
  server should return an error, never fall over.
- Denial of service that is disproportionately cheap — a single request that
  hangs the server or wedges every TV at once.

## Out of scope

- The absence of authentication, authorization, TLS, rate limiting or audit
  logging. See above.
- Anything that requires access to the LAN as its premise, since the LAN is
  the trust boundary.
- Anything requiring physical access to a Pi or an SD card.

## Operational notes that bite people

- `ServeDir` hands out **everything** in `VIDEOS_DIR`, not just video files.
  Keep that directory flat and keep non-video files out of it.
- `/etc/tv-agent/device.id` is a device's identity across reboots. Delete it
  before imaging an SD card, or every Pi cloned from that image registers as
  the same device.
- The server's `.env` holds no credentials today, but it is gitignored for a
  reason — keep it that way if you add any.

## Reporting a vulnerability

Open a [security advisory](https://github.com/padjal/tv-controller/security/advisories/new)
on GitHub, which keeps the report private until it is resolved. If you would
rather not use GitHub, open a regular issue saying only that you have a
security report and asking for a contact address — do not put the details in a
public issue.

This is a small hobby project maintained in spare time. There is no bounty and
no guaranteed response time, but reports will be read and taken seriously.
