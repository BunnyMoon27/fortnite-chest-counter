# Chest Counter (Rust)

Counts the chests you open in Fortnite by reading the replay files the game saves in
`%LOCALAPPDATA%\FortniteGame\Saved\Demos`. It only reads those files: it never touches the game,
its memory or its install folder, and it sends nothing over the network.

This is a Rust port of [ChestCounter](https://github.com/xdoofy92/ChestCounter).

Unofficial fan tool. Not affiliated with or endorsed by Epic Games. Fortnite is a trademark of Epic Games, Inc.

## Build

```
cargo build --release
```

- `chest-counter.exe`: the tracker window (total, last match, today, session, average, best match,
  best day, last 7 days, recent matches). It watches the replay folder and, when a match ends,
  shows "You opened N chests". **Reset** starts from zero and keeps a backup copy of the history.
- Overlay: a small always-on-top counter over the game. Fortnite must be in *Windowed Fullscreen*
  (exclusive fullscreen cannot be drawn over). **Ctrl+Shift+O** shows/hides it, **Ctrl+Shift+L**
  locks it (clicks go through) or unlocks it (drag to move). Right-click hides it.
  Change both hotkeys under **Hotkeys** in the window (e.g. `Ctrl+Alt+K`, `F9`).
- `CHESTCOUNTER_DEMO_RESULT=1` (environment variable) shows the latest match as just finished,
  to preview the result banner and overlay.
- `cc-cli.exe <file.replay>`: prints one match as JSON. `cc-cli.exe --scan` analyzes every replay.

History is saved in `%LOCALAPPDATA%\ChestCounterRs\history.json`, so matches stay counted after
Fortnite deletes old replays.

## What counts

Same rules as the original:

- Chests opened by the replay owner, plus chests they broke instead of opening.
- Ammo boxes, food boxes and other containers are tracked separately and not in the total.
- Only standard Battle Royale playlists from Fortnite v43 (Chapter 7 Mini Season) onward count.

## Status

Phase 1 of the port: replay parser, chest counting, saved history, simple window.
Not ported yet: the browser report, the OBS page, treasures,
the teammates view and settings, profiles for several accounts, "Open with Fortnite", updates,
translations.

## Licenses

- Most of the logic is ported from ChestCounter (MIT, Copyright (c) 2026 LED-N) and
  FortniteReplayDecompressor (MIT). Their notices are in `LICENSE-ChestCounter-MIT` and
  `LICENSE-FortniteReplayDecompressor-MIT`.
- `src/replay/oodle.rs` is ported from OozSharp's `Kraken.cs`, a port of
  [ooz](https://github.com/powzix/ooz) by Powzix, licensed **GPL-3.0-or-later**. Because the app
  includes it, the app as a whole is GPL-3.0-or-later if you distribute it.
