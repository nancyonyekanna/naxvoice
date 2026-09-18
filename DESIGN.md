# Dashboard design

Desktop window, roughly 900x700, left sidebar plus content pane. Six screens.
Plus one floating overlay that is not part of this window.

The HTML wireframe at `design/wireframes.html` is the visual reference for everything below. Open it in a browser alongside this file.

Visual direction: quiet and dense. Hairline borders, no shadows, no gradients,
no accent colour except for status. This is a settings surface for someone who
opens it rarely and wants to find a control fast, not a dashboard to admire.
System font. Monospace only for transcript text, model identifiers and
dictionary terms, where character-level precision is the point.

Every control below maps to a key in `config.example.yaml`.

---

## Sidebar

Status · Models · Profiles · Dictionary · Voice · History · Hotkeys

Single-level, no nesting, no collapse. Seven items fit without scrolling.

---

## 01 Status

Landing screen. Answers "is it working and what is it costing me."

**Header** — app name, machine name, and one status pill: ready / degraded / offline.

**Three metric tiles**
- Round trip, 7-day median, in ms. The number that matters.
- Words today, with dictation count underneath.
- Spend this month, pulled from OpenRouter.

**Engine list** — four rows, each a status dot, a name, and a right-aligned state.
- Transcription · model name — cloud, last latency
- Cleanup · model name — cloud, last latency
- Kokoro — local, loaded / loading
(No second engine. Chatterbox-Turbo was measured at ~2.1s of compute per spoken
second and dropped; see CLAUDE.md step 7.)

Dot colours: green ready, amber warming, red failed. Never rely on colour alone;
the state text carries the same information.

---

## 02 Models and routing

**OpenRouter key** — password field, with remaining credit shown beside it once
validated. Stored in the OS keychain, never written to config.

**Transcription model** — select. Label each option with its trade-off, not just
its name: "deepgram/nova-3 · fastest", "openai/whisper-large-v3 · most accurate".

**Cleanup model** — select.

**If offline** — select: paste raw transcript / queue until reconnect.

**Chunking** section, below a divider. These three controls decide whether the
tool feels fast or feels broken, so they are exposed rather than buried.
- Pause threshold, slider, 200-1200ms, default 450
- Chunk overlap, slider, 0-500ms, default 200
- Audio, select: Opus 24kbps (recommended) / WAV 16kHz

Helper line under the sliders: lower threshold sends sooner but risks cutting
mid-thought.

---

## 03 Prompt profiles

Two-pane. List on the left, editor on the right.

**List** — profile name with the matched app identifier underneath in smaller
muted text. Default sits at the bottom, visually separated.

**Editor**
- Cleanup instruction, textarea, ~4 rows
- Model, select, defaulting to "inherit"
- Two checkboxes: strip fillers, auto-capitalize
- Preview block: a sample raw transcript above, the cleaned result below, both
  monospace. This is the only way to tell whether a prompt edit did what you
  wanted without leaving the screen.

---

## 04 Dictionary

**Add row** — two inputs side by side: term, and "sounds like" (optional), then
an Add button.

**Table** — term in monospace on the left; recorded mishearings and a correction
count on the right, muted.

The correction count is not decoration. It decides which terms make the top-40
prompt budget. Sort by it descending.

Footer line: top 40 terms bias the transcription prompt, the rest are corrected
by the cleanup model.

---

## 05 Voice and read-aloud

- First sentence — engine and voice select, with a play button to preview
- Main voice — engine and voice select, with a play button
- Clone source — a file row showing the current sample with a Re-record button
- Speed — slider 0.5-2.0, default 1.2
- Expression — not shown. It was a Chatterbox-only control, and Chatterbox is
  not used; Kokoro ignores the setting.

**Pronunciation rules**, below a divider. Rules render as removable chips in
monospace, `from → to`. Three checkboxes: skip code blocks, skip URLs,
expand numbers.

There is no handoff to tune: read-aloud uses one voice throughout. The clone
source row is dead until an engine that can clone is fast enough to use — see
CLAUDE.md step 7 for the measurements that ruled Chatterbox out.

---

## 06 Overlay

Not part of the dashboard window. A borderless always-on-top widget near the
cursor, roughly 200x40. Three states:

- **Listening** — mic icon, elapsed time, chunk count sent
- **Polishing** — spinner, no time estimate (an estimate that is wrong is worse
  than none)
- **Reading** — speaker icon, time remaining, Stop button

Dismissable with Escape in every state. Never steals focus — if it takes focus
the paste target is lost and the whole interaction fails.

This is the surface the user actually lives in. Polish it before the dashboard.

---

## Copy rules

Sentence case throughout. Active voice on buttons: "Add", "Re-record", "Stop".

Errors say what happened and what to do, in one line, with no apology and no raw
exception text. "Couldn't reach OpenRouter. Pasting raw transcript." not
"Error: request failed".

Empty states are an invitation, not a report. The dictionary with no terms says
"Add a term Whisper keeps getting wrong", not "No terms yet".
