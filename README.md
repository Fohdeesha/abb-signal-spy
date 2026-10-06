# ABB Signal Spy

Windows application for seeing what an ABB IRC5 based robot system is really doing. It reads the motion test signals the
controller streams over RobAPI InfoStream (motor currents and torques, DC-link voltage, resolver angles,
joint speeds and a few hundred more), charts them live and records them. UI optimized for in-field use on shitty laptop screens.

Currently working fully with IRC5 / RW6 based cells. Works on the Service, LAN, or WAN ports of the controller. Can be made to work on RW7 / OmniCore, but I'd need someone with access to one to gather some data for me (Virtual RW7 controllers are not enough).

It only ever reads. No motion, no RAPID, no config or I/O writes, no controller mastership grant required. To read, it sets up test-signal streams on the controller, just like TuneMaster does; a signal number the controller doesn't have leaves one entry in its event log, and **controller > reset InfoStream...** clears every program's streams (it asks first, and you should only need it after something crashed).

## Hidden Figures

The core reason I built this: It exposes more than 150 diagnostic signals that TuneMaster or RobotStudio will not show you - including raw resolver angles and other values needed to commutate robot servo motors (ABB, why did you only make these officially available for external axes? You made me build this)

| Signals | Count |
|:--|--:|
| In the catalogue | **407** |
| Documented by ABB (the ones TuneMaster offers) | 57 |
| Hidden, but now defined and identified | ~150 |
| Discovered, not yet identified (maybe you can?) | ~200 |




![Live charts](screenshots/live.png)

## Get it

Grab the latest `.exe` from [Releases](../../releases). No installer, just run it - Windows 10 or 11. If you have a *really* shitty field laptop (Windows 7, 8 or 8.1) grab the exe with `-win7` in the filename.

The exe isn't signed, so Windows SmartScreen may complain the first time: **More info > Run anyway**. `SHA256SUMS.txt` has the checksum of every file in a release. The PC needs to reach the controller on TCP port 5515.

## Use it

1. Enter the controller's address and hit **connect**. RobotStudio virtual controllers show up under **list**.
2. Pick signals on the left and **add** them, up to 12 at once.
3. Watch them live, hit **record** before something happens, or **save last** right after it did.

If they're running, close TuneMaster's signal logging and RobotStudio's signal tools first. The controller will stream signals to one
consumer at a time only, and Signal Spy tells you when someone else is on it instead of fighting over it.

Spacebar pauses, add cursors to measure anything on the chart, zoom in and out, change scale, make the chosen chart fill the whole chart area, etc:

![Paused with cursors](screenshots/cursors.png)

Or switch to the **live dashboard** for big numbers you can read from across the cell:

![Live dashboard](screenshots/dashboard.png)

Light theme also available if you're insane: uncheck **dark** under **view**:

![Light theme](screenshots/light.png)

The pictures are of the built-in fake controller the tests use, not a real cell.

Recordings go to `Documents\TestSignals` (or wherever **file > recordings folder...** points), one folder each: a plain `data.csv`
(`controller_ms,channel,value`) plus a `recording.json` with the details. A **slow log** writes `slow.csv` instead
(`controller_ms,channel,count,mean,min,max`, one row per interval). **file > open a recording** brings one back up.

Settings, your notes on signals and the log live in `%LOCALAPPDATA%\ABB Signal Spy`. For a portable copy, put the exe in a folder of its own and copy the `settings.json` from there next to it.

The built-in signal list was measured on an IRB 2600 with RobotWare 6.16. Other robots or versions may
number things differently, but the identified signals should be correct - the unidentified signals are most likely model specific things, or OmniCore/RW7 specific.

## Build it

Rust 1.95 or newer with MSVC: `cargo build --release -p abb-signal-spy`.

## License

Copyright (C) 2026 Jon Sands. GPL-3.0-or-later, see [LICENSE](LICENSE).
Not affiliated with ABB; ABB and IRC5 are ABB's trademarks.
