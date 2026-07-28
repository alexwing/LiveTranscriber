<#
.SYNOPSIS
    Deja LiveTranscriber listo para funcionar en un Windows recien puesto.

.DESCRIPTION
    Provisiona el entorno de Python, descarga los modelos, escribe la
    configuracion con las rutas absolutas correctas y comprueba que todo
    arranca de verdad. Se puede volver a ejecutar sin romper nada.

    Lo que NO hace: instalar el driver de NVIDIA. Eso va antes y a mano.

.PARAMETER ModelsDir
    Donde guardar los modelos (~7 GB). Por defecto la cache de Hugging Face en
    el perfil del usuario. Util si el disco C: va justo.

.PARAMETER SkipTranslator
    No descargar NLLB. Ahorra unos 2,4 GB si no vas a traducir.

.PARAMETER WithVoice
    Montar tambien la voz sintetica (hablar tu traduccion por un microfono
    virtual). Es opt-in porque no es barata: un segundo entorno de Python con
    su propio torch (~7 GB) mas los modelos de voz (~4 GB). El porque del
    segundo entorno esta en sidecar/requirements-tts.txt: el ASR exige
    transformers>=5.13 y chatterbox-tts esta probado con 4.57.x.

.PARAMETER SkipBuild
    No compilar la aplicacion. Solo provisiona Python y los modelos.

.PARAMETER SkipVerify
    Saltarse la comprobacion final. No recomendado: es lo unico que distingue
    "instalado" de "instalado y funcionando".

.PARAMETER InstallPython
    Si no hay un Python valido, instalarlo con winget.

.PARAMETER Force
    Rehacer el entorno virtual aunque ya exista.

.EXAMPLE
    .\scripts\install.ps1

.EXAMPLE
    .\scripts\install.ps1 -ModelsDir D:\modelos -SkipTranslator

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
$SupportedPython = @("3.12", "3.13", "3.11", "3.10")
$TorchIndex = "https://download.pytorch.org/whl/cu128"
$NeedGB = 15
# La voz trae otro venv con su propio torch (~7 GB) y ~4 GB de modelos.
if ($WithVoice) { $NeedGB = 27 }

$Root = Split-Path -Parent $PSScriptRoot
$Venv = Join-Path $Root ".venv"
$VenvPython = Join-Path $Venv "Scripts\python.exe"
$VenvTts = Join-Path $Root ".venv-tts"
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
    Write-Host "  FALLO: $text" -ForegroundColor Red
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
Write-Host "  LiveTranscriber - instalacion" -ForegroundColor White
Write-Host "  $Root" -ForegroundColor DarkGray

# ---------------------------------------------------------------------------
Write-Step "Comprobaciones previas"

if (-not [Environment]::Is64BitOperatingSystem) {
    Fail "hace falta Windows de 64 bits"
}
Write-Ok "Windows 64 bits"

$drive = (Get-Item $Root).PSDrive.Name
$free = (Get-PSDrive $drive).Free / 1GB
if ($free -lt $NeedGB) {
    Fail ("en {0}: quedan {1:N1} GB y hacen falta unos {2} GB (entorno de Python + modelos)" -f $drive, $free, $NeedGB)
}
Write-Ok ("espacio en {0}: {1:N1} GB libres" -f $drive, $free)

# La GPU: sin ella el modelo va a CPU y no da tiempo real, asi que se avisa
# fuerte pero no se aborta.
$dtype = "bfloat16"
$gpuName = $null
if (Get-Command nvidia-smi -ErrorAction SilentlyContinue) {
    $smi = Invoke-Native "nvidia-smi" @("--query-gpu=name,driver_version,memory.total,compute_cap", "--format=csv,noheader") -Quiet
    $gpus = @($smi.Output -split "`n" | Where-Object { $_ -and $_.Trim() })
    if ($gpus.Count -eq 0) {
        Write-Warn2 "nvidia-smi no devuelve ninguna GPU"
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
                Write-Warn2 "$($f[0]) no tiene bfloat16 nativo (hace falta capability 8.0+). Se configura float16."
                Write-Info "float16 transcribe algo peor que bfloat16 en este modelo. Con float32 no hay perdida, pero es mas lento."
            }
        }
        $i++
    }
    if ($gpus.Count -gt 1) {
        Write-Info "Hay $($gpus.Count) GPU pero la app usa solo la primera. Una sola aguanta unos 4 flujos."
    }
} else {
    Write-Warn2 "no se encuentra nvidia-smi: sin GPU NVIDIA esto no va a dar tiempo real"
}

