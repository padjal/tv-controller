# User guide

Everything happens on one page. Open the server's address in a browser —
`http://192.168.1.10:8000`, or whatever your server was given — and you get the
whole control surface: the TVs on the left, the video library on the right, and
the command bar along the bottom.

No login, no install. Any device on the network with a browser works, phones
included.

---

## The three parts of the screen

### TVs

One tile per Raspberry Pi that has registered itself. Each tile shows three
things:

- **The name** you set as `DEVICE_NAME` on that Pi
- **A status badge** — `Idle`, `Playing`, `Paused` or `Offline`
- **What it is playing**, or `—` when it is playing nothing. An offline tile
  shows when it was last seen instead, so you can tell a TV unplugged a minute
  ago from one that has been dark for a week.

Click a tile to select it; click again to deselect. Tiles are real buttons, so
you can also tab to them and hit space — useful if the dashboard lives on a
wall panel with no mouse.

Tiles update themselves. The server checks every TV every 10 seconds and pushes
changes to your browser, so you do not need to refresh — including changes made
from somebody else's browser.

If no Pi has registered yet, the panel says so rather than showing an empty
grid.

### Videos

Every file the server has indexed, with its duration and size. Click a row to
select it; only one video can be selected at a time.

The list follows the server's video directory. Add a file there and it appears
here within a couple of seconds; delete one and it disappears. If the file you
had selected is the one that got deleted, the selection clears itself.

Adding videos is a server-side job — see
[deployment.md](deployment.md#adding-videos).

### The command bar

Fixed along the bottom. It tells you what is currently selected, and holds the
five buttons.

---

## Playing something

1. **Click the TVs** you want. The bar shows a count as you go.
2. **Click a video.**
3. **Click Play.**

The TVs stream the file from the server directly, so it starts at roughly the
same moment on all of them. Each tile flips to `Playing` on its own as the
server confirms it.

### The buttons

| Button | Needs | Does |
| --- | --- | --- |
| **Play** | TVs + a video | Starts that video on every selected TV |
| **Pause** | TVs | Freezes them where they are |
| **Resume** | TVs | Picks up from the pause |
| **Stop** | TVs | Stops playback and clears the TV |
| **Play on all** | a video only | Plays it on every TV that is online |

A button is greyed out until it has what it needs, which is why **Play** stays
disabled until you have chosen both TVs and a video.

**Play on all** ignores your TV selection entirely — the server picks the
targets, which is every device not currently marked offline. It is the button
for "the whole building, now", and it saves selecting twenty tiles to do it.

While a command is in flight, every button is disabled. That is deliberate:
two commands racing to set the same TV's state would leave the dashboard
disagreeing with the wall.

---

## Reading the result

A message appears under the buttons after every command, and clears itself
after a few seconds.

- **"Playing on 4 TVs"** — everything worked.
- **"Playing on 4 of 5; TV-03: connection refused"** — partial success. The
  message names the TVs that failed and why. The other four are playing.
- **"Play failed: …"** — no TV accepted the command.

A partial failure is worth reading rather than dismissing: it tells you exactly
which TV to go look at, and the reason usually says whether the Pi is
unreachable, or reachable but unhappy.

Note that a TV that refuses a command keeps whatever status it had. The
dashboard does not guess — the next heartbeat, within 10 seconds, settles what
that TV is actually doing.

---

## Things worth knowing

**Pause and resume keep the video; stop forgets it.** After a stop, the tile
shows `—` and you need to pick a video again to start it.

**A TV goes `Offline` after 30 seconds of silence.** Powering one back on
brings it back within a poll or two — the agent re-registers itself on boot,
and nothing needs doing on the dashboard.

**Several people can have the dashboard open.** Everyone sees the same state,
and everyone's changes propagate. Selections are per-browser, so two people
selecting different TVs will not interfere with each other — but two people
pressing Play at once will, in the obvious way.

**One case where a refresh helps.** The page loads the current state and then
subscribes to updates, so anything that happens in that split second is missed.
It is rare, and the next real change corrects it. If a tile looks wrong and
nothing else explains it, refresh the page.

**Deleting a TV needs the API.** There is no delete button on the tile — a
device removed from the database while its agent is still running would simply
re-register. Stop the agent first, then:

```bash
curl -X DELETE http://<server>:8000/api/devices/<device-id>
```

Other dashboards already open will keep showing the tile until they are
refreshed.

---

## When something looks wrong

**A tile says `Playing` but the screen is black.** The agent and the server
agree, so this is the Pi's display configuration, not the dashboard. See
[the display section of the deployment guide](deployment.md#if-the-video-does-not-appear-on-screen).

**A tile is stuck `Offline` but the TV is on.** The server cannot reach the Pi.
See [troubleshooting](deployment.md#troubleshooting).

**Nothing updates at all — no tile ever changes.** The event stream from the
server has dropped. Refresh the page; if it keeps happening, check the server
logs for `lagged` warnings.

**The page is blank or 404s.** The server is running but has no dashboard
build. That is a deployment problem — see
[deployment.md](deployment.md#without-docker).
