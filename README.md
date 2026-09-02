<div align="center">

**English** · [Русский](README.ru.md)

# 🎣 terraria piscatio automatica

### Auto-fishing for Terraria 1.4.5 — with a catch filter, auto-drinking and quick-stacking into chests

**Fishes on its own. Keeps only what you want. Runs while the game is minimised.**

[![Download](https://img.shields.io/github/v/release/genius8loci/terraria-piscatio-automatica?label=download&style=for-the-badge&color=4a56a8)](https://github.com/genius8loci/terraria-piscatio-automatica/releases/latest)
[![Telegram](https://img.shields.io/badge/Telegram-channel-2AABEE?style=for-the-badge&logo=telegram&logoColor=white)](https://t.me/terraria_piscatio_automatica)
[![Bugs and ideas](https://img.shields.io/badge/bugs%20and%20ideas-issues-24292f?style=for-the-badge&logo=github)](https://github.com/genius8loci/terraria-piscatio-automatica/issues)

<img src="docs/img/panel.png" width="620" alt="Terraria auto-fishing panel — rod autoclicker, catch filter, auto-drinking">

</div>

---

## What this is

An injectable DLL for **Terraria 1.4.5.8** that fishes for you.
It casts to a remembered spot, waits for the bite, hooks only what you
allowed, and never wastes bait on junk.

The panel is drawn with the **game's own textures** — it looks like it has
always been part of Terraria: the same windows, the same slots, the same
font, the same tooltips when you hover an item.

> **Keywords:** Terraria fishing bot, auto fishing, AFK fishing, fishing rod
> autoclicker, farm crates, Angler quest helper, Terraria 1.4.5 mod,
> fishing automation DLL.

---

## What it does

### 🎣 Fishes on its own

It remembers the spot of your first cast — the manual one — and keeps casting
there. Hooks inside the bite window, waits, casts again. Delays are jittered
so the actions don't tick like a metronome.

### 🐟 Catch filter: keeps only what you want

**Skipping is free.** The game decides what bit **before** the hook, and bait
is only consumed on a successful pull. So junk can be skipped for nothing,
as many times as you like.

- **Blacklist** — keep everything except what is marked red.
- **Whitelist** — keep only what is marked green.
- Every catchable item the game itself lists, search by name, one click to mark.

<img src="docs/img/filter.png" width="620" alt="Terraria catch filter — fish blacklist and whitelist, search by name">

### 🧪 Auto-drinking

Tops up the Fishing, Sonar and Crate buffs as soon as they run out, with two
more slots for Ale and Sake — their Tipsy buff is worth +5 fishing power, the
same as fishing while sitting. It only drinks while fishing is actually running
— switched on and the cast point locked — so no buff burns away while you look
for a spot. An item you don't have in your inventory is crossed out and never
clicked — with a tooltip saying what to bring.

### 📦 Quick-stack into chests

Inventory filled up — it presses "quick stack to nearby chests" with the
game's own hands. Nowhere left to stack — it stops instead of drowning
your catch.

### 💬 Tells you in chat what it is doing

A skip by filter, a quest fish, a spawn, a potion or a drink used — one line in chat,
with the item icon and a tooltip on hover. Colours are configurable.
The messages are visible only to you and never leave for the server.

**Quest fish is marked with a sound** — it is easy to miss.

### 📊 Statistics

Time fishing, items caught, crates, skipped, average time to bite,
potions and food drunk.

<img src="docs/img/stats.png" width="620" alt="Terraria fishing statistics — catch, crates, average time to bite">

### 😴 Works while the game is minimised

It removes the throttling that puts Terraria to sleep without focus. Minimise
it and go do your things — fishing keeps running at full speed.

### 🌍 English and Russian

The panel takes its language from the game. Switch the language in Terraria's
settings and the panel follows within a couple of seconds — no restart needed.

### 🐉 Spawning mobs while fishing

Sometimes it is not a fish on the hook but a guest — Duke Fishron and other
things from the deep. A separate toggle, off by default, so nothing crawls
out while you are away making tea.

---

## Installation

### 1. Download the DLL

[![Download piscatio.dll](https://img.shields.io/badge/%E2%AC%87%20download-piscatio.dll-4a56a8?style=flat-square)](https://github.com/genius8loci/terraria-piscatio-automatica/releases/latest)

### 2. Get an injector

Tested with **[Extreme Injector v3](https://github.com/master131/ExtremeInjector)**
— free, simple, needs nothing installed. Any other injector that can load
32-bit DLLs will do.

### 3. Start the game and inject

1. Launch Terraria and enter a world.
2. In the injector pick the `Terraria.exe` process, point it at `piscatio.dll`, press Inject.
3. The panel appears at the top of the screen.

> Terraria is a 32-bit process, so the DLL is built for `i686-pc-windows-msvc`.
> A 64-bit build would not fit and is not needed.

### 4. First run

1. Hold a **fishing rod** and put **bait** in your inventory.
2. Turn on **Auto-fishing** in the panel.
3. **Cast the rod yourself, once** — that spot gets remembered.
4. From there you can leave it alone.

---

## Controls

| Key | What it does |
|---|---|
| <kbd>↑</kbd> | collapse and expand the panel |
| <kbd>↓</kbd> | turn auto-fishing on and off |
| <kbd>Delete</kbd> | unload the DLL from the game |

Keys are only listened to while the game window is in front, and stay silent
while you are typing in the search box. They are remappable in `piscatio.toml`.

---

## Files next to the DLL

| File | What's in it |
|---|---|
| `piscatio.toml` | settings — every line with a comment |
| `piscatio.log` | what happened: version, attach, catch, errors |

The config creates itself on first run. The panel rewrites it on every toggle,
so edit it by hand with the game closed.

---

## If something went wrong

**The panel did not appear.** Look into `piscatio.log` next to the DLL — the
first line is the build version, then every attach step.

**It does not cast.** Check the "Auto-fishing" row in the panel: until the
spot is remembered it says "waiting for your first cast" in green. Cast the
rod yourself once.

**It stopped by itself.** The reason is written right in that row: out of
bait, inventory full, nowhere to stack, or the bobber missing the water
three casts in a row. The stop also goes to chat with a sound — it is easy
to miss while you are in another window.

**You switched the hotbar item** — auto-fishing turns itself off. That is on
purpose: otherwise it would keep swinging whatever ended up in your hand.

**The game crashed.** The log will hold a `ПАДЕНИЕ:` line with the code, the
address and what the DLL was busy with. Send it over — it shows whose code
is to blame.

<div align="center">

### Found a bug? Got an idea?

[![Issues](https://img.shields.io/badge/GitHub-open%20an%20issue-24292f?style=for-the-badge&logo=github)](https://github.com/genius8loci/terraria-piscatio-automatica/issues)
[![Telegram](https://img.shields.io/badge/Telegram-write-2AABEE?style=for-the-badge&logo=telegram&logoColor=white)](https://t.me/terraria_piscatio_automatica)

Logs and screenshots help a lot.

</div>

---

## For those curious how it works

The whole technical base — how the DLL is put together, the fishing mechanics
read off the decompiled game, the detours, the overlay, the reflection
pitfalls — lives separately:

**📖 [genius8loci.github.io/terraria-piscatio-automatica](https://genius8loci.github.io/terraria-piscatio-automatica/)**

The same text in the repository — [docs/index.md](docs/index.md).
It is written in Russian.

The project is written in Rust, without offsets: it reaches the game's fields
and methods by reflection over names, which is why it survives small patches.

```
cargo build --release
```

---

## Disclaimer

This project was made for single-player and for myself. Automation in
multiplayer is a matter of server rules, not mine; on someone else's server
use your head.

Terraria is a trademark of Re-Logic. This project is not affiliated with
Re-Logic and is not endorsed by it.

<div align="center">

**MIT** · made by [@Genius_Loci](https://t.me/Genius_Loci)

</div>