# ---------------------------------------------------------------------------
Write-Step "Interprete de Python"

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
            Fail "no hay winget para instalar Python. Bajalo de https://www.python.org/downloads/"
        }
        Write-Info "instalando Python 3.12 con winget..."
        Invoke-NativeLive "winget" @(
            "install", "-e", "--id", "Python.Python.3.12",
            "--accept-source-agreements", "--accept-package-agreements"
        ) | Out-Null
        # winget no refresca el PATH de esta sesion.
        $env:Path = [Environment]::GetEnvironmentVariable("Path", "Machine") + ";" +
                    [Environment]::GetEnvironmentVariable("Path", "User")
        $python = Find-Python
        if (-not $python) {
            Fail "Python instalado pero no localizable. Cierra esta consola, abre otra y vuelve a ejecutar."
        }
    } else {
        Write-Host ""
        Write-Host "  No hay ningun Python de estos: $($SupportedPython -join ', ')" -ForegroundColor Red
        Write-Host "  (3.14 no sirve todavia: PyTorch no publica ruedas para el)" -ForegroundColor DarkGray
        Write-Host ""
        Write-Host "  Instalalo con:      winget install -e --id Python.Python.3.12"
        Write-Host "  O vuelve a lanzar:  .\scripts\install.ps1 -InstallPython"
        exit 1
    }
}
$pyVersion = (Invoke-Native $python @("-c", "import sys; print('%d.%d.%d' % sys.version_info[:3])")).Output.Trim()
Write-Ok "Python $pyVersion"
Write-Info $python

# ---------------------------------------------------------------------------
Write-Step "Entorno virtual"

if ((Test-Path $VenvPython) -and -not $Force) {
    Write-Ok "ya existe, se reutiliza"
    Write-Info "usa -Force para rehacerlo desde cero"
} else {
    if (Test-Path $Venv) {
        Write-Info "borrando el anterior..."
        Remove-Item $Venv -Recurse -Force
    }
    if ((Invoke-NativeLive $python @("-m", "venv", $Venv)) -ne 0) {
        Fail "no se pudo crear el entorno virtual"
    }
    Write-Ok "creado en .venv"
}

if ((Invoke-NativeLive $VenvPython @("-m", "pip", "install", "--quiet", "--upgrade", "pip", "setuptools", "wheel")) -ne 0) {
    Fail "no se pudo actualizar pip"
}
Write-Ok "pip al dia"

# ---------------------------------------------------------------------------
Write-Step "PyTorch con CUDA"

$probe = Invoke-Native $VenvPython @("-c", "import torch; print(torch.__version__)") -Quiet
if ($probe.Code -eq 0 -and -not $Force) {
    Write-Ok "ya instalado: $($probe.Output.Trim())"
} else {
    Write-Info "descargando (~2,8 GB, tarda un rato)..."
    if ((Invoke-NativeLive $VenvPython @("-m", "pip", "install", "torch", "--index-url", $TorchIndex)) -ne 0) {
        Fail "no se pudo instalar torch"
    }
    Write-Ok "instalado"
}

# ---------------------------------------------------------------------------
Write-Step "Dependencias de los sidecars"

