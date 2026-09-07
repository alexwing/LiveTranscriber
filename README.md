# LiveTranscriber

You are in a Teams call. The others speak English. You speak Spanish into your
microphone, and they hear it in English, in your own voice, through a virtual
microphone. At the same time, what they say in English you read in Spanish on screen.

That is the case this application was built for.

More generally: live transcription of **whatever is playing on your PC** — a Teams
call, a film, the browser — and of **your microphone** at the same time, as two
separate sessions. Both can be translated, each in its own direction. And what you say
into the microphone can come back out spoken in the other language, in a clone of your
own voice, through whichever output device you point it at.

Everything runs locally on your GPU. No API key, and no audio leaves the machine.

Built on [`nvidia/nemotron-3.5-asr-streaming-0.6b`](https://huggingface.co/nvidia/nemotron-3.5-asr-streaming-0.6b)
(FastConformer + RNNT, cache-aware streaming) for recognition,
[NLLB-200](https://huggingface.co/facebook/nllb-200-distilled-600M) for translation, and
[Chatterbox](https://huggingface.co/ResembleAI/chatterbox) or
[Kokoro-82M](https://huggingface.co/hexgrad/Kokoro-82M) for the voice.

**Windows only**, NVIDIA GPU required. Every number in this document was measured on one
machine — RTX 3060 12 GB, Windows 11 — not copied from a model card. Where a number
comes from a different test bench, it says so next to the number.

> The interface is bilingual English/Spanish, with an ES/EN selector in the header and
> hot switching. It defaults to English and falls back to Spanish only when the system
> locale starts with `es`. Source comments and commit messages are in Spanish.
> Configuration keys and identifiers are English.

This README is a walkthrough. Install, wire it up, run one meeting, then the rest of
what it does.

---

## Step 0 — What you need first

| Requirement | Detail |
|---|---|
| **NVIDIA GPU, Turing or newer** | The `cu128` PyTorch wheel starts at `sm_75`. A GTX 10xx (`sm_61`) will not work — the installer checks and stops there rather than failing at runtime. |
| **NVIDIA driver** | PyTorch needs CUDA 12.8. |
| **Free disk: 15 GB**, or **27 GB with the voice** | Both are hard limits that abort the installer, not recommendations. |
| **Python 3.10–3.13** | 3.14 does not work even if installed: PyTorch publishes no wheels for it. The installer can put 3.12 in for you. |
| **Rust and Node** | Only to *build* the app. Not needed to provision the models. |
| **VB-CABLE** (voice only) | A 1 MB virtual audio cable from [vb-audio.com/Cable](https://vb-audio.com/Cable/). You install it by hand; the script does not. |
| **A WAV of your voice** (voice only) | 10–30 seconds of clean speech, for the cloned voice. Not needed with the preset voices. |

Three things to turn on, in that order, and each one is optional. You can stop after the
first.

| What you turn on | What it costs | What it gives |
|---|---|---|
| **Transcription** | one Python environment, 15 GB of disk, ~2.4 GB of VRAM per source | live text of the system audio and your microphone |
| **Translation** (installed by default) | 4.60 GB more model, 1.27 GB of VRAM, one sentence of lag | both sides translated, in both directions |
| **The voice** | a second Python environment, 27 GB of disk in total, VB-CABLE, a voice sample | the meeting hears you in their language, in your voice |

Rough shape of the disk: two Python environments, each carrying its own PyTorch, plus
the model cache. Measured sizes of the cache on this machine:

```
%USERPROFILE%\.cache\huggingface\hub\
    models--nvidia--nemotron-3.5-asr-streaming-0.6b\    2.38 GB
    models--facebook--nllb-200-distilled-600M\          4.60 GB
    models--ResembleAI--chatterbox\                     2.99 GB   (voice only)
    models--hexgrad--Kokoro-82M\                        0.34 GB   (voice only)
```

That cache is shared with every other project on the machine. If you already have these
models, the installer downloads nothing.

---

## Step 1 — Install

**From the release**, which is the short way: download the `.exe` from
[Releases](https://github.com/alexwing/LiveTranscriber/releases), install it, and
provision from the app's own folder. The installer carries the scripts inside it, so
there is nothing to clone:

```powershell
& "$env:LOCALAPPDATA\LiveTranscriber\install.cmd" -InstallPython -WithVoice
```

**From source**, if you want to build it yourself:

```powershell
git clone https://github.com/alexwing/LiveTranscriber
cd LiveTranscriber
.\install.cmd -InstallPython -WithVoice
```

Drop `-WithVoice` if you only want transcription and translation.

Use `install.cmd` and not the `.ps1` directly. A freshly installed Windows has its
execution policy on `Restricted`, and a file downloaded from the internet carries the
mark-of-the-web on top of that. Either one stops you at your very first command; a
`.cmd` is subject to neither.

The voice is opt-in because it costs roughly 11 GB more: it needs a **second Python
environment** in `.venv-tts`, with its own torch. The ASR sidecar requires
`transformers>=5.13` for `AutoModelForRNNT`, and `chatterbox-tts` is tested against
4.57.x. They do not fit in one environment.

The installer sizes up the machine, reads the GPU's compute capability and picks the
precision, provisions the environments, downloads the models by actually loading them,
writes `transcriber-config.toml` with absolute paths, builds the app, and then verifies
its own work. It can be run again without breaking anything.

Useful switches:

| Switch | What for |
|---|---|
| `-ModelsDir <path>` | Put the models somewhere other than `%USERPROFILE%\.cache`. Writes `hf_home` into the config, which reaches the sidecars as `HF_HOME`. |
| `-SkipTranslator` | Skip NLLB. Saves 4.6 GB if you are not going to translate. |
| `-WithVoice` | The second environment and the voice models. |
| `-SkipBuild` | Python and models only. |
| `-Force` | Rebuild the virtual environments. Leaves your settings alone. |
| `-ResetConfig` | Replace the configuration with a fresh one, keeping a dated backup. |

Full detail — every step in order, where things land, how the configuration is written —
is in [INSTALL.md](INSTALL.md).

Day to day:

```powershell
npm run app:dev
```

### How to check step 1 worked

```powershell
.\scripts\verify.ps1
```

It walks the pieces in dependency order and stops at the first failure. The last step is
the one that matters: it launches the ASR sidecar, loads the model onto the GPU, waits
for `ready`, sends three seconds of audio, asks for a `reset` and checks that the segment
closes. What it sends is a tone, not speech, so getting no text back is the expected
result. With the voice enabled it also starts the voice environment, checks that both
engines import, that a voice WAV has been chosen, and that the voice models are in the
cache.

---

## Step 2 — Wire it up

The window opens on the **Settings** tab. Four things to set, in this order.

### 2.1 — VB-CABLE, and the four settings that make it work

The synthetic voice needs somewhere to speak that is not your speakers. On Windows that
is a virtual audio cable, and the app does not install it — you do, by hand, from
[vb-audio.com/Cable](https://vb-audio.com/Cable/). It is 1 MB.

**When it installs, VB-CABLE makes itself the Windows default output.** Put your speakers
back as the default before going any further.

The cable runs one way: **CABLE Input → CABLE Output**. You write into the input, and
anything recording from the output hears it. Four settings follow from that, and three of
them are in this app:

1. **Speak through**, in the voice section: `CABLE Input`. That is where the voice writes.
2. **Sources → My microphone**: your **real** microphone. Put the cable here and the app
   only hears itself.
3. **Sources → System audio**: your speakers or headphones. That is where the meeting
   comes out, and it is what needs transcribing.
4. **Inside Teams** (or Meet, or Zoom), as the microphone: `CABLE Output`. That is the
   step that makes them hear you.

The rest of this section is the detail behind each of those four.

If the app sees no `CABLE Input` among the outputs, the voice section shows a notice with
a link and an **"Already installed it, look again"** button that re-enumerates devices
without restarting the app.

### 2.2 — Sources

Under **Sources**, pick the two capture devices:

- **System audio** — your speakers or headphones. Capture is loopback: it records what
  the device is playing.
- **My microphone** — your real microphone, not the cable.

Both can run at once. That is two independent sessions, each with its own Python process
and its own copy of the ASR model in VRAM.

If a chosen device is not there when you start (a USB microphone moved to another port
gets a new id; an unplugged one disappears), the app does not stop: it captures from the
Windows default instead and says so in the window, naming the device it is actually
using. The log names the device opened for every capture, so "why does it hear me worse
today" has an answer.

### 2.3 — Languages: there are four slots, not one

Translation here is bidirectional. There are four independent language slots:

| Slot | Meaning |
|---|---|
| Room source | What the others speak |
| Room target | What you read |
| Mic source | What you speak |
| Mic target | What they hear |

The interface presents them as two blocks with an arrow:

```
Room (the system audio)      en-US  →  es-ES     what the others say, and what you read it in
Microphone (what you say)    es-ES  →  en-US     what you say, and what they hear you in
```

Left alone, the microphone mirrors the room: you speak the language you read in, and you
are translated into the room's. Set them by hand and all four can differ. The two
recognition sessions genuinely transcribe in **different languages** — the microphone
session uses its own ASR language, not the room's.

One hard constraint: **with translation on, the room language cannot be "Detect
automatically"**. NLLB has to know the source language. The app validates that before
starting anything and tells you, instead of translating from the wrong language.

### 2.4 — The voice

The **Speak for me** section only appears when translation is on, and it requires
**Translate in parallel** and **My microphone** to be enabled. What gets spoken is the
translation of your microphone, and nothing else. Your own side is the only side it ever
speaks: the other participants' sentences are shown on screen and never synthesized,
because speaking them would mean talking over the meeting.

Pick an engine:

| Engine | Voice | Languages | VRAM | Speed |
|---|---|---|---|---|
| **Chatterbox** | cloned from your sample | 23 | 3.40 GB | 0.84–0.97x on short sentences, 1.02–1.03x on long text |
| **Kokoro** | preset, neutral | 7 | 0.56 GB | 46.6–47.9x |

> Both engine measurements come from a **different test bench** than the rest of this
> document: the same RTX 3060, but driven over the HTTP backend of a separate project,
> not through this app's own sidecar. Chatterbox covers ar, da, de, el, en, es, fi, fr,
> he, hi, it, ja, ko, ms, nl, no, pl, pt, ru, sv, sw, tr, zh. Kokoro covers en, es, fr,
> hi, it, pt, zh — no German, no Russian, no Korean, which is usually what decides the
> choice. Kokoro's Japanese is deliberately not offered: its G2P needs `pyopenjtalk`,
> which has no Windows wheel and wants the Visual Studio C++ compiler, so shipping it
> would break the install for everyone in exchange for one language.

The language the voice speaks is whatever your microphone is translated into. It is
validated before anything is launched: if the target is not among Chatterbox's 23, or
among Kokoro's 7 with that engine selected, startup fails with a message naming what to
change.

**With Chatterbox**, give it a sample of your voice: a WAV with 10–30 seconds of clean
speech. You type the **path** into a text box — there is no file picker for the sample
(there is one for the output folder). Without a WAV, Chatterbox does not start; the
sidecar also checks the WAV is readable and not empty before loading the model. The clone
imitates the sample's delivery: a monotone sample gives a monotone voice.

**With Kokoro**, there is no WAV. You type the name of a preset voice — `af_heart`,
`am_adam`, `ef_dora`, `em_alex` — whose prefix encodes language and gender. The sidecar
warns if the voice does not match the language you asked for.

Then **Speak through**: `CABLE Input`. That is where the voice writes.

Two wirings the interface actively warns about:

- Voice through the **speakers** with the microphone open. Your own synthetic voice gets
  picked up and translated again. Use headphones, or use the cable.
- Capturing from the **same cable** the voice speaks into. The app hears only itself and
  transcribes nothing. The check requires both ends to belong to the same cable, so it
  does not fire on an unrelated virtual device.

Last, **sentence grouping**. Chatterbox has about one second of fixed cost per request,
measured, so single short sentences fall below real time and the lag grows without bound.
Sentences are batched up to a size (150/250/350/500 characters, default 250) or until the
oldest one has waited (1/2/3/5 seconds, default 2). Both are selectable; grouping itself
is not something you switch off. It only applies **while the voice is already speaking**:
with the audio queue empty the first sentence is synthesized immediately, because waiting
there would be pure latency.

### 2.5 — Check it before the meeting starts

The CLI is the test bench. It needs neither the window nor a call.

The commands below use `cargo run`, which is the form for a cloned repository. If you
installed from the release, `asr-cli.exe` ships inside it — substitute
`"$env:LOCALAPPDATA\LiveTranscriber\asr-cli.exe"` for `cargo run -p asr-cli --` and
everything else is the same. No Rust needed.

```powershell
cargo run -p asr-cli -- devices
```

Lists inputs and outputs with the ids the configuration uses. Take the ids for
`CABLE Input` and `CABLE Output` from here.

```powershell
cargo run -p asr-cli -- level --from system --seconds 10
```

Tells you whether audio is arriving, at what level, and how much gain is being applied.
This is the first thing to check whenever something is wrong.

Then test the virtual microphone circuit without arranging a meeting. In one terminal,
listen on the far end of the cable:

```powershell
cargo run -p asr-cli -- run --from mic --device-id "<CABLE Output>" --seconds 30 --language en-US
```

In another, speak into the near end:

```powershell
cargo run -p asr-cli -- speak --text "Testing the virtual microphone" --lang en --engine kokoro --device-id "<CABLE Input>"
```

If the first terminal transcribes what the second one said, the loop works, and so will
the meeting. It was verified that way here with the cloned voice: 8.36 seconds of audio,
transcribed back word for word with punctuation.

---

## Step 3 — Your first meeting

Set up the meeting side first: **inside Teams (or Meet, or Zoom), choose `CABLE Output`
as the microphone.** Your speakers stay as the meeting's output, which is what the app
transcribes.

Press **Start**, or the global shortcut `Ctrl+Shift+T`.

Loading takes a while. The recognizers, the translator and the voice load **in parallel**,
each in its own thread, and the window reports each stage as it becomes ready. Chatterbox
alone is about 21 seconds cold, Kokoro about 6, both on the separate bench described in
2.4. The voice also waits up to 10 seconds for the output device to genuinely open, and
fails loudly if it does not — including a reminder that reinstalling VB-CABLE changes
device ids.

Then, for the rest of the call:

**They speak.** The system audio is transcribed in the room's language, and each sentence
is translated into yours as soon as its punctuation closes it. You read it.

**You speak, in your own language, into your real microphone.** That session transcribes
in your language, translates into the room's, and hands the result to the voice. The voice
speaks it through `CABLE Input`, and the meeting hears it through `CABLE Output`, in your
cloned voice, in their language. It never touches your speakers.

Switch to the **Transcript** tab and pick the **Meeting** view (`⊞`) — four boxes:

```
The others (room language)        their translation (your language)
My voice tells them (room lang.)  Me (my language)
```

The directions are deliberately flipped between rows, because that is the way the
translation flows. Each box scrolls on its own.

While the voice is running, the interface shows the accumulated lag live — seconds queued,
sentences waiting — and above 10 seconds it says plainly that it is generating slower than
you speak.

If your synthetic voice comes back through the capture, the app recognizes it. It keeps a
short memory of what the voice just said and compares incoming sentences by **word
overlap** — 70% minimum, never on fewer than 3 words, because anyone can say "yes".
Overlap rather than exact match, because recognition is not verbatim ("I'm" against "I
am"). A matched sentence is drawn dimmed with a **"your voice"** tag, is not translated,
is not spoken again, and is excluded from the translated boxes in the meeting view. The
entry expires after the pending queue plus the audio duration plus 10 seconds of grace.
The checkbox that turns the whole thing off is `mark_echo`. Turned off, your own
synthetic voice is transcribed and translated back into your language, and on the
microphone side it is spoken again.

If a sidecar dies mid-session, the app treats it as a terminal state and tells the window,
rather than swallowing sentences in silence for the rest of the call.

**Stop** closes the sources first and lets the voice finish what it already has. Between
you saying a sentence and the voice reaching it there is more than a second of
recognition, sentence closing and translation, so cutting the queue at Stop threw the
last sentence away every time. If the queue is long, closing the window sends the app to
the tray and you can wait it out there.

A meeting is working when the top row of the meeting view fills as they talk, the bottom
row fills as you talk, and the voice lag indicator stays low. If the bottom row fills but
nobody reacts, the problem is on the cable, not in the app — see the table at the end.

---

## Step 4 — The rest of what it does

The synthetic voice is optional and off by default. Turned off it costs no process and no
VRAM, and everything below still stands on its own.

### Transcription without any of the above

Turn translation off and it is a plain live transcriber of system audio: a film, a
lecture, a video with no subtitles. One session, one Python process, no voice.

Text appears as the model emits it and is grouped into paragraphs. A paragraph closes when
the model has gone `paragraph_idle_secs` (1.2 s) without producing new text, capped at
`paragraph_max_secs` (30 s). The cut is decided by the recognizer, not by the audio level,
so background music does not hold a paragraph open forever.

**Per-process loopback** captures a single application's audio — Teams without the music
playing next to it. It is implemented in the crate and exposed in the CLI as `--pid`, but
not yet in the interface.

### On-screen subtitles

A separate window — transparent, undecorated, always on top, kept out of the taskbar,
hidden at startup. It shows the last 2 closed lines, the live partial from both sources
merged, and below it in a different color the translations, which arrive one sentence
behind. Drag it to move it; it has its own close button.

When translation is on, a small switch in its top-right corner picks what to show: only
what is being said, only the translation, or both — labelled with the language codes from
your configuration (`EN`, `ES`, `EN+ES`). The choice is remembered between sessions.

Open it from the **Subtitles** button on the Transcript tab, from the tray menu, or with
`Ctrl+Shift+O`. Set `overlay_enabled = true` in the TOML and it appears on startup.

### Profiles

Named configuration profiles, saved in `transcriber-profiles.toml` next to the config
file. A profile stores everything on the Settings tab: languages, sources and their
devices, the voice, paragraph timing, output folder. Reusing a name updates instead of
duplicating. Saving is atomic, and you cannot switch profiles with a session running.

What a profile does **not** store: the installation paths — the ASR Python, the voice
Python, the sidecar scripts, `hf_home`. Those describe the machine, not how you want to
use it. A profile saved before reinstalling the environment would otherwise point the app
at a venv that no longer exists.

If a saved device is gone — headphones unplugged, VB-CABLE reinstalled — it falls back to
the default and **says which one**, as a visible error rather than a silent substitution.
All three are checked: system, microphone, voice.

### Six view modes

Selectable by glyph on the Transcript tab: combined (`≡`), split vertical (`⬌`), split
horizontal (`⬍`), original only (`O`), translation only (`T`), meeting (`⊞`). The
available modes are filtered by the configuration — without translation only "original
only" survives, and the meeting view needs both sources active. The effective mode is
computed without overwriting your saved preference, so applying a profile without
translation does not lose it. The split modes have a draggable divider, persisted between
20% and 80%.

### Export, in four formats

`.txt` of the original, `.srt`, `.srt` of the translation, and a bilingual `.txt` with
original and translation interleaved.

Filenames are `YYYY_MM_DD_<name>[_suffix].<ext>` — local date, configurable base name,
per-format suffix (`_translated`, `_bilingual`), and Windows-forbidden characters
replaced. If a file with that name exists, `_2`, `_3`… is appended up to 9999. It never
overwrites.

The folder is chosen with a native picker, can be opened in Explorer with a button, and
the interface shows the **effective** path plus a filename preview. A relative path in the
TOML is ignored in favor of `Documents\LiveTranscriber`.

There is also a copy button that follows the current view mode, and a `⧉` button on every
paragraph.

### Tray and shortcuts

Two global shortcuts, both configurable in the TOML: `Ctrl+Shift+T` starts and stops,
`Ctrl+Shift+O` toggles the subtitles. They are shown in the window footer. A shortcut
already taken by another app produces a warning, not a failure to start.

The tray icon has four entries: show/hide, start/stop, subtitles, quit. **Closing the main
window does not quit the app** — it goes to the tray. Quitting for real is the menu entry,
which stops the session first.

### The CLI

Four subcommands: `devices`, `level` (with `--pid` for per-process loopback), `run` (with
`--translate-to`, `--save-txt`, `--save-srt`) and `speak`. It is the way to diagnose
anything without the GUI, and it reads the same configuration file the app does.

`asr-cli.exe` is bundled with the installer, so it is there whether or not you have Rust:
next to the application in `%LOCALAPPDATA%\LiveTranscriber\`, or in `target\release\`
if you built from source.

---

## How it is built

A Cargo workspace with the logic in crates that know nothing about Tauri, and `src-tauri`
as a thin layer that only translates to the UI.

```
crates/asr-audio    WASAPI capture and playback, gain normalization, silence gate
crates/asr-core     engine, sessions, translation, synthetic voice, profiles, transcript, config
crates/asr-cli      headless test bench
src-tauri           commands, tray, shortcuts, events, windows
sidecar/            three Python processes: asr_server.py, mt_server.py, tts_server.py
src/                React 18 + Vite + TS, typed wrapper over invoke
```

The flow in: WASAPI → normalizer → gate → length-prefixed frames over stdin to the ASR
sidecar → text back on stdout → translation, sentence by sentence → `emit` to the window →
React paints it. The flow out: translated microphone text → grouper → TTS sidecar → PCM
back → WASAPI output device.

`AsrEngine`, `Translator` and `Synthesizer` are traits, each with a factory. Today every
implementation shells out to Python. Replacing one — an ONNX engine in pure Rust, a
permissively licensed translator — means implementing the trait again and changing the
line that constructs it. Neither the capture layer nor the UI notices.

With everything on there are up to four Python sidecars alive: two recognizers, possibly
transcribing different languages, one translator, one synthesizer. Each is closed by
protocol — a `shutdown` frame, then stdin closed, then five seconds of grace, then
`kill()` on the child. That kills the child, not the tree: an orphaned
`multiprocessing.spawn` grandchild can still outlive it holding VRAM (measured once at
11.6 GB). The fix is a Windows Job Object with `KILL_ON_JOB_CLOSE`, and it is **not
implemented yet** — it is written up as pending work in `PLAN_TTS.md`.

---

## Measured performance

RTX 3060 12 GB, Windows 11.

Every stage of the pipeline stamps the log with a sentence id and a stage name, so a
session can be turned into a per-sentence latency table after the fact:

```powershell
python scripts\latency_report.py
```

It reads the newest log and prints, for each sentence, how long it spent between the
recognizer closing it, the translator returning, the voice starting to synthesize, and
the first byte reaching the output device — with medians and p90 across the session.
The numbers below for the voice come from that tool, not from a separate bench.

### Recognition

| lookahead | latency | RTFx |
|---|---|---|
| 0 | 80 ms | 1.8x |
| 3 (default) | 320 ms | 4.6x |
| 6 | 560 ms | 6.3x |
| 13 | 1120 ms | 9.4x |

Only `lookahead` values 0, 3, 6 and 13 are accepted. **The latency column is not a
stopwatch** — it is the value the model itself declares, which the sidecar re-emits in its
`ready` message. Only RTFx is measured here.

### Translation

~160 ms per **sentence** in isolation; 190–950 ms measured inside the running pipeline
with both recognizers on the card (see the voice section below). 1.27 GB of VRAM, with a
hard 30-second timeout per sentence.

Translation runs sentence by sentence, cut on punctuation, and is grouped by paragraph
only for display. Not for latency: NLLB is trained at sentence level and hands a paragraph
back short — given two sentences it returned one. The sentence splitter respects decimals
("3.5"), ellipses and multibyte text.

### Voice

Chatterbox: 4,435–4,606 ms for an 83-character sentence, 0.84–0.97x, rising to 1.02–1.03x
on 330 characters. Cold start about 21 seconds; it is preloaded when the app starts, never
on the first sentence. Kokoro: 111–115 ms, 46.6–47.9x, cold start about 6 seconds. Both
figures come from the separate bench described in step 2.4.

The only figure measured through this app's own sidecar and the full loop: Kokoro,
Spanish, 5.15 seconds of audio generated in 1,233 ms — 4.18x — and played back over WASAPI
in exactly 5.15 seconds. Kokoro hot in this sidecar is 103 ms per request.

Chatterbox through this app's sidecar, with both recognizers and the translator on the
same card at the same time, comes out slower than the isolated bench: 0.71x–1.05x real
time across six cloned-voice sentences in three runs. Short sentences pay a fixed cost and
sit near 0.7–0.8x; from about 170 characters they reach 1.0x. Synthesis was 85–94% of the
microphone path's latency: 5.9 s from the recognizer closing a 62-character sentence to
its first byte on the output device, 12.6 s for 211 characters.

The same runs put numbers on three waits that no bench shows. Cutting a paragraph on
silence costs `paragraph_idle_secs` before translation even starts — 1.84 s measured at the
1.8 s default. The translator is one thread for both sources, so a microphone sentence
queues behind the room's translation — 457 ms in one case. And the voice is serial end to
end: a short sentence that arrived while a long one was being synthesized waited 5.5 s for
the synthesizer, 2.0 s for grouping and 5.0 s for the previous audio to finish playing —
17.7 s in total for 38 characters. Translation itself was 190–950 ms per sentence: the
first call of a session costs about 750 ms, short sentences about 200 ms after that, and
200–270 characters 650–950 ms.

### VRAM

| Component | VRAM |
|---|---|
| ASR (per session) | 2.40 GB |
| NLLB | 1.27 GB |
| Chatterbox | 3.40 GB |
| Kokoro | 0.56 GB |

One capture source, translation and Chatterbox is about 7.1 GB of the card's 12. On this
machine the Windows desktop was already sitting at roughly 0.87 GB on top of that — a
baseline, not a cost of the application. The voice requires microphone capture and system
capture is on by default, so in practice there are two ASR sessions and two copies of the
2.40 GB, which puts the realistic total near 10.3 GB. That last figure is arithmetic, not
a measurement.

---

## Configuration

Everything the app keeps lives in one place, and it is not next to the `.exe`:

```
%APPDATA%BSLiveTranscriberBS
    transcriber-config.toml     settings
    transcriber-profiles.toml   saved profiles
    logsBS                       one file per day
```

That is where the installer writes, where `verify.ps1` reads, and where the app looks —
installed or not. It has to be one agreed location: the installer runs from a cloned
repository while the installed app runs from `%LOCALAPPDATA%` or `Program Files`, so
anything derived from "wherever I happen to be" produces two files that never meet.
`LIVETRANSCRIBER_CONFIG` overrides it for all three, and `npm run app:dev` points it at
the repository's own TOML so development does not touch your personal settings.

The log matters more than it sounds. The app is built windowed and has no console, so
that file is the only place its own account of a failure exists — including every error
the interface shows you, and the size and date of the configuration it actually read.
That last detail exists because a stale copy of the config once cost several rounds of
wrong diagnoses.

Only one window runs at a time; launching again brings the existing one to the front.
Two windows meant one of them could hold settings from before an edit, and both wrote to
the same log, which made a failure in the old one look like it came from the new.

Most settings are editable from the Settings tab. Worth knowing about directly:

- `hf_home` moves the models out of the default Hugging Face cache. It reaches the
  sidecars as the `HF_HOME` environment variable, which is the only way the Python library
  sees it. Set it with `install.cmd -ModelsDir <path>`.
- `python` and `speak.python`, the two interpreter paths. **There is no field for them
  in the interface**: if they are wrong, run the installer again or edit the file. The app
  refuses to overwrite a non-empty one with an empty value, which is how a bad startup
  used to destroy a good configuration.
- `overlay_enabled`, `hotkey_toggle`, `hotkey_overlay`.
- The whole `[speak]` section, off by default.
- Tuning defaults: `gate_drop_db` 25.0, `gate_floor_dbfs` -80.0, `gate_hold_secs` 2.0,
  `paragraph_idle_secs` 1.2, `paragraph_max_secs` 30.0, `normalize_gain` true, `lookahead`
  3, `dtype` bfloat16.

The active tab, the view mode and the split ratio live in `localStorage`, not in the TOML.

---

## Limits

Stated flat, because they matter.

**NLLB-200 is CC-BY-NC-4.0 — non-commercial.** This is the one blocker for anything
shipped as a product. The way out is Opus-MT or MADLAD-400, which means implementing the
`Translator` trait again and nothing else. Chatterbox is MIT, Kokoro is Apache 2.0, and
the app's own code is MIT — the voice engines add no commercial restriction. The ASR
model's license is not recorded anywhere in this repository; check its model card before
assuming anything. VB-CABLE is a separate product with its own license.

**Chatterbox runs at the edge of real time.** 0.84–0.97x on short sentences, 1.02–1.03x on
long ones, and every one of those measurements was taken with the voice alone. Nobody has
measured it with the recognizers and the translator on the same card, which is the most
relevant open risk here: the margin over 1.0x is 3%. Almost all of the gap is the ~1
second of fixed cost per request, and no cache removes it — hence the grouping, and hence
the lag warning. Kokoro, at ~47x, has no such problem, but it gives up voice cloning and
covers 7 languages instead of 23.

**All Chatterbox audio carries Resemble AI's Perth watermark.** Imperceptible, but present.

**This is interpreting one sentence behind, not simultaneous interpretation.** Nothing is
spoken until its sentence is complete and translated, and grouping adds to that.

**It is a cascade, and errors chain.** If recognition mishears a word, the translation
propagates it and the voice says it out loud in your voice. No stage checks the previous
one.

**Mute Windows and nothing gets transcribed.** Loopback captures after the volume control,
so with the volume down the model receives near-silence. The gain normalizer compensates
and the interface warns in amber when it runs out of headroom, but there is no way around
a muted output through this route.

**Translation cannot start from auto-detect.** NLLB needs the source language, so the room
language has to be a concrete one.

**20 target languages in the interface, 37 in the engine.** The FLORES table maps 37
codes; the dropdown offers 20 concrete locales plus auto-detect, because it reuses the
recognizer's list.

**bfloat16 needs Ampere or newer.** On Turing PyTorch does not fail — it emulates, and the
app crawls with no hint why. The installer reads the compute capability and writes
`float16`; the sidecars check again at startup and warn. The cost of float16 is real and
double: it transcribes worse, inserting filler words nobody said, and it runs slower.

**A second GPU buys nothing as built.** Each sidecar takes a single device (`cuda`, that
is `cuda:0`), so the second card sits idle.

**Windows only.** Capture is WASAPI, with `cfg(windows)` gates throughout `asr-audio`. On
other platforms the crate compiles but device listing and capture return
`UnsupportedPlatform`. Everything above the capture layer is platform-agnostic, so a port
concentrates in a new backend behind the same API — but the recognition model's own
support matrix is the larger unknown.

**Total startup time with everything loaded has never been measured.** What is measured is
per model: Chatterbox about 21 seconds, Kokoro about 6. They load in parallel, so startup
costs whatever the slowest one takes.

---

## If something goes wrong

**Start with the log**, at `%APPDATA%\LiveTranscriber\logs\`. It carries every error the
interface showed you, with a timestamp, plus the size and date of the configuration the
app actually read and the interpreter it resolved. Most of the table below is faster to
settle from those three lines than by guessing.

| Symptom | Probable cause |
|---|---|
| Transcribes very little or nothing | Windows volume low, or muted. `cargo run -p asr-cli -- level --from system` tells you in two seconds |
| No audio at all, zero blocks | The output device was idle. WASAPI fires no events when nothing is playing |
| It crawls | Emulated precision on a pre-Ampere card. Check `dtype` with `verify.ps1` |
| Paragraphs never close | With background music this takes a while by design: the cut waits for the model to stop producing text. Lower `paragraph_idle_secs` |
| `cannot find the sidecar` | Relative path, different working directory. The error lists everywhere it looked |
| The window shows `ERR_CONNECTION_REFUSED` | In development you need Vite. Use `npm run app:dev`, not the bare `.exe` |
| `the synthesizer did not start` | Usually a missing voice WAV, or a voice environment without the engines. `verify.ps1` checks both |
| The voice speaks but the meeting cannot hear it | The voice is going to the speakers instead of `CABLE Input`, or the meeting app does not have `CABLE Output` as its microphone |
| Everything worked yesterday, now the voice device is wrong | Reinstalling VB-CABLE changes device ids. Pick the device again and re-save the profile. Startup waits up to 10 seconds for the output device to actually open and reports the failure rather than claiming the voice is ready |
| The voice lag keeps growing | Chatterbox is generating more slowly than you speak. Raise the grouping size, or switch to Kokoro |
| A device from a profile changed silently | It did not: the app reports every fallback it made, as a visible error |
| Sentences stop arriving mid-session | A sidecar died. The window is told; the log names which one |
| `no Python interpreter is configured` | The `python` key is empty. Run the installer again, or set it by hand; the log's first line says which file the app read |

---

## Status

Verified end to end through the CLI with real loopback audio: capture, normalization,
gate, sidecar protocol, transcription, translation, export, and the virtual microphone
loop with VB-CABLE. The interface has been verified visually across the six view modes,
the meeting view, the profile round trip, the voice section, the subtitle overlay, the
language selector and the global shortcut.

**79 unit tests** in `cargo test --workspace` — 62 in `asr-core`, 13 in `asr-audio`,
4 in the Tauri layer — covering the gate, the normalizer, sentence splitting, FLORES-200
mapping, the transcript, filename generation, configuration round-trips, the echo
registry, the voice grouper (in both directions: grouping while speaking, immediate
while silent), the synthesizer's language mapping, profiles with their device fallback
and atomic save, and per-source translation directions including non-mirrored pairs.

A handful of those exist because of specific failures and would catch them again: the
defaults carrying no absolute path from the machine that built the binary, adopting a
configuration edited from outside, keeping what is in memory when the file is
momentarily unreadable, refusing to write an empty interpreter path over a good one,
and never adopting a configuration left behind in the installation directory by an
older build.

**Not verified:** the tray menu under real clicks. Everything else that used to sit in
this list has since been measured: Chatterbox through this app's own sidecar and under
simultaneous load (see [Measured performance](#measured-performance) — it came out
slower than the isolated bench), the global shortcuts (`CmdOrControl+Shift+T` starts and
stops a real session), and the `%APPDATA%` configuration path from an installed build.

---

## License

[MIT](LICENSE) for the code in this repository. The models it downloads carry their own
licenses, and one of them is restrictive — see [Limits](#limits).

Findings from the development, kept out of this document: [docs/engineering-notes.md](docs/engineering-notes.md).
