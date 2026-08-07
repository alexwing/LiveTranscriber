<#
.SYNOPSIS
    Gets LiveTranscriber ready to run on a freshly built Windows.

.DESCRIPTION
    Provisions the Python environment, downloads the models, writes the
    configuration with the right absolute paths and checks that everything
    really starts. It can be run again without breaking anything.

    What it does NOT do: install the NVIDIA driver. That comes first, by hand.

.PARAMETER ModelsDir
    Where to keep the models (~7 GB). Defaults to the Hugging Face cache in
    the user profile. Useful if the C: drive is tight.

.PARAMETER SkipTranslator
    Don't download NLLB. Saves about 2.4 GB if you aren't going to translate.

.PARAMETER WithVoice
    Also set up the synthetic voice (speaking your translation through a
    virtual microphone). It is opt-in because it isn't cheap: a second Python
    environment with its own torch (~7 GB) plus the voice models (~4 GB). The
    reason for the second environment is in sidecar/requirements-tts.txt: the
    ASR requires transformers>=5.13 and chatterbox-tts is tested with 4.57.x.

.PARAMETER SkipBuild
    Don't build the application. Provisions Python and the models only.

.PARAMETER SkipVerify
    Skip the final check. Not recommended: it is the only thing that tells
    "installed" from "installed and working".

.PARAMETER InstallPython
    If there is no valid Python, install it with winget.

.PARAMETER Force
    Rebuild the virtual environment even if it already exists.

.EXAMPLE
    .\scripts\install.ps1

.EXAMPLE
    .\scripts\install.ps1 -ModelsDir D:\models -SkipTranslator

.EXAMPLE
    .\scripts\install.ps1 -WithVoice
#>
#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$ModelsDir,
    [switch]$SkipTranslator,
    [switch]$WithVoice,
    [switch]$SkipBuild,
    [switch]$SkipVerify,
    [switch]$InstallPython,
    [switch]$Force
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

# Versiones de Python con ruedas de torch disponibles. 3.14 todavia no las
# tiene, asi que no vale aunque este instalado.
#
# Con -WithVoice se cae ademas la 3.13: kokoro y misaki declaran
# `Requires-Python: <3.13`, asi que pip los rechaza. Descartarla AQUI, antes de
# empezar, en vez de dejar que reviente en el paso 7 despues de ~20 minutos y
# varios GB descargados.
$SupportedPython = @("3.12", "3.13", "3.11", "3.10")
if ($WithVoice) { $SupportedPython = @("3.12", "3.11", "3.10") }
$TorchIndex = "https://download.pytorch.org/whl/cu128"
$NeedGB = 15
# La voz trae otro venv con su propio torch (~7 GB) y ~4 GB de modelos.
if ($WithVoice) { $NeedGB = 27 }

$Root = Split-Path -Parent $PSScriptRoot

# El script viaja DOS veces: en el repositorio clonado y dentro del instalador,
# como recurso del bundle. Son situaciones distintas y hay que distinguirlas.
#
# Desde el repositorio hay codigo, y los entornos van al lado. Desde una
# aplicacion ya instalada no hay codigo que compilar, y $Root apunta a la
# carpeta de recursos: con el MSI eso es Program Files, donde un usuario sin
# permisos de administrador no puede escribir. Los entornos van entonces al
# perfil del usuario, que siempre es suyo.
$FromSource = Test-Path (Join-Path $Root "Cargo.toml")
if ($FromSource) {
    $DataRoot = $Root
} else {
    $DataRoot = Join-Path $env:LOCALAPPDATA "LiveTranscriber"
    New-Item -ItemType Directory -Force -Path $DataRoot | Out-Null
    # No hay nada que compilar sin codigo, y el usuario ya tiene el .exe.
    $SkipBuild = $true
}

$Venv = Join-Path $DataRoot ".venv"
$VenvPython = Join-Path $Venv "Scripts\python.exe"
$VenvTts = Join-Path $DataRoot ".venv-tts"
$VenvTtsPython = Join-Path $VenvTts "Scripts\python.exe"

