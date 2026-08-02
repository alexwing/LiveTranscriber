# LiveTranscriber

Live transcription of **whatever is playing on your PC** — a Teams call, a film, the
browser — and optionally your microphone at the same time. Runs locally, no API bill.

Optional parallel translation into 40+ languages.

Built on [`nvidia/nemotron-3.5-asr-streaming-0.6b`](https://huggingface.co/nvidia/nemotron-3.5-asr-streaming-0.6b)
(FastConformer + RNNT, cache-aware streaming) with
[NLLB-200](https://huggingface.co/facebook/nllb-200-distilled-600M) for translation.

**Windows only** — see [Portability](#portability). Every number below was measured on
one machine (RTX 3060 12 GB, Windows 11), not copied from documentation.

> Source comments and commit messages are in Spanish. The UI is in Spanish too.
> Configuration keys and identifiers are English.

## Getting started

You need an NVIDIA driver and ~15 GB of free disk space. Everything else is
provisioned for you. See [INSTALL.md](INSTALL.md) for details.

```bash
.\scripts\install.ps1 -InstallPython
```

That sets up the Python environment, downloads the models, picks the right precision
for whatever GPU it finds, writes the config with absolute paths, builds the app, and
**checks it actually works** — it launches the sidecar, loads the model onto the GPU
and speaks the real protocol to it.

If something breaks later:

```bash
.\scripts\verify.ps1
```

Day to day:

```bash
npm run app:dev
```

## How it is put together

A Cargo workspace with the logic in crates that know nothing about Tauri, and
`src-tauri` as a thin layer that only translates to the UI.

```
crates/asr-audio    WASAPI capture, gain normalisation, silence gate
crates/asr-core     engine, session, transcript, configuration
crates/asr-cli      headless test bench
src-tauri           commands, tray, shortcuts, events
sidecar/            the Python processes that run the models
src/                React 18 + Vite + TS, typed wrapper over invoke
```

The flow: WASAPI → normaliser → gate → length-prefixed frames over stdin to the
sidecar → the model returns text on stdout → `emit` to the window → React paints it.

### The engines are decoupled on purpose

`asr-core::AsrEngine` and `asr-core::Translator` are traits. Today the only
implementations shell out to Python, which drags in a ~5 GB PyTorch environment.
Swapping either for an ONNX engine in pure Rust means implementing the trait again and
changing the line that constructs it — neither the capture layer nor the UI notices.

## Headless testing

`asr-cli` exists to diagnose things without starting the GUI or loading the model.

```bash
cargo run -p asr-cli -- devices
```

```bash
cargo run -p asr-cli -- level --from system --seconds 10
```

```bash
cargo run -p asr-cli -- run --from system --seconds 30 --language es-ES
```

```bash
cargo run -p asr-cli -- run --from system --seconds 40 --language es-ES --translate-to en-US
```

`level` is the first thing to check when something is wrong: it tells you whether
audio is arriving, at what level, and how much gain is being applied.

## Five things that only turned up by measuring

None of them is obvious, and all five shaped the design.

### 1. Loopback captures *after* the volume control

A source tone at rms **0.6364** came back through loopback at rms **0.00252** —
**48 dB of attenuation**, which is exactly where the Windows volume slider was.

The consequence: with the volume down, the model receives near-silence and transcribes
nothing, with no error to explain it. Hence the gain normaliser
(`asr-audio::Normalizer`), on by default, which tracks the recent peak and rescales.
When it runs out of headroom the UI says so.

And yes: **mute Windows and nothing gets transcribed.** There is no way around that
through this route.

### 2. Audio level is the wrong signal for deciding where a paragraph ends

This was the most expensive design mistake, and it had two lives.

The first version cut paragraphs on an **absolute** dBFS threshold. Wrong twice over:
speech arriving through loopback measured −62 dBFS (see above), so a −50 dBFS threshold
discarded all of it; and with the normaliser compensating by up to ×64, background
noise rises above any fixed threshold and the gate **never closes again** — the whole
session becomes one endless paragraph.

Switching to a threshold **relative** to recent speech fixed the quiet-room case:
there is 20–30 dB between speech and a pause regardless of system volume. But the real
case was still broken: **with background music the level never drops**, so paragraphs
never closed. And since translation fired on paragraph close, it never arrived either.
It looked like "translation takes forever".

The right signal is not the audio, it is **the recogniser itself**. If music is playing
but nobody is speaking, the model emits no text. So a paragraph closes when the model
has gone `paragraph_idle_secs` without transcribing anything new, capped by
`paragraph_max_secs` for uninterrupted monologues.

Verified with music playing continuously and three spoken passages over it: four
paragraphs, each closed and translated.

The audio gate now only decides whether a block is worth spending GPU on, and it
decides that on the **raw** level, before normalisation.

### 3. An idle output device produces no events at all

Capturing loopback from a device with nothing playing returned **zero blocks** — not
blocks of silence. WASAPI simply never fires the event.

That is why the capture loop treats `EventTimeout` as normal rather than an error.
Without it, the app would fail on startup whenever nothing happened to be playing.

### 4. NVIDIA's own streaming example breaks at lookahead 0

With `lookahead = 0` the first chunk covers a single mel frame, so the first chunk of
the loop asks for sample `1×160 − 256 = −96`. NumPy reads that negative as an index
from the end and returns an empty slice, and the STFT blows up. Here it is padded with
silence.

### 5. NLLB drops sentences if you hand it a paragraph

Covered under [Translation](#translation) — it is the reason translation runs
sentence by sentence and is only grouped for display.

## Three more from the installer, which only showed up in a clean room

All three were invisible on the development machine. They surfaced when the installer
ran against a copy of the project with no `.venv` and no configuration.

**`librosa` is required, even though Rust does the capture.** The dependency list was
derived by reading the sidecars' imports and came out short: `transformers`, `numpy`,
`huggingface_hub`. But `NemotronAsrStreamingFeatureExtractor` declares `librosa` as a
*required backend*, so `AutoProcessor.from_pretrained` fails with `ImportError` before
it ever looks at audio. It was already installed in the dev environment from earlier
work, so it never showed.

**`$ErrorActionPreference = "Stop"` kills PowerShell scripts.** Anything an `.exe`
writes to stderr becomes a *terminating* error, and `2>$null` does not prevent it — it
only hides the text. A probe as innocent as "is torch installed?" aborted the installer
with Python's traceback, and a single `pip` warning would have done the same mid-run.
Native calls now go through a wrapper that drops the preference to `Continue` and
decides on the exit code.

**The `.msi` shipped without the translation sidecar.** `tauri.conf.json` declared only
`asr_server.py` as a resource, so an MSI install would have transcribed but failed to
translate. And the bundle lands in `target\release\bundle`, not
`src-tauri\target\release\bundle` — in a Cargo workspace `target` sits at the root.

## And four from the Tauri layer

**COM is per-thread, and Tauri's thread is STA.** WebView2 leaves the thread that
serves Tauri commands in STA, so `initialize_mta()` fails there with
`RPC_E_CHANGED_MODE` (0x80010106) and the device list came back empty with a red error.
`list_devices` now enumerates on its own thread, where the MTA is always clean.

**The working directory cannot be trusted.** `tauri dev` launches the binary from
`src-tauri/`, not the root, so a relative path like `sidecar/asr_server.py` resolved to
`src-tauri/sidecar/asr_server.py` and the app reported *sidecar not found*. An
installed `.exe` or a shortcut has yet another cwd. Relative paths are now searched
across several bases (cwd, the executable's directory walking up, and the bundle's
resource directory), and if nothing matches the error **lists everywhere it looked**.

Careful with the root marker: the sidecar is declared as a bundle resource, so Tauri
also copies it to `target/debug/sidecar/`, which makes it useless for identifying the
real project root. `transcriber-config.example.toml` is used instead, since nothing
copies that.

**Exported transcripts ended up where nobody would look.** Same root cause:
`output_dir` was `"."`, resolved against the process working directory. Under
`tauri dev` that is `src-tauri/`, so `.txt` and `.srt` files appeared there unasked.
The folder is now chosen with a picker (`tauri-plugin-dialog`) and stored absolute; a
relative path in the TOML is ignored in favour of `Documents\LiveTranscriber`. The UI
shows the effective path so there is never any doubt, and the export command receives
only a filename — Rust decides the folder.

Filenames are `YYYY_MM_DD_<name>.<ext>`, with a configurable base name and a
per-format suffix (`_translated`, `_bilingual`). If one already exists that day, `_2`,
`_3`… is appended rather than overwriting: two exports on the same day with the same
name are normal, and silently losing the first one is not.

Installed via MSI the app lives in `Program Files`, where a non-admin user cannot
write, so the config falls back to `%APPDATA%\LiveTranscriber\`. Writability is tested
by actually writing a file, because inspecting ACLs on Windows is unreliable.

**Vite may listen on IPv6 only.** Without an explicit `server.host`, Vite bound to
`::1` while Tauri's `devUrl` points at `127.0.0.1`: the window showed a browser
`ERR_CONNECTION_REFUSED` instead of the UI. `vite.config.ts` now pins
`host: "127.0.0.1"`.

The last two are invisible if you only check that the app "starts": the process lives
and the window title is correct in both cases. It took looking at a screenshot.

## Translation

**The speech model does not translate.** Checked two ways: the model card never
mentions it (its `target_lang` parameter is the *source* language, despite the name),
and the model itself confirms it — all 121 prompts are locales and its 13,089-token
vocabulary contains no task token. There is no way to ask.

So translation is a second, chained step, with NLLB-200 in its own sidecar. A single
one serves every source.

**Translation runs sentence by sentence and is grouped by paragraph for display.**
That combination is the result of two failed attempts:

1. Translating per sentence **and showing per sentence**: choppy, and it did not match
   the transcript, which runs in paragraphs.
2. Waiting for the paragraph to close and translating it whole: two problems at once.
   **NLLB ate content** — given *"La primera parte consiste en capturar el audio del
   sistema. Eso ya funciona bien."* it returned only the first sentence, because it is
   trained at sentence level — and above all the translation **took forever**, since
   nothing appeared until the paragraph closed.

What it does now: each sentence is translated as soon as its punctuation closes it
(~160 ms) and tagged with the paragraph it belongs to. The UI joins sentences sharing a
tag and paints them as one block. One sentence of latency, paragraph presentation, and
nothing lost.

Measured here: **~160 ms per sentence** and **1.27 GB of VRAM**. With the ASR model
that is ~3.7 GB of the card's 12.

**Inherited NLLB limitation:** it needs to know the source language, so translating
requires picking a concrete language rather than *auto-detect*. The app says so instead
of translating from the wrong language.

**Licence:** NLLB-200 is **CC-BY-NC-4.0, non-commercial**. Fine for personal use; for a
product it would have to be swapped for Opus-MT or MADLAD-400, which means implementing
`Translator` again and nothing else.

It is a cascade, with everything that implies: if recognition mishears a word, the
translation propagates the error. This is translated subtitling one sentence behind,
not professional simultaneous interpretation.

## Measured performance (RTX 3060)

| lookahead | latency | RTFx | concurrent streams |
|---|---|---|---|
| 0 | 80 ms | 1.8x | ~1 |
| 3 (default) | 320 ms | 4.6x | ~4 |
| 6 | 560 ms | 6.3x | ~6 |
| 13 | 1120 ms | 9.4x | ~9 |

Capturing system audio **and** the microphone at once means two sessions, each with its
own Python process and its own copy of the model in VRAM (~2.4 GB each). At lookahead 3
the 3060 handles it comfortably.

## Portability

### Another NVIDIA GPU on Windows

Works unchanged, with one caveat about precision. The model card lists *"NVIDIA Ampere,
NVIDIA Blackwell, NVIDIA Hopper, NVIDIA Jetson, NVIDIA Lovelace, NVIDIA Turing, NVIDIA
Volta"*, so Turing (RTX 20xx) and Volta are covered.

But **bfloat16 needs Ampere or newer** (capability 8.0+). On Turing PyTorch does not
fail, it *emulates*, and the app crawls with no hint why —
`is_bf16_supported()` returns `True` via emulation unless you pass
`including_emulation=False`. The installer reads the capability and writes `float16`;
the sidecars check again at startup and warn.

The cost is real: measured on this model, float16 transcribes worse (it inserts filler
words that were never said) and runs slower than bf16 (RTFx 8.7 versus 15.7). float32
loses no quality but raises VRAM and lowers throughput.

**Multiple GPUs buy nothing as built.** Each sidecar takes a single device (`cuda`,
i.e. `cuda:0`), so a second card would sit idle. Splitting them across cards is a small
change — pass `--device cuda:1` to one sidecar — but not worth it: one card already
handles ~4 streams at 320 ms and this uses one or two. The GPU is not the bottleneck.

### macOS / Apple Silicon

**Does not work today**, and there are two independent problems.

**Capture is Windows-only by construction.** WASAPI, with 17 `cfg(windows)` gates in
`asr-audio`. On macOS the crate compiles but `list_devices` and `spawn_capture` return
`UnsupportedPlatform`: the app would start and capture nothing. Everything above it —
`asr-core`, Tauri, React — is platform-agnostic, so the work concentrates in a new
backend behind the same API.

The good news is that a virtual device like BlackHole is no longer needed: macOS 14.4
added *Core Audio Process Taps*, which capture system audio with the user's permission,
and [cpal](https://github.com/RustAudio/cpal/releases) ships CoreAudio loopback for
macOS > 14.6.

**The risky part is not the port, it is the model.** The model card lists only *"Linux,
Linux 4 Tegra"* as the operating system, and only CUDA architectures. On a Mac you
would go through MPS, where the concrete risks are the RNNT's LSTM decoder and
bfloat16 coverage. The fallback is CPU, and there is no RTFx measurement for CPU or M1
here — every figure in this README comes from a 3060.

Before writing a line of macOS capture code, the sensible move is to test the model
alone on the Mac with a twenty-line script. If MPS cannot keep up with real time, the
audio port is moot.

## Design notes

**The gate does not drop short silences.** The model is cache-aware and its state
assumes contiguous audio; removing chunks would produce garbage at the joins.
Everything within a paragraph is passed through.

**Per-process loopback.** `CaptureTarget::Process { pid }` uses
`ActivateAudioInterfaceAsync` to capture only one process's audio — Teams without the
music playing alongside it. Implemented in the crate and exposed in the CLI (`--pid`),
but not yet in the UI.

**Closing the window does not quit the app**, it sends it to the tray.

## Status

Verified end to end through the CLI with real loopback audio: capture, normalisation,
gate, sidecar protocol, transcription, translation and export to `.txt` and `.srt`. The
UI has been verified visually — both tabs render, the split layout works, the low-volume
warning fires, and the global shortcut starts a session.

39 unit tests in `cargo test --workspace` covering the gate, the normaliser, sentence
splitting, FLORES-200 language mapping, the transcript, filename generation and
configuration round-trips.

**Not verified:** the tray menu and global shortcuts responding to actual clicks and
key presses beyond the one start/stop test, and the `%APPDATA%` config fallback, which
needs a real MSI install to exercise.
