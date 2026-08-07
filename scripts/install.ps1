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
    [switch]$Force,
    # -Force rehace los entornos virtuales. NO toca la configuracion: para eso
    # esta -ResetConfig, que ademas deja una copia con fecha. Separarlos es el
    # arreglo: antes -Force se llevaba en silencio los ajustes del usuario, y
    # verify.ps1 lo recomendaba como remedio.
    [switch]$ResetConfig
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

# Dentro del bloque de la voz se LANZA en vez de salir: su try lo recoge y el
# resto de la instalacion sigue. Usar Fail ahi mataria una instalacion de ASR
# ya terminada por una funcion opcional.
function Fail-Voice($text) {
    throw $text
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

# --------------------------------------------------------------- TOML
#
# Antes el TOML se componia interpolando en cadenas literales:
# "output_dir = '$outDir'". En TOML la comilla simple delimita una cadena
# literal, asi que un apostrofo dentro la cierra: un usuario de Windows
# llamado O'Brien —nombre perfectamente normal— generaba un fichero invalido.
#
# El escapado ingenuo (pasar a comillas dobles sin tocar nada mas) es PEOR,
# porque falla en silencio: "C:\temp\nuevo\respaldo" parsea sin error y
# devuelve 19 caracteres de 22, con \t \n \r convertidos en tabulador, salto
# de linea y retorno. Por eso, en la rama de comillas dobles, las barras se
# doblan ANTES que nada.

function ConvertTo-TomlString {
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Value)
    # Literal mientras se pueda: es lo que hay hoy en los ficheros y lo que
    # elige tambien toml::to_string_pretty para rutas de Windows, asi que para
    # un nombre de usuario normal el fichero generado no cambia en nada.
    $basic = $false
    if ($Value.Contains("'")) { $basic = $true }
    if (-not $basic) {
        foreach ($ch in $Value.ToCharArray()) {
            $c = [int]$ch
            if ($c -lt 0x20 -or $c -eq 0x7F) { $basic = $true; break }
        }
    }
    if (-not $basic) { return "'" + $Value + "'" }
    $sb = New-Object System.Text.StringBuilder
    [void]$sb.Append('"')
    foreach ($ch in $Value.ToCharArray()) {
        $c = [int]$ch
        if     ($ch -eq '"')  { [void]$sb.Append('\"') }
        elseif ($ch -eq '\')  { [void]$sb.Append('\\') }
        elseif ($c -eq 8)     { [void]$sb.Append('\b') }
        elseif ($c -eq 9)     { [void]$sb.Append('\t') }
        elseif ($c -eq 10)    { [void]$sb.Append('\n') }
        elseif ($c -eq 12)    { [void]$sb.Append('\f') }
        elseif ($c -eq 13)    { [void]$sb.Append('\r') }
        elseif ($c -lt 0x20 -or $c -eq 0x7F) { [void]$sb.AppendFormat('\u{0:X4}', $c) }
        else { [void]$sb.Append($ch) }
    }
    [void]$sb.Append('"')
    return $sb.ToString()
}

<#
Editor TOML por lineas. NO es un parser, y no pretende serlo: el instalador
solo escribe siete claves escalares, y por lineas se conservan intactos los
comentarios y —lo que importa— las 29 claves que son del usuario.

La raiz acaba en la PRIMERA cabecera de tabla: sin eso, una clave de raiz
insertada al final caeria dentro de [speak].
#>
function Get-TomlSection {
    param([string[]]$Lines, [string]$Section)
    if (-not $Section) {
        $end = $Lines.Count
        for ($i = 0; $i -lt $Lines.Count; $i++) {
            if ($Lines[$i] -match '^\s*\[') { $end = $i; break }
        }
        return @{ Start = 0; End = $end; Found = $true }
    }
    $pattern = '^\s*\[' + [regex]::Escape($Section) + '\]\s*$'
    for ($i = 0; $i -lt $Lines.Count; $i++) {
        if ($Lines[$i] -match $pattern) {
            $end = $Lines.Count
            for ($j = $i + 1; $j -lt $Lines.Count; $j++) {
                if ($Lines[$j] -match '^\s*\[') { $end = $j; break }
            }
            return @{ Start = $i + 1; End = $end; Found = $true }
        }
    }
    return @{ Start = $Lines.Count; End = $Lines.Count; Found = $false }
}

# $Rendered ya viene en sintaxis TOML (de ConvertTo-TomlString, o un literal).
function Set-TomlValue {
    param([string[]]$Lines, [string]$Key, [string]$Rendered, [string]$Section)
    $Lines = @($Lines)
    $sec = Get-TomlSection -Lines $Lines -Section $Section
    $keyPattern = '^\s*' + [regex]::Escape($Key) + '\s*='
    for ($i = $sec.Start; $i -lt $sec.End; $i++) {
        if ($Lines[$i] -match $keyPattern) { $Lines[$i] = "$Key = $Rendered"; return $Lines }
    }
    if (-not $sec.Found) {
        $out = @($Lines)
        if ($out.Count -gt 0 -and $out[$out.Count - 1].Trim() -ne "") { $out += "" }
        $out += "[$Section]"
        $out += "$Key = $Rendered"
        return $out
    }
    $at = $sec.End
    while ($at -gt $sec.Start -and $Lines[$at - 1].Trim() -eq "") { $at-- }
    $out = @()
    if ($at -gt 0) { $out += $Lines[0..($at - 1)] }
    $out += "$Key = $Rendered"
    if ($at -lt $Lines.Count) { $out += $Lines[$at..($Lines.Count - 1)] }
    return $out
}

function Write-TomlFile {
    param([string]$Path, [string[]]$Lines)
    # UTF-8 SIN BOM, que es lo que escribe la aplicacion. `Out-File -Encoding
    # utf8` en PS 5.1 mete EF BB BF y deja dos formatos del mismo fichero
    # segun quien lo escribiera el ultimo.
    [System.IO.File]::WriteAllText($Path, (($Lines -join "`r`n") + "`r`n"), (New-Object System.Text.UTF8Encoding $false))
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

# La red. Se descargan ~12 GB de cuatro origenes distintos, y hasta ahora
# ningun mensaje de fallo nombraba la red: un proxy corporativo o un cortafuegos
# se manifestaban como "no se pudo instalar torch", que manda a mirar donde no
# es. Sonda barata, 3 s en total, y solo avisa: un origen puede estar caido y
# el resto valer.
$origins = @("pypi.org", "download.pytorch.org", "huggingface.co", "github.com")
$unreachable = @()
foreach ($host_ in $origins) {
    $ok = $false
    try {
        $client = New-Object System.Net.Sockets.TcpClient
        $ok = $client.ConnectAsync($host_, 443).Wait(750)
    } catch { $ok = $false } finally { if ($client) { $client.Dispose() } }
    if (-not $ok) { $unreachable += $host_ }
}
if ($unreachable.Count -eq $origins.Count) {
    Fail "no network: none of $($origins -join ', ') answers on port 443"
} elseif ($unreachable.Count -gt 0) {
    Write-Warn2 "these are not answering: $($unreachable -join ', '). The download may fail."
} else {
    Write-Ok "the four download origins answer"
}

# Rutas largas. Los paquetes de Python anidan mucho, y con el limite clasico de
# 260 caracteres la instalacion de la voz revienta a medias con un error de
# fichero no encontrado que no tiene nada que ver con lo que pasa. Medido: el
# umbral esta cerca de los 96 caracteres de ruta raiz con -WithVoice.
$longPaths = 0
try {
    $longPaths = (Get-ItemProperty "HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem" -Name LongPathsEnabled -ErrorAction Stop).LongPathsEnabled
} catch { $longPaths = 0 }
$rootLen = $DataRoot.Length
if ($longPaths -ne 1 -and $rootLen -gt 90) {
    Write-Warn2 "long paths are off and this root is $rootLen characters ($DataRoot)"
    Write-Info "with -WithVoice that tends to break. Either move the project higher up, or turn on"
    Write-Info "LongPathsEnabled: reg add HKLM\SYSTEM\CurrentControlSet\Control\FileSystem /v LongPathsEnabled /t REG_DWORD /d 1 /f"
}

# La GPU: sin ella el modelo va a CPU y no da tiempo real, asi que se avisa
# fuerte pero no se aborta.
$dtype = "bfloat16"
# Si la deteccion obliga a bajar, la fusion puede pisar un bfloat16 heredado.
$dtypeForced = $false
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
                $dtypeForced = $true
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
        # El unico Invoke-NativeLive del fichero que descartaba su codigo de
        # salida. Sin mirarlo, un winget que no instalo nada terminaba dando el
        # consejo equivocado ("cierra esta consola y abre otra"), que no arregla
        # nada porque no hay nada que encontrar en el PATH.
        # 0x8A15002B es "ya estaba instalado", y eso no es un fallo.
        $wingetCode = Invoke-NativeLive "winget" @(
            "install", "-e", "--id", "Python.Python.3.12",
            "--accept-source-agreements", "--accept-package-agreements"
        )
        $wingetOk = ($wingetCode -eq 0 -or $wingetCode -eq 0x8A15002B)
        # winget no refresca el PATH de esta sesion.
        $env:Path = [Environment]::GetEnvironmentVariable("Path", "Machine") + ";" +
                    [Environment]::GetEnvironmentVariable("Path", "User")
        $python = Find-Python
        if (-not $python) {
            # Dos causas distintas con dos remedios distintos. Darlas por la
            # misma mandaba a reabrir la consola a quien no tiene Python.
            if ($wingetOk) {
                Fail "Python installed but not locatable. Close this console, open another one and run again."
            }
            Fail "winget could not install Python (exit code $wingetCode). Install it by hand from https://www.python.org/downloads/"
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
# La voz es opt-in y cuesta 11 GB aparte. Que su fallo se llevara por delante
# una instalacion de ASR ya TERMINADA -y ademas los pasos de configuracion,
# compilacion y verificacion que vienen detras- era desproporcionado. Aqui
# dentro nada aborta: se apunta el motivo y se sigue.
$VoiceFailed = $null
if ($WithVoice) {
    Write-Step "Voice environment (chatterbox + kokoro)"
  try {

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
            Fail-Voice "could not create the voice environment"
        }
        Write-Ok "created in .venv-tts"
    }

    if ((Invoke-NativeLive $VenvTtsPython @("-m", "pip", "install", "--quiet", "--upgrade", "pip", "setuptools", "wheel")) -ne 0) {
        Fail-Voice "could not upgrade pip in the voice environment"
    }

    $probe = Invoke-Native $VenvTtsPython @("-c", "import torch; print(torch.__version__)") -Quiet
    if ($probe.Code -eq 0 -and -not $Force) {
        Write-Ok "torch already installed: $($probe.Output.Trim())"
    } else {
        Write-Info "downloading torch for the voice (~2.8 GB, takes a while)..."
        if ((Invoke-NativeLive $VenvTtsPython @("-m", "pip", "install", "torch", "torchaudio", "--index-url", $TorchIndex)) -ne 0) {
            Fail-Voice "could not install torch in the voice environment"
        }
        Write-Ok "torch installed"
    }

    $reqTts = Join-Path $Root "sidecar\requirements-tts.txt"
    if (-not (Test-Path $reqTts)) { Fail "can't find $reqTts" }
    if ((Invoke-NativeLive $VenvTtsPython @("-m", "pip", "install", "--quiet", "-r", $reqTts)) -ne 0) {
        Fail-Voice "could not install the voice dependencies"
    }
    # --no-deps A PROPOSITO: chatterbox-tts pina torch==2.6 y transformers
    # exactos que aqui no valen; sin esto, pip reinstalaria torch sin CUDA.
    # Sus dependencias reales ya vienen de requirements-tts.txt.
    if ((Invoke-NativeLive $VenvTtsPython @("-m", "pip", "install", "--quiet", "--no-deps", "chatterbox-tts")) -ne 0) {
        Fail-Voice "could not install chatterbox-tts"
    }

    # La sonda de verdad: importar los dos motores. Instalar en limpio es
    # justo donde aparecen las dependencias que faltan en la lista.
    $engines = Invoke-Native $VenvTtsPython @("-c", "import chatterbox.mtl_tts, kokoro; print('ok')") -Quiet
    if ($engines.Code -ne 0) {
        Fail-Voice "the voice engines cannot be imported; look at: $VenvTtsPython -c `"import chatterbox.mtl_tts, kokoro`""
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
        Fail-Voice "could not download the voice models"
    }
    Write-Ok "voice models ready"
    Write-Info "the voice is turned on in the app (the 'Speak for me' section); for"
    Write-Info "the virtual microphone you also need VB-CABLE: https://vb-audio.com/Cable/"
  } catch {
        $VoiceFailed = $_.Exception.Message
        Write-Warn2 "the voice did not finish installing: $VoiceFailed"
        Write-Info "everything else is done; add the voice later with -WithVoice"
  }
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

$template = @(
    "# Generated by scripts\install.ps1. Most keys can be changed from the app;",
    "# the interpreter paths below can only be changed here, or by running the",
    "# installer again.",
    ("python = " + (ConvertTo-TomlString $VenvPython)),
    'script = "sidecar/asr_server.py"',
    'mt_script = "sidecar/mt_server.py"'
)
if ($ModelsDir) { $template += ("hf_home = " + (ConvertTo-TomlString $ModelsDir)) }
$template += @(
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
    ("output_dir = " + (ConvertTo-TomlString $outDir)),
    'output_name = "transcript"'
)

# La tabla [speak] va la ultima: en TOML no puede haber claves de raiz
# despues de una tabla.
if ($WithVoice) {
    $template += @(
        "",
        "# Synthetic voice: speak your translation through a virtual microphone.",
        "# It is turned on from the app (the 'Speak for me' section), where you",
        "# also pick the WAV with your voice; without it, chatterbox won't start.",
        "[speak]",
        "enabled = false",
        'engine = "chatterbox"',
        ("python = " + (ConvertTo-TomlString $VenvTtsPython)),
        'script = "sidecar/tts_server.py"'
    )
}

# Escribir la configuracion es una FUSION, no una sustitucion.
#
# Antes se componia el fichero entero y se volcaba. Con -Force eso se llevaba
# por delante todo lo que el usuario hubiera tocado desde la interfaz: los dos
# dispositivos, los idiomas del micro, la carpeta de salida y —si no se pasaba
# -WithVoice— la tabla [speak] ENTERA, incluido el output_device_id de
# VB-CABLE, que es el motivo mismo de la funcion. Y verify.ps1 recomendaba ese
# comando como remedio.
#
# El instalador solo es dueño de siete claves, las que describen ESTA maquina:
# python, script, mt_script, hf_home, dtype, y speak.python / speak.script.
# Las otras 29 son del usuario y no se tocan.
$fresh = $true
if (Test-Path -LiteralPath $configPath) {
    $existing = @(Get-Content -LiteralPath $configPath -Encoding UTF8)
    $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
    if ($ResetConfig) {
        Copy-Item -LiteralPath $configPath "$configPath.bak-$stamp"
        Write-Warn2 "-ResetConfig: your settings were replaced. The old file is at $configPath.bak-$stamp"
    } elseif (-not ($existing -match '^\s*[A-Za-z_][A-Za-z0-9_]*\s*=')) {
        # Ni una clave legible: no hay nada que fusionar. Editarlo a ciegas
        # dejaria a la aplicacion con los valores por defecto sin decir nada.
        Copy-Item -LiteralPath $configPath "$configPath.bak-$stamp"
        Write-Warn2 "the configuration had nothing readable in it; starting fresh. The old file is at $configPath.bak-$stamp"
    } else {
        $lines = $existing
        $fresh = $false
    }
}
if ($fresh) { $lines = $template }

$lines = Set-TomlValue -Lines $lines -Key 'python'    -Rendered (ConvertTo-TomlString $VenvPython)
$lines = Set-TomlValue -Lines $lines -Key 'script'    -Rendered '"sidecar/asr_server.py"'
$lines = Set-TomlValue -Lines $lines -Key 'mt_script' -Rendered '"sidecar/mt_server.py"'
if ($ModelsDir) { $lines = Set-TomlValue -Lines $lines -Key 'hf_home' -Rendered (ConvertTo-TomlString $ModelsDir) }

# dtype solo cuando el instalador SABE algo: si falta, o si la deteccion
# obligo a bajar de bfloat16 y lo que hay escrito sigue siendo bfloat16. Nunca
# al reves: un float16 puesto a mano no se distingue de uno heredado, asi que
# no se sube solo. verify.ps1 avisa si la tarjeta da para mas.
$dtypeLine = @($lines | Where-Object { $_ -match '^\s*dtype\s*=' })
if ($fresh -or $dtypeLine.Count -eq 0 -or ($dtypeForced -and $dtypeLine[0] -match 'bfloat16')) {
    $lines = Set-TomlValue -Lines $lines -Key 'dtype' -Rendered (ConvertTo-TomlString $dtype)
}

if ($WithVoice) {
    $lines = Set-TomlValue -Lines $lines -Key 'python' -Rendered (ConvertTo-TomlString $VenvTtsPython) -Section 'speak'
    $lines = Set-TomlValue -Lines $lines -Key 'script' -Rendered '"sidecar/tts_server.py"' -Section 'speak'
}

Write-TomlFile $configPath $lines
if ($fresh) {
    Write-Ok "wrote transcriber-config.toml"
    Write-Info "output_dir = $outDir"
} else {
    Write-Ok "updated transcriber-config.toml (your settings were kept)"
}
Write-Info "dtype = $dtype"
if ($WithVoice) { Write-Info "the voice interpreter for the app is: $VenvTtsPython" }

# ---------------------------------------------------------------------------
Write-Step "Application"

if ($SkipBuild) {
    Write-Ok "skipped (-SkipBuild)"
} else {
    $missing = @()
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { $missing += "Rust (https://rustup.rs)" }
    if (-not (Get-Command npm -ErrorAction SilentlyContinue)) { $missing += "Node.js (https://nodejs.org)" }
    # Que cargo EXISTA no basta: en Windows enlaza con link.exe, de las Build
    # Tools de Visual Studio, y sin ellas la compilacion muere al final del
    # todo con un error de linker que no dice que instalar. Es la asimetria
    # que habia: sin cargo, aviso blando; con cargo a medias, fallo duro.
    if ((Get-Command cargo -ErrorAction SilentlyContinue) -and -not (Get-Command link.exe -ErrorAction SilentlyContinue)) {
        $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
        $hasVc = $false
        if (Test-Path $vswhere) {
            $found = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2>$null
            $hasVc = [bool]$found
        }
        if (-not $hasVc) {
            $missing += "the C++ build tools (Visual Studio Build Tools, 'Desktop development with C++')"
        }
    }
    # Vite 5 pide Node 18+. Comprobar la version y no solo la presencia.
    $nodeProbe = Invoke-Native "node" @("--version") -Quiet
    if ($nodeProbe.Code -eq 0 -and $nodeProbe.Output -match "^v(\d+)") {
        if ([int]$Matches[1] -lt 18) { $missing += "Node 18 or newer (found $($nodeProbe.Output.Trim()))" }
    }
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
        # No se aborta: con `Fail` aqui, el resumen y la lista de avisos —que
        # es donde esta escrito QUE ha fallado— no llegaban a imprimirse nunca.
        if ($LASTEXITCODE -ne 0) {
            Write-Warn2 "verification did not pass; look at the messages above"
        }
    } else {
        Write-Warn2 "can't find verify.ps1"
    }
}

# ---------------------------------------------------------------------------
Write-Host ""
if ($VoiceFailed) {
    Write-Host "  Installation finished, without the voice" -ForegroundColor Yellow
    Write-Host "  Transcription and translation are ready. The voice is not:" -ForegroundColor DarkGray
    Write-Host "    $VoiceFailed" -ForegroundColor DarkGray
    Write-Host "  Run again with -WithVoice once that is sorted." -ForegroundColor DarkGray
} else {
    Write-Host "  Installation finished" -ForegroundColor Green
}
if ($script:Warnings.Count -gt 0) {
    Write-Host ""
    Write-Host "  Warnings:" -ForegroundColor Yellow
    foreach ($w in $script:Warnings) { Write-Host "   - $w" -ForegroundColor Yellow }
}
Write-Host ""
# Los consejos cambian segun donde se este: desde una instalacion no hay
# `npm` ni `cargo`, y decirle a ese usuario que compile algo era mandarle a un
# sitio al que no puede ir. El asr-cli.exe empaquetado si lo tiene al lado.
$cliHint = Join-Path $Root "asr-cli.exe"
if ($FromSource) {
    Write-Host "  Start in development:  npm run app:dev"
    Write-Host "  Test without the UI:   cargo run -p asr-cli -- devices"
    if ($WithVoice) {
        Write-Host "  Test the voice:        cargo run -p asr-cli -- speak --engine kokoro --lang es --text `"hola`" --python .venv-tts\Scripts\python.exe"
    }
} else {
    Write-Host "  Start:                 the LiveTranscriber shortcut"
    if (Test-Path $cliHint) {
        Write-Host "  Test without the UI:   `"$cliHint`" devices"
        if ($WithVoice) {
            Write-Host "  Test the voice:        `"$cliHint`" speak --engine kokoro --lang es --text `"hola`" --python `"$VenvTtsPython`""
        }
    }
}
Write-Host ""
Write-Host "  Turn the Windows volume up before testing: the loopback captures" -ForegroundColor DarkGray
Write-Host "  after the volume control." -ForegroundColor DarkGray
Write-Host ""