$script:Step = 0
$script:Warnings = @()

function Write-Step($text) {
    $script:Step++
    Write-Host ""
    Write-Host "[$($script:Step)] $text" -ForegroundColor Cyan
}
function Write-Ok($text) { Write-Host "    OK  $text" -ForegroundColor Green }
function Write-Info($text) { Write-Host "        $text" -ForegroundColor DarkGray }
function Write-Warn2($text) {
    Write-Host "    !   $text" -ForegroundColor Yellow
    $script:Warnings += $text
}
function Fail($text) {
    Write-Host ""
    Write-Host "  FAILED: $text" -ForegroundColor Red
    exit 1
}

<#
Ejecuta un programa externo sin que su stderr tumbe el script.

Con $ErrorActionPreference = "Stop", PowerShell 5.1 convierte cada linea que un
.exe escriba en stderr en un error TERMINANTE, y `2>$null` no lo evita: solo
esconde el texto. Es decir, un simple aviso de pip abortaba la instalacion, y
una sonda tan inocente como "esta torch instalado?" reventaba en cuanto Python
soltaba su traceback.

Asi que las llamadas nativas pasan por aqui: se baja la preferencia a Continue
solo durante la llamada y se decide con el codigo de salida, que es el dato de
verdad. `Quiet` descarta stderr, para sondas cuyo fallo es esperable.
#>
function Invoke-Native {
    param(
        [Parameter(Mandatory)][string]$Exe,
        [string[]]$Arguments = @(),
        [switch]$Quiet
    )
    $previous = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        if ($Quiet) {
            $out = & $Exe @Arguments 2>$null
        } else {
            $out = & $Exe @Arguments
        }
        return [pscustomobject]@{
            Output = (@($out) -join "`n")
            Code   = $LASTEXITCODE
        }
    } finally {
        $ErrorActionPreference = $previous
    }
}

# Igual pero mostrando la salida en directo, para pip y npm.
function Invoke-NativeLive {
    param(
        [Parameter(Mandatory)][string]$Exe,
        [string[]]$Arguments = @()
    )
    $previous = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        & $Exe @Arguments | Out-Host
        return $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previous
    }
}

Write-Host ""
Write-Host "  LiveTranscriber - installation" -ForegroundColor White
if ($FromSource) {
    Write-Host "  $Root" -ForegroundColor DarkGray
} else {
    Write-Host "  provisioning the installed app" -ForegroundColor DarkGray
    Write-Host "  environments and models go to $DataRoot" -ForegroundColor DarkGray
}

# ---------------------------------------------------------------------------
Write-Step "Preflight checks"

if (-not [Environment]::Is64BitOperatingSystem) {
    Fail "64-bit Windows is required"
}
Write-Ok "64-bit Windows"

# La unidad que importa es la de los ENTORNOS, no la del script: instalado, el
# script esta en Program Files y los entornos en el perfil del usuario. Y si
# -ModelsDir manda la cache a otra unidad, esa tambien cuenta.
$drive = (Get-Item $DataRoot).PSDrive.Name
$free = (Get-PSDrive $drive).Free / 1GB
if ($free -lt $NeedGB) {
    Fail ("on {0}: {1:N1} GB left and about {2} GB are needed (Python environment + models)" -f $drive, $free, $NeedGB)
}
Write-Ok ("space on {0}: {1:N1} GB free" -f $drive, $free)

