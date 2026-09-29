# ABB Signal Spy

A Windows app for reading, charting and recording the motion test signals an ABB IRC5 robot controller
streams over its RobAPI InfoStream interface: motor currents and voltages, torques, the DC-link voltage,
resolver angles, joint and motor positions and speeds, and several hundred more.

It only reads. It never commands motion, never writes RAPID, configuration or I/O, and never takes
mastership.

Not affiliated with or endorsed by ABB. ABB and IRC5 are trademarks of ABB.

**Status:** first usable version, in testing. Tested against RobotStudio's RobotWare 6 virtual
controller, an in-process stand-in for the protocol, and a real IRC5 (IRB 2600, RobotWare 6.16).

## Using it

1. **Connect.** A real IRC5 answers on port 5515: type its address and press Connect. A RobotStudio
   virtual controller picks a new port every time it starts: open **List** and pick it there.
2. **One program at a time.** A controller streams test signals to one program at a time. A second
   program is not refused; the two break each other's streams, and the controller can even hand the
   second program's signals to the first under the first one's channel numbers. Close TuneMaster's
   signal logging and RobotStudio's signal tools first. When other programs are connected, ABB Signal
   Spy names them and asks before taking over. If another program takes the stream while it runs, it
   stops and says so, before showing anything that could be the other program's signals, and it does
   not take the stream back. If the network drops, it reconnects by itself: it first waits for the
   controller to let go of the broken connection (about 16 s), and it never takes the stream from a
   program that connected meanwhile (it asks, or stops and says why). If the controller at the address
   turns out to be a different one (a cable moved to the next robot), it stops rather than carry on.
3. **Add channels.** Pick a signal in the catalogue and press Add. The dialog asks only what that signal
   needs: the robot, the axis, or nothing. Up to 12 channels. *Channel sets...* adds a common group in
   one go: the DC links, one robot's torques, joint positions or resolver angles, or the 8000-8009 block.
4. **Read and chart.** Values show as a 150 ms mean (for a joint speed that pads between its values
   with zeros, the mean of the values it reports; for an angle within one turn, the mean on the circle);
   charts show every sample. Space pauses the charts so you can scroll back through the last 10 minutes.
   A value that stops updating is dimmed and marked STALE, never shown as live. Angles are in degrees;
   click the unit to switch to radians.
5. **Record.** REC records every sample. *Save last* saves the last seconds from memory, for when
   something has just happened and nothing was recording. *Slow log* records count, mean, min and max
   per interval, for runs of hours. M drops a marker.
6. **Derived channels**, from a channel's menu (⋯): a resolver angle's **turn to a target** (the short
   way round, ON TARGET within 0.25 degrees), the **PWM duty sum** of an axis (1.50 by construction), and
   the **DC-link sag**: how far the drive's DC-link voltage dips below its resting level (the plateau),
   which you set with the robot armed and still once the link has held level for 20 s (after the
   motors go off it drains slowly, for about 20 minutes). A derived value is never shown as more live
   than its inputs.
7. **Look back.** *File > Open a recording* (or drop its folder on the window) charts a recording again,
   marked REVIEWING. *Save CSV* and *Save PNG* above the charts save what is in view, live or reviewed.
8. **Controller details** (Controller menu, optional): with the controller's RWS login, typed each
   time and never stored, the window names the robot system and its RobotWare version, puts the
   controller's event log on the charts and into recordings (a look every 5 s, which a setting turns
   off), and a turn to target can take its target from the motor's commutator offset. Read-only, and
   checked to be the same controller as the one streaming.
9. **Phone view** (off until switched on) serves a read-only page with the current values to a phone
   on the same network.

The built-in catalogue was measured on an IRB 2600 with RobotWare 6.16. Another robot or RobotWare
version may use other numbers; the controller's refusal is shown per channel, and a catalogue file for
another controller can be loaded (Catalogue menu).

Settings live in `%LOCALAPPDATA%\ABB Signal Spy`, or beside the `.exe` when a `settings.json` is
already there. `--connect HOST[:PORT]` connects at startup.

## Recordings

Each recording is a folder under `Documents\TestSignals`, named for the local date and time it
started, for example `2026-09-26_04-47-20 dc dip`:

- `data.csv`: `controller_ms,channel,value`, one row per sample. `controller_ms` is the controller's own
  clock; `channel` is `signal/unit/joint`, for example `4002/ROB_1/J2` (torque, ROB_1, joint 2).
  Recordings made before 2026-09-27 wrote `4002/ROB_1/2`; this program reads both.
- `slow.csv` (slow logs): `controller_ms,channel,count,mean,min,max`, one row per channel per interval.
- `recording.json`: the controller and its system id, each channel's name, units and sample time,
  reconnects and controller restarts, markers, and whether any samples were lost. `anchors` map the
  controller's clock to UTC: each applies from its `row` (the first data row it maps, counted from 0)
  until the next anchor's, so rows on both sides of a controller restart map correctly. If the
  controller behind the address changes during a recording, the recording is closed there and says so.
  `derived` lists the derived channels shown (their values are not recorded; opening the recording
  computes them again from the recorded inputs).
- `* view.csv` and `* charts.png` beside the recording folders are *Save CSV* and *Save PNG*:
  `time_utc,t_s,channel,name,units,value`, one row per sample, in the units the window shows.

String signals (the work object, the tool's name, a program position) are written quoted, and may
contain commas, quotes and line breaks; any CSV reader, pandas included, reads them correctly.

```python
import json, pandas as pd
meta = json.load(open("recording.json"))
df = pd.read_csv("data.csv")
wide = df.pivot_table(index="controller_ms", columns="channel", values="value")
```

## Console probe

`signal-spy-probe` is the command-line side: `list` finds local virtual controllers, `hello` shows a
controller's connected clients, `stream` records statistics for a set of signals, `typed` reports the
record type of a list of numbers, `selftest` runs against a built-in stand-in, `rws` reads what the
window's controller details read (the password from `SPY_RWS_PASSWORD`). It tears its streams down
on Ctrl+C.

## Building

Rust 1.95 or later, on Windows with the MSVC toolchain:

```
cargo build --release
cargo test --workspace
```

The workspace has three crates: `crates/core` (the protocol, decoder, session, store, catalogue and
recorder, with no user interface), `crates/app` (the window, egui) and `crates/probe` (the console
tool). The catalogue in `catalogue/` is generated from the research data.
