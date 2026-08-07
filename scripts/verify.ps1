<#
.SYNOPSIS
    Checks that a LiveTranscriber installation works.

.DESCRIPTION
    Can be run on its own when something stops working. It walks, in dependency
    order, through every piece that has to be in place, and stops at the first
    one that fails, saying what to do about it.

    `install.ps1` calls it at the end, but it stands on its own.

.EXAMPLE
    .\scripts\verify.ps1
#>
#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$Root
)

$ErrorActionPreference = "Stop"
if (-not $Root) { $Root = Split-Path -Parent $PSScriptRoot }

# Igual que install.ps1: este script viaja tambien dentro del instalador, y ahi
# los entornos no estan al lado del codigo sino en el perfil del usuario.
$FromSource = Test-Path (Join-Path $Root "Cargo.toml")
$DataRoot = if ($FromSource) { $Root } else { Join-Path $env:LOCALAPPDATA "LiveTranscriber" }

$script:Failed = 0

<#
Ejecuta un programa externo sin que su stderr tumbe el script.

Con $ErrorActionPreference = "Stop", PowerShell 5.1 convierte lo que un .exe
escriba en stderr en un error terminante, y `2>$null` solo esconde el texto: no
evita el error. Aqui casi todo son sondas cuyo fallo es esperable (¿esta torch?,
¿esta transformers?), asi que se decide por el codigo de salida.
#>
function Invoke-Native {
    param(
        [Parameter(Mandatory)][string]$Exe,
        [string[]]$Arguments = @(),
        [switch]$Live
    )
    $previous = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        if ($Live) {
            & $Exe @Arguments | Out-Host
            return [pscustomobject]@{ Output = ""; Code = $LASTEXITCODE }
        }
        $out = & $Exe @Arguments 2>$null
        return [pscustomobject]@{ Output = (@($out) -join "`n"); Code = $LASTEXITCODE }
    } finally {
        $ErrorActionPreference = $previous
    }
}

function Check($name) { Write-Host ("  {0,-38}" -f $name) -NoNewline }
function Pass($detail) {
    Write-Host "OK" -ForegroundColor Green -NoNewline
    if ($detail) { Write-Host "  $detail" -ForegroundColor DarkGray } else { Write-Host "" }
}
function Bad($detail, $fix) {
    Write-Host "FAILED" -ForegroundColor Red -NoNewline
    Write-Host "  $detail" -ForegroundColor Red
    if ($fix) { Write-Host "         -> $fix" -ForegroundColor Yellow }
    $script:Failed++
}

Write-Host ""
Write-Host "  Verification" -ForegroundColor White
Write-Host ""

# --- configuracion ---------------------------------------------------------
# Misma ubicacion canonica que install.ps1 y que la aplicacion. Con
# LIVETRANSCRIBER_CONFIG puesto se respeta, para poder verificar un desarrollo.
$configPath = if ($env:LIVETRANSCRIBER_CONFIG) {
    $env:LIVETRANSCRIBER_CONFIG
} else {
    Join-Path (Join-Path $env:APPDATA "LiveTranscriber") "transcriber-config.toml"
}
Check "configuration"
if (-not (Test-Path $configPath)) {
    Bad "transcriber-config.toml not found" "run scripts\install.ps1"
    exit 1
}
Pass $configPath

$config = Get-Content $configPath -Raw
function Get-Value($key) {
    $m = [regex]::Match($config, "(?m)^\s*$key\s*=\s*['""]?([^'""\r\n]+)['""]?")
    if ($m.Success) { return $m.Groups[1].Value.Trim() }
    return $null
}

$python = Get-Value "python"
$dtype = Get-Value "dtype"
$hfHome = Get-Value "hf_home"
$outDir = Get-Value "output_dir"

# --- interprete ------------------------------------------------------------
Check "Python interpreter"
if (-not $python -or -not (Test-Path $python)) {
    Bad "not found: $python" "fix `python` in transcriber-config.toml"
    exit 1
}
$pv = Invoke-Native $python @("-c", "import sys; print('%d.%d.%d' % sys.version_info[:3])")
if ($pv.Code -ne 0) {
    Bad "does not start" "rebuild the environment with install.ps1 -Force"
    exit 1
}
Pass "Python $($pv.Output.Trim())"

# De donde sale el interprete importa: si es de otro proyecto, esto funciona
# hasta el dia en que ese proyecto se mueva, se borre o se le instale algo que
# rompa la compatibilidad. Conviene que se vea, no que se descubra el dia malo.
Check "environment ownership"
$ownVenv = Join-Path $DataRoot ".venv"
if ($python -like (Join-Path $ownVenv "*")) {
    Pass "own (project .venv)"
} else {
    Write-Host "SHARED" -ForegroundColor Yellow -NoNewline
    Write-Host "  it is outside the project" -ForegroundColor DarkGray
    Write-Host ("         -> {0}" -f (Split-Path -Parent (Split-Path -Parent $python))) -ForegroundColor DarkGray
    Write-Host "         -> fine, but if you move or clean that project this stops starting." -ForegroundColor DarkGray
    Write-Host "            For your own: .\scripts\install.ps1" -ForegroundColor DarkGray
}