if ($ModelsDir) {
    $modelsParent = if (Test-Path $ModelsDir) { $ModelsDir } else { Split-Path -Parent $ModelsDir }
    if ($modelsParent -and (Test-Path $modelsParent)) {
        $mDrive = (Get-Item $modelsParent).PSDrive.Name
        if ($mDrive -ne $drive) {
            $mFree = (Get-PSDrive $mDrive).Free / 1GB
            # Los modelos son la mayor parte: ~7 GB, ~11 con la voz.
            $modelsGB = if ($WithVoice) { 11 } else { 7 }
            if ($mFree -lt $modelsGB) {
                Fail ("on {0} (-ModelsDir): {1:N1} GB left and about {2} GB of models are going there" -f $mDrive, $mFree, $modelsGB)
            }
            Write-Ok ("space on {0} for the models: {1:N1} GB free" -f $mDrive, $mFree)
        }
    }
}

# La GPU: sin ella el modelo va a CPU y no da tiempo real, asi que se avisa
# fuerte pero no se aborta.
$dtype = "bfloat16"
$gpuName = $null
if (Get-Command nvidia-smi -ErrorAction SilentlyContinue) {
    $smi = Invoke-Native "nvidia-smi" @("--query-gpu=name,driver_version,memory.total,compute_cap", "--format=csv,noheader") -Quiet
    $gpus = @($smi.Output -split "`n" | Where-Object { $_ -and $_.Trim() })
    if ($gpus.Count -eq 0) {
        Write-Warn2 "nvidia-smi returns no GPU"
    }
    $i = 0
    foreach ($line in $gpus) {
        $f = $line -split ",\s*"
        Write-Ok ("GPU {0}: {1} - driver {2} - {3} - capability {4}" -f $i, $f[0], $f[1], $f[2], $f[3])
        if ($i -eq 0) {
            $gpuName = $f[0]
            $major = [int]($f[3] -split "\.")[0]
            if ($major -lt 8) {
                # bfloat16 necesita Ampere. En Turing PyTorch lo EMULA en vez de
                # fallar, y la app va lentisima sin ninguna pista del motivo.
                $dtype = "float16"
                Write-Warn2 "$($f[0]) has no native bfloat16 (capability 8.0+ is required). Setting float16."
                Write-Info "float16 transcribes slightly worse than bfloat16 on this model. With float32 there is no loss, but it is slower."
            }
        }
        $i++
    }
    if ($gpus.Count -gt 1) {
        Write-Info "There are $($gpus.Count) GPUs but the app uses only the first. One alone handles about 4 streams."
    }
} else {
    Write-Warn2 "nvidia-smi not found: without an NVIDIA GPU this won't keep up in real time"
}

# ---------------------------------------------------------------------------
Write-Step "Python interpreter"

function Find-Python {
    if (Get-Command py -ErrorAction SilentlyContinue) {
        foreach ($want in $SupportedPython) {
            $probe = Invoke-Native "py" @("-$want", "-c", "import sys; print(sys.executable)") -Quiet
            if ($probe.Code -eq 0 -and $probe.Output.Trim()) { return $probe.Output.Trim() }
        }
    }
    # Sin py launcher: mirar el python del PATH y comprobar su version.
    if (Get-Command python -ErrorAction SilentlyContinue) {
        $probe = Invoke-Native "python" @("-c", "import sys; print('%d.%d' % sys.version_info[:2])") -Quiet
        if ($probe.Code -eq 0 -and $SupportedPython -contains $probe.Output.Trim()) {
            $where = Invoke-Native "python" @("-c", "import sys; print(sys.executable)") -Quiet
            if ($where.Code -eq 0) { return $where.Output.Trim() }
        }
    }
    return $null
}

