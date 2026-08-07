# Installing from scratch

To get LiveTranscriber onto a freshly built Windows. It is designed and tested
for a machine with an NVIDIA GPU; without one the model falls back to CPU and
won't keep up in real time.

## What you need beforehand

Only two things, and the script installs neither:

| Requirement | Why | How |
|---|---|---|
| **NVIDIA driver** | PyTorch needs CUDA 12.8 | [nvidia.com/drivers](https://www.nvidia.com/Download/index.aspx) |
| **~15 GB free** | 4 GB of PyTorch + 7 GB of models | — |
| **~27 GB with `-WithVoice`** | the voice adds another venv with its own torch (~7 GB) and ~4 GB of models | — |

Python is installed by the script if you pass it `-InstallPython`. Rust and Node
are only needed if you want to **build** the application; not to provision the model.

## Two ways in, and you only need one

**From the release.** Download the `.exe` from
[Releases](https://github.com/alexwing/LiveTranscriber/releases), install it, and
provision from the app's own folder — the installer carries the scripts inside it, so
there is nothing to clone:

```powershell
& "$env:LOCALAPPDATA\LiveTranscriber\install.cmd" -InstallPython -WithVoice
```

(That is where the `.exe` installer puts it. The `.msi` puts the same files under
`C:\Program Files\LiveTranscriber\`.)

The environments and models land in `%LOCALAPPDATA%\LiveTranscriber\`, and the
application step is skipped: you already have the `.exe`.

**From source**, if you want to build it yourself:

```powershell
git clone https://github.com/alexwing/LiveTranscriber
cd LiveTranscriber
.\install.cmd -InstallPython
```

Use `install.cmd` and not the `.ps1` directly. A freshly installed Windows has its
execution policy on `Restricted` and refuses to run a `.ps1`, and a file downloaded from
the internet carries the mark-of-the-web on top of that. Both stop you at your very first
command. The `.cmd` is not subject to either.

## Running it again, and the switches

If the C: drive is tight, put the models somewhere else:

```powershell
.\install.cmd -ModelsDir D:\models
```

With the synthetic voice (speaking your translation through a virtual microphone):

```powershell
.\install.cmd -WithVoice
```

It can be run again without breaking anything. It reuses whatever is already there, and
over an existing install it **keeps your settings** — it rewrites only the handful of
keys that describe this machine (the interpreter paths, the precision, the model cache)
and leaves the other twenty-nine alone.

`-Force` rebuilds the virtual environments. `-ResetConfig` is the one that replaces the
configuration, and it keeps a dated backup next to it.

### Switches

| Option | What for |
|---|---|
| `-ModelsDir <path>` | Models outside `%USERPROFILE%\.cache`. Writes `hf_home` into the configuration |
| `-SkipTranslator` | Don't download NLLB. Saves ~2.4 GB if you aren't going to translate |
| `-WithVoice` | Set up the synthetic voice (speaking your translation through a virtual microphone). It is opt-in because it costs ~11 GB more |
| `-SkipBuild` | Don't build the app. Python and models only |
| `-SkipVerify` | Skip the final check. Not recommended |
| `-InstallPython` | Install Python 3.12 with winget if there is no valid one |
| `-Force` | Rebuild the virtual environments. Your settings are kept |
| `-ResetConfig` | Replace the configuration with a fresh one, keeping a `.bak-<date>` copy |

## What it does, in order

1. **Sizes up the machine**: 64-bit, disk space, and which GPU is there. It reads
   the *compute capability* and decides the precision (see below).
2. **Looks for a valid Python**: 3.10 to 3.13. **3.14 won't do** even if it is
   installed, because PyTorch doesn't publish wheels for it yet.
3. **Creates the virtual environment** and installs PyTorch from the `cu128` index.
4. **Checks that the wheel works for your card.** `cu128` ships `sm_75` and up,
   so it covers Turing (RTX 20xx) and later but **not** a GTX 10xx. If it doesn't
   fit, it stops and says so, instead of failing later at runtime.
5. **Installs the sidecar dependencies** from `sidecar\requirements.txt`
   (`transformers`, `numpy`, `huggingface_hub` and `librosa` — we don't use that
   last one ourselves, the model's feature extractor demands it on import).
6. **Downloads the models** by actually loading them, not with a `snapshot_download`.
   That way it only pulls what transformers uses (the ASR repo also includes a
   2.4 GB `.nemo` file we never touch) and it checks the weights are sound along
   the way.
7. **(With `-WithVoice`) Sets up the voice environment**: a **second** venv in
   `.venv-tts` with its own torch, the dependencies from
   `sidecar\requirements-tts.txt` and `chatterbox-tts` installed `--no-deps` (its
   pins would reinstall a torch without CUDA). It doesn't share a venv with the ASR
   because they can't: the ASR requires `transformers>=5.13` and chatterbox is
   tested against 4.57.x. It checks that both engines import and downloads their
   models (chatterbox's multilingual weights and all of kokoro).
8. **Writes `transcriber-config.toml`** with absolute paths and whichever precision
   applies (and the `[speak]` section if there was a `-WithVoice`).
9. **Builds the application** if Rust and Node are there, leaving the installer in
   `target\release\bundle`.
10. **Verifies**, and this is the part that matters.

## The precision picks itself, and there's a reason

`bfloat16` needs **Ampere or newer** (capability 8.0+). On a Turing — a 2080, a
1660 — PyTorch **doesn't fail**: it *emulates* it. The application would crawl with
no hint as to why, because `is_bf16_supported()` returns `True` unless you pass it
`including_emulation=False`.

The installer reads the capability and writes `float16` when it has to. The sidecars
also check it at startup and warn in the log, in case someone edits the TOML by
hand.

The cost is real: measured on this model, `float16` transcribes slightly worse than
`bfloat16` (it inserts filler words nobody said) and runs slower. With `float32`
there is no quality loss, but VRAM goes up and the pace goes down.

## Two pieces, and why it can't be one

The Windows installer (`.msi` / `.exe` in `target\release\bundle`) carries
**only the application**: a few MB. It can't carry the Python environment or the
models, because they are ~12 GB and putting them in an MSI makes no sense.

So the split is:

| Piece | Who provides it | Size |
|---|---|---|
| Application (window, capture, tray) | the `.msi` | a few MB |
| Python + PyTorch | `install.ps1` | ~4 GB |
| Models | `install.ps1` | ~7 GB |
| Synthetic voice (its own venv + models) | `install.ps1 -WithVoice` | ~11 GB |
| VB-CABLE (virtual microphone) | by hand, [vb-audio.com/Cable](https://vb-audio.com/Cable/) | 1 MB |

The `.msi` carries the Python sidecars inside it as bundle resources, so the app
finds them on its own. What it needs is an interpreter with the dependencies, and
`install.ps1` writes that path into the configuration. **There is no field for it in
the interface**: if it is wrong, run `install.ps1` again or edit the file.

**The configuration lives in one place, and it is not next to the `.exe`:**

```
%APPDATA%\LiveTranscriber\transcriber-config.toml
%APPDATA%\LiveTranscriber\transcriber-profiles.toml
```

That is where `install.ps1` writes it, where `verify.ps1` reads it, and where the app
looks — installed or not. It has to be a single agreed location, because the installer
runs from a cloned repository and the installed app runs from `%LOCALAPPDATA%` or
`Program Files`: anything derived from "wherever I happen to be" gives two files that
never meet. (It used to, and that is how a colleague ended up running the app against
paths from someone else's machine.)

Two consequences worth knowing. `Program Files` is not writable by a non-admin user, so
storing the configuration beside the `.exe` would lose every change on close. And if you
had a configuration in the old location, the app copies it — profiles included — the
first time it starts.

For development, `npm run app:dev` sets `LIVETRANSCRIBER_CONFIG` to the repository's own
`transcriber-config.toml`, so the project's configuration is used instead of your
personal one. Setting that variable by hand overrides the location everywhere: app,
`install.ps1` and `verify.ps1`.

## Where the models end up, and why they aren't duplicated

They go into the Hugging Face cache, which is **shared by everything you use on this
machine**:

```
%USERPROFILE%\.cache\huggingface\hub\
    models--nvidia--nemotron-3.5-asr-streaming-0.6b\    2.4 GB
    models--facebook--nllb-200-distilled-600M\          4.6 GB
    models--ResembleAI--chatterbox\                     3.4 GB   (-WithVoice only)
    models--hexgrad--Kokoro-82M\                        0.4 GB   (-WithVoice only)
```

That has a useful consequence: if you already had them from another project, the
installer **downloads nothing**. `fetch_models.py` opens them, checks they are
complete and moves on. Its output says so: *"everything was already cached"* versus
*"downloaded: X GB"*.

With `-ModelsDir` the cache moves wherever you say and `hf_home` is written into the
configuration. The sidecars receive it as the environment variable `HF_HOME`, which
is the only way the Python library will see it.

The virtual environment, by contrast, is per project and is **not** shared
automatically: it is ~4.7 GB of PyTorch. If you point the configuration at
another project's `python.exe`, it works — `verify.ps1` detects it and flags it
as `SHARED` — but it is then tied to that project staying where it is.

## Verifying when something fails

```bash
cd E:\projects\LiveTranscriber; .\scripts\verify.ps1
```

It goes through each piece in dependency order and stops at the first one that
fails, telling you what to do. The last thing it does is what really proves the
install: **it launches the ASR sidecar, loads the model onto the GPU and speaks the
real protocol to it** — it waits for the `ready`, sends it three seconds of audio,
asks for a `reset` and checks that the segment closes.

It doesn't measure transcription quality: it sends a tone, not speech. Getting no
text out is normal and it says so. What it checks is that the whole pipeline works,
which is where installation failures are.

If the synthetic voice is enabled in the configuration, it also checks its pieces:
the voice venv starts, chatterbox and kokoro import, the sidecar exists, a voice WAV
has been chosen, and the model is in the cache (that last one in yellow if it is
missing: it downloads itself on first use). Disabled, it says so and it doesn't
count as a failure — it is optional.

## Starting up

```bash
cd E:\projects\LiveTranscriber; npm run app:dev
```

**Turn the Windows volume up before testing.** The loopback captures *after* the
volume control: with the volume low, what reaches the model is practically silence.
The app warns in amber when it happens, but better to save yourself the scare.

## If something goes wrong

The log is at `%APPDATA%\LiveTranscriber\logs\`, one file per day. The application is
built windowed and has no console, so that file is the only place its own account of
what happened exists.

| Symptom | Probable cause |
|---|---|
| `cannot find the sidecar` | Relative path and a different working directory. The error lists where it looked |
| `no Python interpreter is configured` | Nothing has been provisioned yet on this machine. Run the installer; the message carries its full path |
| The window comes up with `ERR_CONNECTION_REFUSED` | In development you need Vite. Use `npm run app:dev`, not the bare `.exe` |
| No audio coming in, zero blocks | The device was idle: WASAPI doesn't generate events if nothing is playing |
| Transcribes very little or nothing | Windows volume low. `cargo run -p asr-cli -- level --from system` tells you in two seconds |
| It crawls | Emulated precision. Check `dtype` with `verify.ps1` |
| Doesn't close paragraphs | With background music it is normal for it to take a while: the cut waits for the model to stop transcribing. Lower `paragraph_idle_secs` |
| `the synthesizer did not start` | The reason is in the log, `synthesizer` lines. The usual: the voice WAV is missing, or the voice venv doesn't have the engines (`verify.ps1` checks it) |
| The voice speaks but the meeting can't hear it | The TTS is going out through the speakers instead of through `CABLE Input`, or Teams doesn't have `CABLE Output` as its microphone. Careful: when it installs, VB-CABLE makes itself the Windows **default output**; put that one back to your speakers |
