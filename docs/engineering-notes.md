# Engineering notes

Development notes from building LiveTranscriber: findings that shaped the design and
are worth keeping, but that do not belong in the README. Most of the text below is
preserved as it was first written, so a few figures have since been superseded — the
Volta claim under Portability is wrong (the `cu128` wheel starts at `sm_75`, so Turing
is the floor), and the README is the current source for every number.

---

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

---

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

---

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
only the format — the name, the date, the suffix and the folder all come from the
configuration.

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

---

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

**Inherited NLLB limitation:** it needs to know the source language, so translating
requires picking a concrete language rather than *auto-detect*. The app says so instead
of translating from the wrong language.

---

## Portability: macOS / Apple Silicon

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
here — every figure in this project comes from a 3060.

Before writing a line of macOS capture code, the sensible move is to test the model
alone on the Mac with a twenty-line script. If MPS cannot keep up with real time, the
audio port is moot.

---

## The gate does not drop short silences

The model is cache-aware and its state assumes contiguous audio; removing chunks would
produce garbage at the joins. Everything within a paragraph is passed through.

---

## Notes from the voice work

**An orphaned Python child can hold the whole card.** A `multiprocessing.spawn` child
can outlive the backend that started it. Measured once: 11.6 GB of VRAM retained by a
process nothing pointed at any more; killing it took the card from 12,045 MiB to
447 MiB.

Shutdown today is cooperative — a `shutdown` frame, stdin closed, five seconds of
grace, then `kill()` on the child ([`sidecar.rs`](../crates/asr-core/src/sidecar.rs)).
That covers the sidecar itself but not a grandchild it spawned. The robust fix is a
Windows Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, which kills the whole
tree when the handle closes. **Not implemented**; it is listed as pending work in
[`PLAN_TTS.md`](../PLAN_TTS.md).

**Loading in series made the window look dead.** With the models loaded one after
another the voice was ready at 46 s and the translator had not started at 77 s, with no
output in between. Each model now loads on its own thread and emits a `loading` event as
it goes. Both handles are always joined, even when the first one fails, so a failure
does not leave a Python sidecar orphaned on the GPU.

**Chatterbox prints to stdout during `generate`.** The sidecar protocol is one JSON line
per message, and a single stray line breaks it. The voice sidecar duplicates the stdout
descriptor for the protocol and redirects fd 1 to stderr before loading the model. Audio
comes back over that same channel as PCM i16 little-endian in base64.

**Chatterbox's fixed cost is about one second per request, and caching barely dents
it.** Calling `prepare_conditionals()` once takes a sentence from 4,015 ms to 3,771 ms
(244 ms, 6%), which is enough to cross 1.0x (0.97x to 1.03x) but no more. Only that
~0.24 s is re-encoding the reference voice. The rest is the autoregressive decoder and
the codec starting up, and no cache removes it. That second is the whole reason the
sentence grouper exists.

**The echo memory is not a mute window.** Using the echo entry's lifetime to silence the
capture was tried and it ate whole sentences the user had actually said. The entry is
only ever used to mark a sentence as an echo, never to stop listening.

**A device that enumerates can still fail to open.** Startup waits up to 10 seconds for
the voice output device to open for real, because the alternative is an interface
reporting the voice as ready while nobody in the meeting can hear it.

**Sessions are collected before the voice is silenced.** Done the other way round, the
last sentence was lost every time.

**Kokoro builds its pipeline lazily.** About 3 seconds on first use, on top of the ~6 s
load. The sidecar warms it up before reporting `ready`, so the cost does not land on the
first sentence of a meeting.