# --- sidecars --------------------------------------------------------------
foreach ($pair in @(@("ASR sidecar", "script"), @("translation sidecar", "mt_script"))) {
    Check $pair[0]
    $rel = Get-Value $pair[1]
    $abs = $rel
    if (-not [System.IO.Path]::IsPathRooted($rel)) { $abs = Join-Path $Root $rel }
    if (Test-Path $abs) { Pass $rel } else { Bad "not found: $abs" "check the path in the configuration" }
}

# --- torch y CUDA ----------------------------------------------------------
Check "PyTorch and CUDA"
$torchScript = @"
import torch
ok = torch.cuda.is_available()
name = torch.cuda.get_device_name() if ok else 'no GPU'
cap = torch.cuda.get_device_capability() if ok else (0, 0)
tag = 'sm_%d%d' % cap
arches = torch.cuda.get_arch_list()
print('%s|%s|%s|%d.%d|%s' % (torch.__version__, ok, name, cap[0], cap[1], tag in arches))
"@
$torch = Invoke-Native $python @("-c", $torchScript)
if ($torch.Code -ne 0) {
    Bad "cannot import torch" "install.ps1 -Force"
} else {
    $f = $torch.Output.Trim() -split "\|"
    if ($f[1] -ne "True") {
        Bad "torch does not see the GPU" "check the NVIDIA driver with nvidia-smi"
    } elseif ($f[4] -ne "True") {
        Bad "the torch wheel has no code for $($f[2])" "a different CUDA version is needed"
    } else {
        Pass "$($f[0]) - $($f[2]) - capability $($f[3])"
        # Coherencia entre la precision configurada y lo que soporta la tarjeta.
        $major = [int]($f[3] -split "\.")[0]
        Check "configured precision"
        if ($dtype -eq "bfloat16" -and $major -lt 8) {
            Bad "dtype = bfloat16 but this GPU has no native support" "set dtype = `"float16`" in the configuration"
        } else {
            Pass $dtype
        }
    }
}

# --- transformers ----------------------------------------------------------
Check "transformers"
$tvProbe = Invoke-Native $python @("-c", "import transformers; print(transformers.__version__)")
if ($tvProbe.Code -ne 0) {
    Bad "not installed" "install.ps1"
} else {
    $tv = $tvProbe.Output.Trim()
    $clean = ($tv -replace '[^\d.].*$','')
    if ([version]$clean -lt [version]"5.13") {
        Bad "$tv is too old" "5.13+ is required: that is where AutoModelForRNNT lives"
    } else {
        Pass $tv
    }
}

# --- modelos en cache ------------------------------------------------------
Check "downloaded models"
if ($hfHome) { $hubRoot = Join-Path $hfHome "hub" } else { $hubRoot = Join-Path $env:USERPROFILE ".cache\huggingface\hub" }
$asrDir = Join-Path $hubRoot "models--nvidia--nemotron-3.5-asr-streaming-0.6b"
if (Test-Path $asrDir) {
    $size = (Get-ChildItem $asrDir -Recurse -File -EA SilentlyContinue | Measure-Object Length -Sum).Sum / 1GB
    Pass ("ASR {0:N2} GB" -f $size)
} else {
    Bad "the ASR model is missing in $hubRoot" "python scripts\fetch_models.py"
}

Check "translation model"
$mtDir = Join-Path $hubRoot "models--facebook--nllb-200-distilled-600M"
if (Test-Path $mtDir) {
    $size = (Get-ChildItem $mtDir -Recurse -File -EA SilentlyContinue | Measure-Object Length -Sum).Sum / 1GB
    Pass ("NLLB {0:N2} GB" -f $size)
} else {
    Write-Host "MISSING" -ForegroundColor Yellow -NoNewline
    Write-Host "  only needed if you turn translation on" -ForegroundColor DarkGray
}

# --- voz sintetica (opcional) ------------------------------------------------
# [speak] es una tabla TOML y sus claves (python, script) se llaman igual que
# las de la raiz, asi que Get-Value se equivocaria: primero se recorta el
# texto de la seccion y se busca solo dentro.
$speakSection = [regex]::Match($config, "(?ms)^\[speak\].*?(?=^\[|\z)").Value
function Get-SpeakValue($key) {
    if (-not $speakSection) { return $null }
    $m = [regex]::Match($speakSection, "(?m)^\s*$key\s*=\s*['""]?([^'""\r\n]+)['""]?")
    if ($m.Success) { return $m.Groups[1].Value.Trim() }
    return $null
}

Check "synthetic voice"
if (-not $speakSection -or (Get-SpeakValue "enabled") -ne "true") {
    Write-Host "DISABLED" -ForegroundColor Yellow -NoNewline
    Write-Host "  optional; turn it on in the app ('Speak for me' section)" -ForegroundColor DarkGray
} else {
    Pass "enabled"

    $speakPython = Get-SpeakValue "python"
    Check "voice venv interpreter"
    if (-not $speakPython -or -not (Test-Path $speakPython)) {
        Bad "not found: $speakPython" "install.ps1 -WithVoice, or fix [speak].python"
    } else {
        $spv = Invoke-Native $speakPython @("-c", "import sys; print('%d.%d.%d' % sys.version_info[:3])")
        if ($spv.Code -ne 0) {
            Bad "does not start" "install.ps1 -WithVoice -Force"
        } else {
            Pass "Python $($spv.Output.Trim())"

            # La sonda que de verdad separa "venv creado" de "venv utilizable".
            Check "voice engines"
            $engines = Invoke-Native $speakPython @("-c", "import chatterbox.mtl_tts, kokoro; print('ok')")
            if ($engines.Code -ne 0) {
                Bad "chatterbox/kokoro cannot be imported" "install.ps1 -WithVoice (the dependencies live in sidecar\requirements-tts.txt)"
            } else {
                Pass "chatterbox and kokoro are importable"
            }
        }
    }

    Check "voice sidecar"
    $speakScript = Get-SpeakValue "script"
    if (-not $speakScript) { $speakScript = "sidecar/tts_server.py" }
    $abs = $speakScript
    if (-not [System.IO.Path]::IsPathRooted($abs)) { $abs = Join-Path $Root $abs }
    if (Test-Path $abs) { Pass $speakScript } else { Bad "not found: $abs" "check [speak].script in the configuration" }

    if ((Get-SpeakValue "engine") -ne "kokoro") {
        Check "voice sample to clone"
        $wav = Get-SpeakValue "voice_wav"
        if ($wav -and (Test-Path $wav)) {
            Pass $wav
        } elseif ($wav) {
            Bad "not found: $wav" "pick the WAV in the app or fix [speak].voice_wav"
        } else {
            Bad "not configured" "record 10-30 s of your voice and pick it in the app; chatterbox will not start without it"
        }
    }

    Check "voice model"
    $cbDir = Join-Path $hubRoot "models--ResembleAI--chatterbox"
    if (Test-Path $cbDir) {
        $size = (Get-ChildItem $cbDir -Recurse -File -EA SilentlyContinue | Measure-Object Length -Sum).Sum / 1GB
        Pass ("chatterbox {0:N2} GB" -f $size)
    } else {
        Write-Host "MISSING" -ForegroundColor Yellow -NoNewline
        Write-Host "  it downloads itself on first use (~3.4 GB); install.ps1 -WithVoice leaves it downloaded" -ForegroundColor DarkGray
    }
}

# --- carpeta de salida -----------------------------------------------------
Check "output folder"
if ($outDir -and [System.IO.Path]::IsPathRooted($outDir)) {
    New-Item -ItemType Directory -Force -Path $outDir | Out-Null
    Pass $outDir
} else {
    Bad "not an absolute path: $outDir" "pick it in the app or set a full path"
}

# --- captura de audio ------------------------------------------------------
Check "WASAPI capture"
$cli = Join-Path $Root "target\debug\asr-cli.exe"
if (-not (Test-Path $cli)) { $cli = Join-Path $Root "target\release\asr-cli.exe" }
if (Test-Path $cli) {
    $devices = Invoke-Native $cli @("devices")
    if ($devices.Code -eq 0) {
        $n = @($devices.Output -split "`n" | Where-Object { $_ -match "^\s+id: " }).Count
        Pass "$n devices"
    } else {
        Bad "asr-cli devices failed" "look at the output of: $cli devices"
    }
} else {
    Write-Host "SKIPPED" -ForegroundColor Yellow -NoNewline
    Write-Host "  build it with: cargo build --workspace" -ForegroundColor DarkGray
}

# --- la prueba de verdad ---------------------------------------------------
if ($script:Failed -eq 0) {
    Write-Host ""
    Write-Host "  Full pipeline test (loads the model, takes a while)" -ForegroundColor White
    $smoke = Join-Path $PSScriptRoot "smoke_test.py"
    if (Test-Path $smoke) {
        $dt = $dtype
        if (-not $dt) { $dt = "bfloat16" }
        $smokeRun = Invoke-Native $python @($smoke, "--python", $python, "--dtype", $dt) -Live
        if ($smokeRun.Code -ne 0) { $script:Failed++ }
    }
}

Write-Host ""
if ($script:Failed -eq 0) {
    Write-Host "  All good." -ForegroundColor Green
    Write-Host ""
    exit 0
}
Write-Host "  $($script:Failed) check(s) failed." -ForegroundColor Red
Write-Host ""
exit 1
