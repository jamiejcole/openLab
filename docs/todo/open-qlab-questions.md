<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Open QLab questions

| # | Question | Assumed (implemented) | Where | Status |
|---|---|---|---|---|
| 1 | Does QLab 5 refuse a malformed OSC address or argument list typed into a Network cue, or accept it and fail at send? Searched qlab.app/docs/v5/networking/network-cues/ and the v3/v4 OSC cue pages: they give the text format (space-separated arguments, integer/float/string/boolean forms) but say nothing about invalid input. | Refuse at the edit: address must start with `/`, printable ASCII, no spaces; at most 32 arguments, finite floats, bounded sizes; destination must be an existing network patch. | `CueProperty::{Destination, Address, Args}`, docs/commands.md | Open |