$python = Find-Python
if (-not $python) {
    if ($InstallPython) {
        if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
            Fail "no winget to install Python with. Get it from https://www.python.org/downloads/"
        }
        Write-Info "installing Python 3.12 with winget..."
        Invoke-NativeLive "winget" @(
            "install", "-e", "--id", "Python.Python.3.12",
            "--accept-source-agreements", "--accept-package-agreements"
        ) | Out-Null
        # winget no refresca el PATH de esta sesion.
        $env:Path = [Environment]::GetEnvironmentVariable("Path", "Machine") + ";" +
                    [Environment]::GetEnvironmentVariable("Path", "User")
        $python = Find-Python
        if (-not $python) {
            Fail "Python installed but not locatable. Close this console, open another one and run again."
        }
    } else {
        Write-Host ""
        Write-Host "  None of these Pythons is here: $($SupportedPython -join ', ')" -ForegroundColor Red
        Write-Host "  (3.14 won't do yet: PyTorch doesn't publish wheels for it)" -ForegroundColor DarkGray
        if ($WithVoice) {
            Write-Host "  (and 3.13 is out with -WithVoice: kokoro and misaki require <3.13)" -ForegroundColor DarkGray
        }
        Write-Host ""
        Write-Host "  Install it with:  winget install -e --id Python.Python.3.12"
        Write-Host "  Or run again:     .\scripts\install.ps1 -InstallPython"
        exit 1
    }
}
$pyVersion = (Invoke-Native $python @("-c", "import sys; print('%d.%d.%d' % sys.version_info[:3])")).Output.Trim()
Write-Ok "Python $pyVersion"
Write-Info $python

# ---------------------------------------------------------------------------
Write-Step "Virtual environment"

if ((Test-Path $VenvPython) -and -not $Force) {
    Write-Ok "already there, reused"
    Write-Info "use -Force to rebuild it from scratch"
} else {
    if (Test-Path $Venv) {
        Write-Info "deleting the previous one..."
        Remove-Item $Venv -Recurse -Force
    }
    if ((Invoke-NativeLive $python @("-m", "venv", $Venv)) -ne 0) {
        Fail "could not create the virtual environment"
    }
    Write-Ok "created in .venv"
}

if ((Invoke-NativeLive $VenvPython @("-m", "pip", "install", "--quiet", "--upgrade", "pip", "setuptools", "wheel")) -ne 0) {
    Fail "could not upgrade pip"
}
Write-Ok "pip up to date"

# ---------------------------------------------------------------------------
Write-Step "PyTorch with CUDA"

$probe = Invoke-Native $VenvPython @("-c", "import torch; print(torch.__version__)") -Quiet
if ($probe.Code -eq 0 -and -not $Force) {
    Write-Ok "already installed: $($probe.Output.Trim())"
} else {
    Write-Info "downloading (~2.8 GB, takes a while)..."
    if ((Invoke-NativeLive $VenvPython @("-m", "pip", "install", "torch", "--index-url", $TorchIndex)) -ne 0) {
        Fail "could not install torch"
    }
    Write-Ok "installed"
}

# ---------------------------------------------------------------------------
Write-Step "Sidecar dependencies"

$req = Join-Path $Root "sidecar\requirements.txt"
if (-not (Test-Path $req)) { Fail "can't find $req" }
if ((Invoke-NativeLive $VenvPython @("-m", "pip", "install", "--quiet", "-r", $req)) -ne 0) {
    Fail "could not install the dependencies"
}
$tv = (Invoke-Native $VenvPython @("-c", "import transformers; print(transformers.__version__)")).Output.Trim()
Write-Ok "transformers $tv"
if ([version]($tv -replace '[^\d.].*$','') -lt [version]"5.13") {
    Fail "transformers 5.13 or newer is required: that is where AutoModelForRNNT lives"
}

# ---------------------------------------------------------------------------
Write-Step "GPU compatibility"

# Que la rueda traiga codigo para esta tarjeta no es evidente: cu128 empieza en
# sm_75, asi que una GTX 10xx (sm_61) no valdria. Va despues de instalar numpy
# porque si no torch avisa de que no lo encuentra y ensucia la salida.
$archScript = @"
import torch, sys
print('CUDA available:', torch.cuda.is_available())
print('architectures:', ' '.join(torch.cuda.get_arch_list()))
if torch.cuda.is_available():
    c = torch.cuda.get_device_capability()
    tag = 'sm_%d%d' % c
    print('this GPU:', tag)
    if tag not in torch.cuda.get_arch_list():
        sys.exit(3)
