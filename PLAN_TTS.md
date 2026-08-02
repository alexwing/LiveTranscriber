# Plan: voice output with cloning (TTS)

What I say in Spanish should come out of a virtual microphone, in another
language, in my own voice. What everyone else says is still **read** on screen,
as it is now.

Everything claimed below is **measured on this machine** (RTX 3060 12 GB,
Windows 11) against the voicebox backend at `E:\projects\voicebox`, not copied
from documentation.

## Status: implemented (phases 1-4)

It is built and verified through phase 4. The feature is **entirely optional**:
its own `[speak]` section in the TOML, its own "Speak for me" panel in the UI,
and switched off it costs nothing (no process, no VRAM). It requires
translation + microphone, because what gets spoken is the translation of the
microphone; never other people's sentences.

| piece | where | verified |
|---|---|---|
| WASAPI output | `asr-audio/src/render.rs` | plays through the chosen device, auto-converts from 24 kHz mono |
| Trait + sidecar + grouper + echo | `asr-core/src/speak.rs` | 10 tests + the real protocol |
| Voice sidecar | `sidecar/tts_server.py` | kokoro 103 ms warm; chatterbox end to end |
| Wiring + events | `src-tauri/src/lib.rs` | compiles; shutdown chain over channels |
| UI | `App.tsx` (SpeakPane), `tauri.ts` | tsc clean; queue visible; echo dimmed |
| CLI | `asr-cli speak` | **full loop measured**: kokoro es, 5.15 s of audio in 1,233 ms (4.18x), played back over WASAPI in exactly 5.15 s |

After implementing, it went through an adversarial multi-agent review (6 lenses +
verification by reproduction) and a transcribe-back test. What came out of it
and got fixed: **chatterbox truncates multi-sentence blocks on a seed lottery**
(isolated with a matrix of 8 runs: only the seed matters; fixed with detection
by ms per character — complete 47.6, truncated 30.8, threshold 38 — and a retry
that keeps the longer audio, verified by transcribing it back); **the death of
the output device was silent** (there is now a startup handshake that fails
startup with a clear message, and the pump reports an error if the render dies
halfway through); **"Stop" did not silence the voice** (there is now a stop
handle: whatever is pending is dropped on stop, and overlapping two synthesizers
on a fast stop-and-start is avoided); **echo is checked on both sources** (on
the microphone too: if the voice plays through speakers, the mic picks it up and
without this it would speak itself back in a loop); plus more validation of the
reference WAV at startup, NaN audio detection, warm-up of the kokoro voice, and
the `.gitignore` that was swallowing `requirements-tts.txt`.

Three things that turned up when actually testing it, none of them in the plan:

1. **numba/SVML kills the process** here too (this machine's
   `LLVM ERROR: __svml_cosf8_ha`): `prepare_conditionals` goes through librosa.
   The sidecar shields itself (`NUMBA_DISABLE_INTEL_SVML=1` before the imports),
   because it cannot depend on the environment of whoever launches it.
2. **Chatterbox prints to stdout during `generate`** (`loaded PerthNet...`), and
   one stray line broke the one-JSON-line protocol. The sidecar duplicates the
   real stdout for the protocol and redirects fd 1 to stderr: no print from any
   library can ever touch it again.
3. **Without the `eager` attention patch** chatterbox's alignment analyzer is
   left with no weights (sdpa ignores `output_attentions`) and the process
   sometimes generates and sometimes dies with no traceback. It is the same
   patch voicebox applies; reproduced and applied.