$req = Join-Path $Root "sidecar\requirements.txt"
if (-not (Test-Path $req)) { Fail "no encuentro $req" }
if ((Invoke-NativeLive $VenvPython @("-m", "pip", "install", "--quiet", "-r", $req)) -ne 0) {
    Fail "no se pudieron instalar las dependencias"
}
$tv = (Invoke-Native $VenvPython @("-c", "import transformers; print(transformers.__version__)")).Output.Trim()
Write-Ok "transformers $tv"
if ([version]($tv -replace '[^\d.].*$','') -lt [version]"5.13") {
    Fail "hace falta transformers 5.13 o superior: es donde vive AutoModelForRNNT"
}

# ---------------------------------------------------------------------------
Write-Step "Compatibilidad de la GPU"

# Que la rueda traiga codigo para esta tarjeta no es evidente: cu128 empieza en
# sm_75, asi que una GTX 10xx (sm_61) no valdria. Va despues de instalar numpy
# porque si no torch avisa de que no lo encuentra y ensucia la salida.
$archScript = @"
import torch, sys
print('CUDA disponible:', torch.cuda.is_available())
print('arquitecturas:', ' '.join(torch.cuda.get_arch_list()))
if torch.cuda.is_available():
    c = torch.cuda.get_device_capability()
    tag = 'sm_%d%d' % c
    print('esta GPU:', tag)
    if tag not in torch.cuda.get_arch_list():
        sys.exit(3)
"@
$arch = Invoke-Native $VenvPython @("-c", $archScript)
if ($arch.Code -eq 3) {
    Fail "esta rueda de PyTorch no incluye codigo para tu GPU. Hara falta otra version de CUDA."
}
if ($arch.Code -ne 0) { Fail "torch no se puede importar" }
foreach ($line in ($arch.Output -split "`n")) {
    if ($line.Trim()) { Write-Info $line.Trim() }
}

# ---------------------------------------------------------------------------
Write-Step "Modelos"

if ($ModelsDir) {
    New-Item -ItemType Directory -Force -Path $ModelsDir | Out-Null
    $ModelsDir = (Resolve-Path $ModelsDir).Path
    $env:HF_HOME = $ModelsDir
    Write-Info "HF_HOME = $ModelsDir"
}

$fetchArgs = @((Join-Path $PSScriptRoot "fetch_models.py"))
if ($SkipTranslator) { $fetchArgs += "--skip-translator" }
if ((Invoke-NativeLive $VenvPython $fetchArgs) -ne 0) {
    Fail "no se pudieron descargar los modelos"
}
Write-Ok "modelos listos"