"@
$arch = Invoke-Native $VenvPython @("-c", $archScript)
if ($arch.Code -eq 3) {
    Fail "this PyTorch wheel has no code for your GPU. A different CUDA version will be needed."
}
if ($arch.Code -ne 0) { Fail "torch cannot be imported" }
foreach ($line in ($arch.Output -split "`n")) {
    if ($line.Trim()) { Write-Info $line.Trim() }
}

# ---------------------------------------------------------------------------
Write-Step "Models"

if ($ModelsDir) {
    New-Item -ItemType Directory -Force -Path $ModelsDir | Out-Null
    $ModelsDir = (Resolve-Path $ModelsDir).Path
    $env:HF_HOME = $ModelsDir
    Write-Info "HF_HOME = $ModelsDir"
}

$fetchArgs = @((Join-Path $PSScriptRoot "fetch_models.py"))
if ($SkipTranslator) { $fetchArgs += "--skip-translator" }
if ((Invoke-NativeLive $VenvPython $fetchArgs) -ne 0) {
    Fail "could not download the models"
}
Write-Ok "models ready"

# ---------------------------------------------------------------------------
if ($WithVoice) {
    Write-Step "Voice environment (chatterbox + kokoro)"

    # Un venv APARTE del de los otros sidecars, y no es capricho: asr_server
    # exige transformers>=5.13 (AutoModelForRNNT) y chatterbox-tts esta
    # probado con 4.57.x. Los dos conjuntos no caben en el mismo entorno.
    if ((Test-Path $VenvTtsPython) -and -not $Force) {
        Write-Ok "already there, reused"
        Write-Info "use -Force to rebuild it from scratch"
    } else {
        if (Test-Path $VenvTts) {
            Write-Info "deleting the previous one..."
            Remove-Item $VenvTts -Recurse -Force
        }
        if ((Invoke-NativeLive $python @("-m", "venv", $VenvTts)) -ne 0) {
            Fail "could not create the voice environment"
        }
        Write-Ok "created in .venv-tts"
    }

    if ((Invoke-NativeLive $VenvTtsPython @("-m", "pip", "install", "--quiet", "--upgrade", "pip", "setuptools", "wheel")) -ne 0) {
        Fail "could not upgrade pip in the voice environment"
    }

    $probe = Invoke-Native $VenvTtsPython @("-c", "import torch; print(torch.__version__)") -Quiet
    if ($probe.Code -eq 0 -and -not $Force) {
        Write-Ok "torch already installed: $($probe.Output.Trim())"
    } else {
        Write-Info "downloading torch for the voice (~2.8 GB, takes a while)..."
        if ((Invoke-NativeLive $VenvTtsPython @("-m", "pip", "install", "torch", "torchaudio", "--index-url", $TorchIndex)) -ne 0) {
            Fail "could not install torch in the voice environment"
        }
        Write-Ok "torch installed"
    }

    $reqTts = Join-Path $Root "sidecar\requirements-tts.txt"
    if (-not (Test-Path $reqTts)) { Fail "can't find $reqTts" }
    if ((Invoke-NativeLive $VenvTtsPython @("-m", "pip", "install", "--quiet", "-r", $reqTts)) -ne 0) {
        Fail "could not install the voice dependencies"
    }
    # --no-deps A PROPOSITO: chatterbox-tts pina torch==2.6 y transformers
    # exactos que aqui no valen; sin esto, pip reinstalaria torch sin CUDA.
    # Sus dependencias reales ya vienen de requirements-tts.txt.
    if ((Invoke-NativeLive $VenvTtsPython @("-m", "pip", "install", "--quiet", "--no-deps", "chatterbox-tts")) -ne 0) {
        Fail "could not install chatterbox-tts"
    }

    # La sonda de verdad: importar los dos motores. Instalar en limpio es
    # justo donde aparecen las dependencias que faltan en la lista.
    $engines = Invoke-Native $VenvTtsPython @("-c", "import chatterbox.mtl_tts, kokoro; print('ok')") -Quiet
    if ($engines.Code -ne 0) {
        Fail "the voice engines cannot be imported; look at: $VenvTtsPython -c `"import chatterbox.mtl_tts, kokoro`""
    }
    Write-Ok "engines import fine (chatterbox, kokoro)"

    # Los pesos multilingues de chatterbox y kokoro entero. Solo los ficheros
    # que se usan: el repo de chatterbox trae ademas los pesos solo-ingles,
    # que no tocamos. Respeta HF_HOME si se movio la cache con -ModelsDir.
    Write-Info "downloading the voice models (~4 GB the first time)..."
    $dlScript = @"
from huggingface_hub import snapshot_download
snapshot_download('ResembleAI/chatterbox', allow_patterns=[
    '*.json', '*.txt', 'conds.pt', 't3_mtl23ls_v2.safetensors', 's3gen.pt', 've.pt',
])
snapshot_download('hexgrad/Kokoro-82M')
print('voice models cached')
"@
    if ((Invoke-NativeLive $VenvTtsPython @("-c", $dlScript)) -ne 0) {
        Fail "could not download the voice models"
    }
    Write-Ok "voice models ready"
    Write-Info "the voice is turned on in the app (the 'Speak for me' section); for"
    Write-Info "the virtual microphone you also need VB-CABLE: https://vb-audio.com/Cable/"
}

# ---------------------------------------------------------------------------
Write-Step "Configuration"

# La ubicacion canonica, la misma que usa la aplicacion (asr_core::config_location).
# Antes se escribia en la raiz del clon, y la aplicacion instalada —que vive en
# %LOCALAPPDATA%— no miraba ahi nunca: provisionar y ejecutar no se encontraban.
if ($env:LIVETRANSCRIBER_CONFIG) {
    $configPath = $env:LIVETRANSCRIBER_CONFIG
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $configPath) | Out-Null
} else {
    $configDir = Join-Path $env:APPDATA "LiveTranscriber"
    New-Item -ItemType Directory -Force -Path $configDir | Out-Null
    $configPath = Join-Path $configDir "transcriber-config.toml"
}
$outDir = Join-Path $env:USERPROFILE "Documents\LiveTranscriber"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

$lines = @(
    "# Generated by scripts\install.ps1. Most keys can be changed from the app;",
    "# the interpreter paths below can only be changed here, or by running the",
    "# installer again.",
    "python = '$VenvPython'",
    'script = "sidecar/asr_server.py"',
    'mt_script = "sidecar/mt_server.py"'
)
if ($ModelsDir) { $lines += "hf_home = '$ModelsDir'" }
$lines += @(
    "",
    '# float16 on cards older than Ampere; bfloat16 on Ampere or newer.',
    "dtype = `"$dtype`"",
    "# Language of the ROOM: what plays through the system. With translation",
    "# it can NOT be auto; it is picked in the app.",
    'language = "auto"',
    "lookahead = 3",
    "",
    "translate = false",
    "# What the ROOM (what plays through the system) is translated into: the",
    "# language you read in. The microphone, unless you pick otherwise, is",
    "# the mirror: you speak in this language and get translated into the",
    # Backtick doblado: en cadena de comillas dobles es el caracter de escape,
    # asi que uno solo se lo comia PowerShell y el TOML salia sin ellos.
    "# room's (``language``).",
    'target_language = "es-ES"',
    "",
    "capture_system = true",
    "capture_mic = false",
    "",
    "gate_drop_db = 25.0",
    "gate_floor_dbfs = -80.0",
    "gate_hold_secs = 2.0",
    "paragraph_idle_secs = 1.2",
    "paragraph_max_secs = 30.0",
    "normalize_gain = true",
    "",
    'hotkey_toggle = "CmdOrControl+Shift+T"',
    'hotkey_overlay = "CmdOrControl+Shift+O"',
    "overlay_enabled = false",
    "",
    "output_dir = '$outDir'",
    'output_name = "transcript"'
)

# La tabla [speak] va la ultima: en TOML no puede haber claves de raiz
# despues de una tabla.
if ($WithVoice) {
    $lines += @(
        "",
        "# Synthetic voice: speak your translation through a virtual microphone.",
        "# It is turned on from the app (the 'Speak for me' section), where you",
        "# also pick the WAV with your voice; without it, chatterbox won't start.",
        "[speak]",
        "enabled = false",
        'engine = "chatterbox"',
        "python = '$VenvTtsPython'",
        'script = "sidecar/tts_server.py"'
    )
}

if ((Test-Path $configPath) -and -not $Force) {
    Write-Ok "already there, left alone"
    Write-Info "use -Force to regenerate it with the detected values"
    if ($WithVoice) {
        Write-Info "the voice interpreter for the app is: $VenvTtsPython"
    }
} else {
    $lines | Out-File -Encoding utf8 $configPath
    Write-Ok "wrote transcriber-config.toml"
    Write-Info "dtype = $dtype"
    Write-Info "output_dir = $outDir"
}

# ---------------------------------------------------------------------------
Write-Step "Application"

if ($SkipBuild) {
    Write-Ok "skipped (-SkipBuild)"
} else {
    $missing = @()
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { $missing += "Rust (https://rustup.rs)" }
    if (-not (Get-Command npm -ErrorAction SilentlyContinue)) { $missing += "Node.js (https://nodejs.org)" }
    if ($missing.Count -gt 0) {
        Write-Warn2 "can't build, missing: $($missing -join ', ')"
        Write-Info "the Python environment and the models are ready; install that and run again"
    } else {
        Push-Location $Root
        try {
            Write-Info "npm install..."
            if ((Invoke-NativeLive "npm" @("install", "--silent")) -ne 0) { Fail "npm install failed" }

            Write-Info "building the application (the first time takes several minutes)..."
            if ((Invoke-NativeLive "npm" @("run", "app:build")) -ne 0) {
                Fail "the application build failed"
            }

            # Es un workspace de Cargo, asi que `target` esta en la raiz y no
            # dentro de src-tauri.
            $bundle = Join-Path $Root "target\release\bundle"
            $installers = @(Get-ChildItem $bundle -Recurse -Include *.msi, *.exe -ErrorAction SilentlyContinue)
            if ($installers.Count -gt 0) {
                Write-Ok "installers generated:"
                foreach ($f in $installers) { Write-Info $f.FullName }
            } else {
                Write-Warn2 "it built but I can't find the installer in $bundle"
            }
        } finally {
            Pop-Location
        }
    }
}

# ---------------------------------------------------------------------------
if (-not $SkipVerify) {
    Write-Step "Verification"
    $verify = Join-Path $PSScriptRoot "verify.ps1"
    if (Test-Path $verify) {
        & $verify -Root $Root
        if ($LASTEXITCODE -ne 0) { Fail "verification did not pass. Look at the messages above." }
    } else {
        Write-Warn2 "can't find verify.ps1"
    }
}

# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "  Installation finished" -ForegroundColor Green
if ($script:Warnings.Count -gt 0) {
    Write-Host ""
    Write-Host "  Warnings:" -ForegroundColor Yellow
    foreach ($w in $script:Warnings) { Write-Host "   - $w" -ForegroundColor Yellow }
}
Write-Host ""
Write-Host "  Start in development:  npm run app:dev"
Write-Host "  Test without the UI:   cargo run -p asr-cli -- devices"
if ($WithVoice) {
    Write-Host "  Test the voice:        cargo run -p asr-cli -- speak --engine kokoro --lang es --text `"hola`" --python .venv-tts\Scripts\python.exe"
}
Write-Host ""
Write-Host "  Turn the Windows volume up before testing: the loopback captures" -ForegroundColor DarkGray
Write-Host "  after the volume control." -ForegroundColor DarkGray
Write-Host ""
