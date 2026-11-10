<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# Changelog

Each release lists what an operator would notice. Versions follow `MAJOR.MINOR.PATCH`; until 1.0.0 the show-file
format may change between releases, and openLab is not yet meant for production shows.

## 0.0.1 (not yet released)

The first release: a macOS app (12 Monterey or later, Apple silicon and Intel) with its own playback engine.

- Cue lists with Audio, Fade, Group, Start, Stop, Pause, Load, Reset, Devamp, GoTo, Arm, Disarm, Wait, Memo and
  Network cues, following QLab 5's behaviour.
- Audio cues: trims, loops, slices, built-in fades, per-cue levels and routing through audio patches.
- GO, panic, pause, resume, load and preview; pre-waits, post-waits, auto-continue and auto-follow.
- Show Mode for performance, with errors shown without dialogs.
- Autosave and recovery after a crash, rotating backups of the show file, relinking of moved media.
- OSC control using QLab 5's address space where the feature exists, with passcodes.
- Pre-Show Check, a show log, and warnings for cues that cannot run.

Distributed as a `.dmg`, ad-hoc signed: macOS asks for confirmation the first time the app is opened.
