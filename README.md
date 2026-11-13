# Fortnite Chest Counter

Counts the chests you open in Fortnite, match by match, and shows your totals.

- Counts chests you open (and chests you break open) in Battle Royale.
- Shows your total, last match, today, session, average and best match.
- A small **overlay** you can see while you play.
- Nothing to install. Just download and open.

> Unofficial fan tool. Not made by, affiliated with, or endorsed by Epic Games.
> Fortnite is a trademark of Epic Games, Inc.

## Download

1. Go to the [Releases page](https://github.com/BunnyMoon27/fortnite-chest-counter/releases).
2. Download **ChestCounter.exe**.
3. Double-click it.

**Windows says "Windows protected your PC"?** This happens because the app is not code-signed yet.
Click **More info**, then **Run anyway**. You can check the file with the SHA-256 value on the release page.

## Before you start: turn on replays in Fortnite

The counter reads Fortnite's replay files, so replays must be saved:

1. Open Fortnite and go to **Settings**.
2. Go to **Game**, then **Replays**.
3. Turn on **Record Replays**.

Without this, nothing gets counted.

## How to use it

- **Play a Battle Royale match.** When the match ends, the counter adds your chests within a few seconds.
- A banner says **"You opened N chests"** when a match finishes.
- Your totals and recent matches are in the window.
- **Reset** starts counting from zero. A backup of your old history is saved first.

## Overlay (see your count while playing)

- Press **Ctrl+Shift+O** to show or hide the overlay.
- Press **Ctrl+Shift+L** to lock it (clicks go through it) or unlock it (so you can drag it).
- Right-click the overlay to hide it.
- Fortnite must be in **Windowed Fullscreen**. Exclusive fullscreen hides the overlay.

You can change both hotkeys in the window under **Hotkeys**. Examples: `Ctrl+Alt+K`, `F9`.
The **Move / Lock** button does the same as the lock hotkey.

## What counts

- Chests you open, plus chests you break open.
- Chest types are listed separately under **By type**: Chest, Rare chest, Buried chest, Relic chest, Spirit poster chest, and Other chest.
- Ammo boxes, food boxes and other containers are not counted in the chest total.
- Only standard Battle Royale matches from Fortnite Chapter 7 Mini Season (v43) onward count.

## Your data

- Everything stays on your PC. The app does not send anything over the internet.
- The app only reads replay files. It never touches the game.
- Your history is saved in `%LOCALAPPDATA%\ChestCounterRs\history.json`.
- Matches stay counted even after Fortnite deletes old replay files.

## Known limits

- Vault counting is not supported yet.
- Only Windows is supported.

---

## For developers

Requires the [Rust toolchain](https://rustup.rs).

```
cargo build --release
```

Files in `target/release/`:

- `chest-counter.exe`: the tracker window.
- `cc-cli.exe <file.replay>`: prints one match as JSON. `cc-cli.exe --scan` analyzes every replay.

Set `CHESTCOUNTER_DEMO_RESULT=1` to show the latest match as just finished, to preview the banner and overlay.

## Credits and licenses

- Most of the logic is ported from [ChestCounter](https://github.com/xdoofy92/ChestCounter)
  (MIT, Copyright (c) 2026 LED-N) and FortniteReplayDecompressor (MIT).
  Their notices are in `LICENSE-ChestCounter-MIT` and `LICENSE-FortniteReplayDecompressor-MIT`.
- `src/replay/oodle.rs` is ported from OozSharp's `Kraken.cs`, a port of
  [ooz](https://github.com/powzix/ooz) by Powzix, licensed **GPL-3.0-or-later**.
  Because the app includes it, the app as a whole is GPL-3.0-or-later. See `LICENSE`.