# ---------------------------------------------------------------------------
if ($WithVoice) {
    Write-Step "Entorno de voz (chatterbox + kokoro)"

    # Un venv APARTE del de los otros sidecars, y no es capricho: asr_server
    # exige transformers>=5.13 (AutoModelForRNNT) y chatterbox-tts esta
    # probado con 4.57.x. Los dos conjuntos no caben en el mismo entorno.
    if ((Test-Path $VenvTtsPython) -and -not $Force) {
        Write-Ok "ya existe, se reutiliza"
        Write-Info "usa -Force para rehacerlo desde cero"
    } else {
        if (Test-Path $VenvTts) {
            Write-Info "borrando el anterior..."
            Remove-Item $VenvTts -Recurse -Force
        }
        if ((Invoke-NativeLive $python @("-m", "venv", $VenvTts)) -ne 0) {
            Fail "no se pudo crear el entorno de voz"
        }
        Write-Ok "creado en .venv-tts"
    }

    if ((Invoke-NativeLive $VenvTtsPython @("-m", "pip", "install", "--quiet", "--upgrade", "pip", "setuptools", "wheel")) -ne 0) {
        Fail "no se pudo actualizar pip en el entorno de voz"
    }

    $probe = Invoke-Native $VenvTtsPython @("-c", "import torch; print(torch.__version__)") -Quiet
    if ($probe.Code -eq 0 -and -not $Force) {
        Write-Ok "torch ya instalado: $($probe.Output.Trim())"
    } else {
        Write-Info "descargando torch para la voz (~2,8 GB, tarda un rato)..."
        if ((Invoke-NativeLive $VenvTtsPython @("-m", "pip", "install", "torch", "torchaudio", "--index-url", $TorchIndex)) -ne 0) {
            Fail "no se pudo instalar torch en el entorno de voz"
        }
        Write-Ok "torch instalado"
    }

    $reqTts = Join-Path $Root "sidecar\requirements-tts.txt"
    if (-not (Test-Path $reqTts)) { Fail "no encuentro $reqTts" }
    if ((Invoke-NativeLive $VenvTtsPython @("-m", "pip", "install", "--quiet", "-r", $reqTts)) -ne 0) {
        Fail "no se pudieron instalar las dependencias de voz"
    }
    # --no-deps A PROPOSITO: chatterbox-tts pina torch==2.6 y transformers
    # exactos que aqui no valen; sin esto, pip reinstalaria torch sin CUDA.
    # Sus dependencias reales ya vienen de requirements-tts.txt.
    if ((Invoke-NativeLive $VenvTtsPython @("-m", "pip", "install", "--quiet", "--no-deps", "chatterbox-tts")) -ne 0) {
        Fail "no se pudo instalar chatterbox-tts"
    }

    # La sonda de verdad: importar los dos motores. Instalar en limpio es
    # justo donde aparecen las dependencias que faltan en la lista.
    $engines = Invoke-Native $VenvTtsPython @("-c", "import chatterbox.mtl_tts, kokoro; print('ok')") -Quiet
    if ($engines.Code -ne 0) {
        Fail "los motores de voz no se pueden importar; mira: $VenvTtsPython -c `"import chatterbox.mtl_tts, kokoro`""
    }
    Write-Ok "motores importables (chatterbox, kokoro)"

    # Los pesos multilingues de chatterbox y kokoro entero. Solo los ficheros
    # que se usan: el repo de chatterbox trae ademas los pesos solo-ingles,
    # que no tocamos. Respeta HF_HOME si se movio la cache con -ModelsDir.
    Write-Info "descargando los modelos de voz (~4 GB la primera vez)..."
    $dlScript = @"
from huggingface_hub import snapshot_download
snapshot_download('ResembleAI/chatterbox', allow_patterns=[
    '*.json', '*.txt', 'conds.pt', 't3_mtl23ls_v2.safetensors', 's3gen.pt', 've.pt',
])
snapshot_download('hexgrad/Kokoro-82M')
print('modelos de voz en cache')
"@
    if ((Invoke-NativeLive $VenvTtsPython @("-c", $dlScript)) -ne 0) {
        Fail "no se pudieron descargar los modelos de voz"
    }
    Write-Ok "modelos de voz listos"
    Write-Info "la voz se activa en la app (seccion 'Hablar por mi'); para el"
    Write-Info "microfono virtual hace falta ademas VB-CABLE: https://vb-audio.com/Cable/"
}

# ---------------------------------------------------------------------------
Write-Step "Configuracion"

$configPath = Join-Path $Root "transcriber-config.toml"
$outDir = Join-Path $env:USERPROFILE "Documents\LiveTranscriber"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

$lines = @(
    "# Generado por scripts\install.ps1. Se puede editar a mano o desde la app.",
    "python = '$VenvPython'",
    'script = "sidecar/asr_server.py"',
    'mt_script = "sidecar/mt_server.py"'
)
if ($ModelsDir) { $lines += "hf_home = '$ModelsDir'" }
$lines += @(
    "",
    '# float16 en tarjetas anteriores a Ampere; bfloat16 en Ampere o superior.',
    "dtype = `"$dtype`"",
    'language = "es-ES"',
    "lookahead = 3",
    "",
    "translate = $(if ($SkipTranslator) { 'false' } else { 'false' })",
    'target_language = "en-US"',
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
    'output_name = "transcripcion"'
)

# La tabla [speak] va la ultima: en TOML no puede haber claves de raiz
# despues de una tabla.
if ($WithVoice) {
    $lines += @(
        "",
        "# Voz sintetica: hablar tu traduccion por un microfono virtual. Se",
        "# activa desde la app (seccion 'Hablar por mi'), donde tambien se",
        "# elige el WAV con tu voz; sin el, chatterbox no arranca.",
        "[speak]",
        "enabled = false",
        'engine = "chatterbox"',
        "python = '$VenvTtsPython'",
        'script = "sidecar/tts_server.py"'
    )
}

if ((Test-Path $configPath) -and -not $Force) {
    Write-Ok "ya existe, no se toca"
    Write-Info "usa -Force para regenerarla con los valores detectados"
    if ($WithVoice) {
        Write-Info "el interprete de voz para la app es: $VenvTtsPython"
    }
} else {
    $lines | Out-File -Encoding utf8 $configPath
    Write-Ok "escrita transcriber-config.toml"
    Write-Info "dtype = $dtype"
    Write-Info "output_dir = $outDir"
}

# ---------------------------------------------------------------------------
Write-Step "Aplicacion"

if ($SkipBuild) {
    Write-Ok "omitida (-SkipBuild)"
} else {
    $missing = @()
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { $missing += "Rust (https://rustup.rs)" }
    if (-not (Get-Command npm -ErrorAction SilentlyContinue)) { $missing += "Node.js (https://nodejs.org)" }
    if ($missing.Count -gt 0) {
        Write-Warn2 "no se puede compilar, falta: $($missing -join ', ')"
        Write-Info "el entorno de Python y los modelos ya estan listos; instala eso y vuelve a ejecutar"
    } else {
        Push-Location $Root
        try {
            Write-Info "npm install..."
            if ((Invoke-NativeLive "npm" @("install", "--silent")) -ne 0) { Fail "npm install fallo" }

            Write-Info "compilando la aplicacion (la primera vez son varios minutos)..."
            if ((Invoke-NativeLive "npm" @("run", "app:build")) -ne 0) {
                Fail "la compilacion de la aplicacion fallo"
            }

            # Es un workspace de Cargo, asi que `target` esta en la raiz y no
            # dentro de src-tauri.
            $bundle = Join-Path $Root "target\release\bundle"
            $installers = @(Get-ChildItem $bundle -Recurse -Include *.msi, *.exe -ErrorAction SilentlyContinue)
            if ($installers.Count -gt 0) {
                Write-Ok "instaladores generados:"
                foreach ($f in $installers) { Write-Info $f.FullName }
            } else {
                Write-Warn2 "compilo pero no encuentro el instalador en $bundle"
            }
        } finally {
            Pop-Location
        }
    }
}

# ---------------------------------------------------------------------------
if (-not $SkipVerify) {
    Write-Step "Verificacion"
    $verify = Join-Path $PSScriptRoot "verify.ps1"
    if (Test-Path $verify) {
        & $verify -Root $Root
        if ($LASTEXITCODE -ne 0) { Fail "la verificacion no paso. Mira los mensajes de arriba." }
    } else {
        Write-Warn2 "no encuentro verify.ps1"
    }
}

# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "  Instalacion terminada" -ForegroundColor Green
if ($script:Warnings.Count -gt 0) {
    Write-Host ""
    Write-Host "  Avisos:" -ForegroundColor Yellow
    foreach ($w in $script:Warnings) { Write-Host "   - $w" -ForegroundColor Yellow }
}
Write-Host ""
Write-Host "  Arrancar en desarrollo:  npm run app:dev"
Write-Host "  Probar sin interfaz:     cargo run -p asr-cli -- devices"
if ($WithVoice) {
    Write-Host "  Probar la voz:           cargo run -p asr-cli -- speak --engine kokoro --lang es --text `"hola`" --python .venv-tts\Scripts\python.exe"
}
Write-Host ""
Write-Host "  Sube el volumen de Windows antes de probar: el bucle de retorno" -ForegroundColor DarkGray
Write-Host "  captura despues del control de volumen." -ForegroundColor DarkGray
Write-Host ""