**The virtual microphone loop is verified end to end** with VB-CABLE installed
(Pack 45, Vincent Burel's signature checked): the cloned voice went in through
`CABLE Input` and Nemotron's own ASR, listening on `CABLE Output`, transcribed
it **word for word, punctuation included** (8.36 s of audio at RTFx 1.00x).
That is: what Teams would hear is exactly what was said.

That test also uncovered the **root cause of the truncation**, which was not
just a lottery: in chatterbox's `alignment_stream_analyzer.py` the cutoff for
"excessive token repetition" says *3x same token in a row* in its comment, but
the code looks at only the LAST TWO, and the `self.complete and` guard is
commented out in the library itself. Two identical silence tokens — a pause
between sentences — decapitate the audio mid-generation. A text with a short
first sentence died 3 out of 3 times in the same place. `tts_server.py`
neutralizes it by trimming the token window before each step (the good alignment
detectors stay active), and the ms-per-character retry stays as a net. Note:
**voicebox has the same latent bug** in its long generations.

Pending (phase 5): `install.ps1` with the second venv (`requirements-tts.txt`
already exists and documents why it does not fit in the ASR venv), `verify.ps1`
and `INSTALL.md`. And the social test: a real Teams meeting with `CABLE Output`
as the microphone.

## Which engine, and why

Measured with a freshly started backend per engine (so that none inherits the
previous one's VRAM), a one-sentence text (83 characters), `seed=1234`, 1
warm-up and 3 measurements. The best warm time is given.

| engine | language | cold | warm | audio | RTFx | VRAM | languages | license |
|---|---|---:|---:|---:|---:|---:|---:|---|
| Kokoro 82M | en | 6,723 ms | 115 ms | 5.35 s | 46.6x | 559 MB | 8 | Apache 2.0 |
| Kokoro 82M | es | 5,815 ms | 111 ms | 5.30 s | 47.9x | 557 MB | 8 | Apache 2.0 |
| **Chatterbox ML** | en | 21,180 ms | 4,435 ms | 3.74 s | 0.84x | 3,400 MB | **23** | **MIT** |
| **Chatterbox ML** | es | 21,694 ms | 4,565 ms | 4.24 s | 0.93x | 3,399 MB | **23** | **MIT** |
| **Chatterbox ML** | de | 20,940 ms | 4,606 ms | 4.46 s | 0.97x | 3,399 MB | **23** | **MIT** |
| Qwen 1.7B | en | 23,837 ms | 11,382 ms | 5.12 s | 0.45x | 4,046 MB | 10 | — |
| Qwen 1.7B | es | 23,038 ms | 12,618 ms | 5.68 s | 0.45x | 4,046 MB | 10 | — |

**With cloning → Chatterbox Multilingual.** It beats Qwen on all three
dimensions at once: 2.7x faster, lighter, and with more than twice the
languages. And it covers German, Russian and Korean, which Kokoro lacks — which
is what makes it viable to change the target language later on.

**Without cloning → Kokoro 82M.** 111 ms and 47.9x. It stays as an alternative
mode (neutral voice) and as a safety net if Chatterbox cannot keep pace.

LuxTTS and Chatterbox Turbo were ruled out: they are **English only**, and that
clashes with the requirement to change language. LuxTTS was the fastest with
cloning (301 ms, 13x), so if English ever gets locked in as the only target, it
is worth revisiting.

## The number that shapes the design: RTFx > 1

For sustained speech what decides it is not latency, it is whether RTFx
clears 1. Below it, you generate more slowly than it plays back and **the
delay grows without settling** for as long as you keep talking. Above it,
the lag stays bounded to one sentence.

Chatterbox sits right on the edge (0.84–0.97x on a short sentence), but with a
long text (330 characters) it rises to **1.02–1.03x**. Fitting the cost over the
English pair (3.74 s and 15.9 s of generated audio):

- **fixed cost per call ≈ 1 s**
- **marginal RTFx ≈ 1.09x**

The fit predicts the long German case to within 0.1 s (17.2 s against 17.1 s
measured) and is off by ~0.5 s on the short ones, so it is approximate. But it
makes clear where the problem is: **the 0.84x of the short sentences is almost
entirely that fixed second.**

### Where that second comes from (measured)

In voicebox, `chatterbox_backend.py` stores the voice prompt as a plain path and
returns `False` (not cached), and then passes `audio_prompt_path=ref_audio` to
`model.generate()` **on every call**, so Chatterbox re-encodes the reference
audio on every sentence.

To measure whether avoiding it recovers the fixed cost, the model was called
directly with the real reference WAV, the same seed and the same parameters the
backend uses:

| strategy | per sentence | audio | RTFx |
|---|---:|---:|---:|
| `audio_prompt_path` on every call | 4,015 ms | 3.88 s | 0.97x |
| `prepare_conditionals()` once | **3,771 ms** | 3.88 s | **1.03x** |

**The saving is 244 ms (6%), not the whole second the fit suggested.**
Re-encoding the voice prompt costs ~0.24 s; the rest of the fixed cost is in the
startup of the autoregressive decoder and the codec, and no caching removes it.

Even so it is worth doing: it is **a single call at startup** and it crosses the
1.0x threshold, which is exactly the sign that decides whether the delay stays
bounded or grows.

Second finding from the same measurement: the direct API gives **4,015 ms**
against voicebox's **4,435 ms** over HTTP. Those ~420 ms are its layer
(normalization, effects chain, WAV encoding, HTTP transport). Together with the
caching, **our own sidecar performs ~15% better** than calling voicebox:
3,771 ms against 4,435 ms. That is the quantitative argument for phase 2 and
against staying on the HTTP probe.

Robustness note observed during the test: Chatterbox emitted
`Detected 2x repetition of token` and forced EOS. Voicebox has a runaway
detector (`engine_retries_runaway`) precisely for this; our sidecar needs
something equivalent or some sentence will come out cut short.

## Architecture

Same shape we already use: the logic in crates that are independent of Tauri,
and the engine behind a trait so it can be swapped without anything else
noticing.

```
crates/asr-core/src/tts.rs           trait TtsEngine + TtsEvent + TtsError   (mirror of engine.rs)
crates/asr-core/src/tts_sidecar.rs   PythonTtsSidecar                        (mirror of sidecar.rs)
crates/asr-core/src/speech_out.rs    grouper + ordered queue + playback
crates/asr-audio/src/render.rs       WASAPI output to a specific device      (NEW)
sidecar/tts_server.py                the engine (Chatterbox | Kokoro)        (mirror of mt_server.py)
src-tauri/src/lib.rs                 commands and events
```

`asr-audio` today **only captures**. Getting audio out to a chosen device is a
new capability, and it is the only piece with no precedent in the repo.

### The sidecar protocol

Same frame as `mt_server.py`: `u32 length | u8 type | payload`, type `0x02` JSON
control, one JSON line per response on stdout, and the `id` traveling back so
they can be paired without assuming order.

```
stdin  (type 0x02, JSON utf-8)
    {"cmd":"speak","id":12,"text":"...","lang":"en","voice":"clone"}
    {"cmd":"shutdown"}

stdout (one JSON line per message)
    {"t":"ready","device":"cuda","engine":"chatterbox","dtype":"float16","rate":24000}
    {"t":"audio","id":12,"pcm":"<base64 i16 LE mono>","rate":24000,"ms":4560}
    {"t":"error","id":12,"message":"..."}
```

The only real difference from translation is that the response is audio. It goes
as **base64 of PCM i16** inside the JSON line, instead of raw bytes, so as not
to break the line protocol and to be able to reuse the reader in `sidecar.rs`.
Cost: a 4 s sentence at 24 kHz i16 is 192 KB, 256 KB in base64 — negligible next
to the 4.5 s it takes to generate. If it ever becomes a nuisance, a binary frame
type gets added on stdout.

`pick_dtype` is copied as-is from `mt_server.py`: the problem of emulated
bfloat16 on Turing is the same here.

### Where it hooks into the pipeline

`SentenceSplitter::push` (`translate.rs`) already returns closed sentences, and
`TranslatedSentence` already carries its `paragraph`. **TTS hooks into the
translated-sentence event, not the paragraph close** — it is exactly the lesson
already documented in the README for translation, and here the cost of getting
it wrong is worse: the others would wait a whole `paragraph_idle_secs` before
hearing you.

The full output flow:

```
mic → gate → ASR → SentenceSplitter → NLLB (es→target) → grouper → TTS → ordered queue → render → CABLE Input
```

### The grouper

A direct consequence of the fixed cost per call. It accumulates translated
sentences and releases the block on whichever comes first:

- **N characters** accumulated (~250–300, where >1x was already measured), or
- **T ms** since the first pending sentence (so that a lone sentence is not left
  waiting).

Both configurable. That is how RTFx > 1 is reached without unbounded latency. If
the `prepare_conditionals` hypothesis is confirmed, N can drop a lot or the
grouper can stay at `N=1`.

### Playback order

Sentences must play in order even if generation finishes out of order. The `id`
already comes back from the sidecar, so a reorder buffer indexed by `id` solves
the case.

## Virtual microphone

**On Windows you cannot create an audio input device from user code.** It takes
a signed kernel driver; it is not solvable from Rust.

The practical route is **VB-CABLE**: it installs a pair of devices, `CABLE
Input` (playback) and `CABLE Output` (recording). We render to `CABLE Input`; in
Teams the user picks `CABLE Output` as the microphone.

Three consequences:

1. **It removes the feedback risk.** The TTS never touches the speakers, so
   loopback capture does not pick it up. Per-PID filtering is not needed for
   this.
2. **The cable format should be pinned to 24 kHz mono**, which is what
   Chatterbox and Kokoro put out, so that Windows does not resample on its own.
3. **The user installs it separately** and it has its own license. The installer
   must detect its absence and say so clearly, not fail opaquely.

## Process lifetime: the Job Object

While measuring this, an orphaned `multiprocessing.spawn` process turned up that
had outlived its backend **holding 11.6 GB of VRAM**: killing it took the GPU
from 12,045 down to 447 MiB. It was also running the system Python, not the
venv's, so it does not show up where you would look for it.

A `taskkill` on the parent PID **is not enough**. The sidecar has to be launched
inside a **Job Object** with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`
(`CreateJobObjectW` + `SetInformationJobObject` + `AssignProcessToJobObject`),
which kills the whole tree including the `multiprocessing` children. Without
that, a dirty shutdown leaves the card unusable until you reboot.

This applies just the same to the ASR and translation sidecars that already
exist.

## VRAM budget

| component | VRAM |
|---|---:|
| ASR Nemotron | 2.40 GB |
| NLLB-200 | 1.27 GB |
| Chatterbox ML | 3.40 GB |
| Kokoro (if loaded at the same time) | 0.56 GB |
| desktop | ~0.87 GB |
| **total with Chatterbox** | **~7.9 GB** |
| **total with both** | **~8.5 GB** |

It fits in the 12 GB with room to spare. Both paths can be kept loaded without
unloading and reloading models.

## Cold start

Chatterbox takes **21 s** to load. It has to be preloaded when the app starts,
behind a visible "preparing voice" state, never on the first sentence. Kokoro is
~6 s.

## Phases

Each phase ends in something verifiable without the next one.

**Phase 0 — probe, without writing a sidecar.** Validate quality and latency by
calling the voicebox backend over HTTP (`POST /generate/stream`, port 17493).
Cheap, and it answers the one question no number settles: whether the cloned
voice is convincing. *Partly done: there are samples generated in Spanish,
English and German.*

**Phase 1 — `asr-audio::render`.** WASAPI output to a device by name, plus an
`asr-cli` command that plays a `.wav` on it. Verifiable with no model at all: if
it plays on `CABLE Input` and Teams hears it through `CABLE Output`, the phase
is closed.

**Phase 2 — `tts_server.py` + `PythonTtsSidecar`.** `prepare_conditionals()`
once only, at startup (already measured: 244 ms per sentence and it crosses
1.0x) and a runaway detector. Command `asr-cli speak --text ... --lang en`. Our
own sidecar pays for itself: ~15% faster than going to voicebox over HTTP.

**Phase 3 — wiring the pipeline.** Grouper, ordered queue and the hook into the
translated-sentence event in `session.rs`. Verifiable end to end with the CLI:
speak into the mic and have it come out of the cable in another language.

**Phase 4 — Tauri and UI.** Commands and events; selector for engine
(Chatterbox/Kokoro), for voice, for output device, and a switch. Show the
**queue depth**, which is the signal that you are falling behind.

**Phase 5 — installer.** The new sidecar's dependencies in `install.ps1`, a
model pre-check, and VB-CABLE detection with a clear message if it is missing.
Mind what was already learned: `$ErrorActionPreference = "Stop"` kills these
scripts, and the new sidecar has to be declared as a resource in
`tauri.conf.json` or the `.msi` will ship without it.

## Risks

- **Marginal RTFx.** 0.84–1.03x leaves little room. If it falls short in real
  use, the ways out are the grouper, `prepare_conditionals`, or dropping to
  Kokoro with a warning.
- **This is delayed interpreting, not simultaneous.** With the full cascade,
  several seconds pass between closing a sentence and being heard. The UI has to
  make that evident instead of looking like it has hung.
- **All Chatterbox audio carries Resemble AI's Perth watermark**, imperceptible
  but present. It is not an impediment; it is worth knowing.
- **NLLB is still the only commercial blocker** (CC-BY-NC-4.0). The chosen TTS
  adds none: Chatterbox is MIT and Kokoro Apache 2.0.
- **Cascading errors.** If the ASR mishears, translation propagates it and now
  it is also spoken in your voice. Listing on screen what has been said in your
  name becomes a feature, not a luxury.

## Unverified

- Chatterbox **with the ASR running at the same time**. All my measurements are
  of the TTS alone; contention for the GPU could make them worse. It is the most
  relevant open risk, because the margin over 1.0x is 3%.
- **TADA 3B** (10 languages, with cloning) was left unmeasured: it is 8 GB and
  Chatterbox already covers 23 languages.
- The behavior of the **grouper** with real conversational sentences, which are
  shorter and more irregular than the test text.

Already verified and therefore off this list: the **quality of the cloned voice**
(approved by listening to samples in Spanish, English and German) and the effect
of **`prepare_conditionals`** (244 ms, table above).
