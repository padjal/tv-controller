# Contributing

Thanks for taking a look. This is a small project with a specific shape, so
this file is mostly about the things that are easy to get wrong rather than a
long process document.

## Before you start

If you are proposing a behaviour change rather than a fix, open an issue first.
Much of this system's behaviour is the way it is on purpose — looping video,
the short timeout for offline devices, the lack of authentication — and the
reasoning is written down in [CLAUDE.md](CLAUDE.md) under *Known issues /
decisions*. It is worth skimming before you conclude something is a bug. It is
also the best map of the codebase there is.

## Setting up

You need a Rust toolchain, Node 20+, and `ffmpeg` (for `ffprobe` and thumbnail
generation; both are optional at runtime, but you want them to exercise the
scanner properly).

```bash
cp server/.env.example server/.env    # SERVER_BASE_URL is required
cargo run -p server                   # http://localhost:8000

cd dashboard
npm ci
npm run dev                           # proxies /api and /videos to :8000
```

Drop a couple of small `.mp4` files into `videos/` so the scanner has something
to index.

You do **not** need a Raspberry Pi to work on most of this. Anything answering
`POST /play`, `/pause`, `/resume`, `/stop` on port 8080 will stand in for an
agent as far as the server is concerned.

## Before you open a pull request

```bash
cargo test --workspace
cargo clippy -- -D warnings           # warnings are errors here, no exceptions

cd dashboard
npm run typecheck
npm test
npm run build
```

All five must pass. CI runs the same commands.

## House rules

These are enforced by review, and mostly by clippy:

- **No `unwrap()` or `expect()` outside tests.** Errors are `anyhow::Result`
  and they propagate.
- **Handlers return `Json<T>`**, never a bare string. Errors go through
  `server/src/error.rs` so every failure is JSON.
- **Database access lives in `server/src/db.rs`.** No inline queries in
  handlers.
- **Changing a type in `shared/` means regenerating the TypeScript.** Run
  `cargo test -p shared`, which rewrites `dashboard/src/types/*.ts`, and commit
  the result. If you add a type, add it to the barrel in
  `dashboard/src/types/index.ts` too.
- **64-bit integer fields need `#[ts(type = "number")]`.** ts-rs maps `i64` and
  `u64` to `bigint`, which `JSON.parse` never produces, so the generated type
  would not match the wire.
- **New endpoints get a script** under `scripts/test/` — see
  [scripts/test/README.md](scripts/test/README.md).

## Testing what needs hardware

A meaningful slice of this project can only really be verified against a real
Pi driving a real screen: display detection in `setup_pi.sh`, mpv's behaviour
on KMS/DRM, streaming under load. If your change touches that path, say in the
pull request what you tested it on — Pi model, Raspberry Pi OS version, and
whether the session was Wayland, X11 or Lite. If you could not test it on
hardware, say that instead. An honest "untested on a Pi" is far more useful
than an assumption.

The same goes for the dashboard: the tests are jsdom, so they cover behaviour
but not layout. Mention it if you changed how something looks.

## Commit messages

Write the subject as what the change does, in the imperative — "Clear pause
when starting playback", not "fix". If the change encodes a non-obvious
decision, put the reasoning in the body, and consider adding a line to
CLAUDE.md's decisions list so the next person finds it.
