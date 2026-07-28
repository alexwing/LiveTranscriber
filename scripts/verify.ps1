<#
.SYNOPSIS
    Comprueba que una instalacion de LiveTranscriber funciona.

.DESCRIPTION
    Se puede lanzar suelto cuando algo deje de ir. Repasa, en orden de
    dependencia, cada pieza que tiene que estar en su sitio, y para en la
    primera que falle diciendo que hacer.

    Lo llama `install.ps1` al final, pero vale por si mismo.

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
    Write-Host "FALLO" -ForegroundColor Red -NoNewline
    Write-Host "  $detail" -ForegroundColor Red
    if ($fix) { Write-Host "         -> $fix" -ForegroundColor Yellow }
    $script:Failed++
}

Write-Host ""
Write-Host "  Verificacion" -ForegroundColor White
Write-Host ""

# --- configuracion ---------------------------------------------------------
$configPath = Join-Path $Root "transcriber-config.toml"
Check "configuracion"
if (-not (Test-Path $configPath)) {
    Bad "no existe transcriber-config.toml" "ejecuta scripts\install.ps1"
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
Check "interprete de Python"
if (-not $python -or -not (Test-Path $python)) {
    Bad "no existe: $python" "corrige `python` en transcriber-config.toml"
    exit 1
}
$pv = Invoke-Native $python @("-c", "import sys; print('%d.%d.%d' % sys.version_info[:3])")
if ($pv.Code -ne 0) {
    Bad "no arranca" "rehaz el entorno con install.ps1 -Force"
    exit 1
}
Pass "Python $($pv.Output.Trim())"

# De donde sale el interprete importa: si es de otro proyecto, esto funciona
# hasta el dia en que ese proyecto se mueva, se borre o se le instale algo que
# rompa la compatibilidad. Conviene que se vea, no que se descubra el dia malo.
Check "de quien es el entorno"
$ownVenv = Join-Path $Root ".venv"
if ($python -like (Join-Path $ownVenv "*")) {
    Pass "propio (.venv del proyecto)"
} else {
    Write-Host "COMPARTIDO" -ForegroundColor Yellow -NoNewline
    Write-Host "  esta fuera del proyecto" -ForegroundColor DarkGray
    Write-Host ("         -> {0}" -f (Split-Path -Parent (Split-Path -Parent $python))) -ForegroundColor DarkGray
    Write-Host "         -> vale, pero si mueves o limpias ese proyecto esto deja de arrancar." -ForegroundColor DarkGray
    Write-Host "            Para uno propio: .\scripts\install.ps1" -ForegroundColor DarkGray
}

# --- sidecars --------------------------------------------------------------
foreach ($pair in @(@("sidecar de ASR", "script"), @("sidecar de traduccion", "mt_script"))) {
    Check $pair[0]
    $rel = Get-Value $pair[1]
    $abs = $rel
    if (-not [System.IO.Path]::IsPathRooted($rel)) { $abs = Join-Path $Root $rel }
    if (Test-Path $abs) { Pass $rel } else { Bad "no existe: $abs" "revisa la ruta en la configuracion" }
}

# --- torch y CUDA ----------------------------------------------------------
Check "PyTorch y CUDA"
$torchScript = @"
import torch
ok = torch.cuda.is_available()
name = torch.cuda.get_device_name() if ok else 'sin GPU'
cap = torch.cuda.get_device_capability() if ok else (0, 0)
tag = 'sm_%d%d' % cap
arches = torch.cuda.get_arch_list()
print('%s|%s|%s|%d.%d|%s' % (torch.__version__, ok, name, cap[0], cap[1], tag in arches))
"@
$torch = Invoke-Native $python @("-c", $torchScript)
if ($torch.Code -ne 0) {
    Bad "no se puede importar torch" "install.ps1 -Force"
} else {
    $f = $torch.Output.Trim() -split "\|"
    if ($f[1] -ne "True") {
        Bad "torch no ve la GPU" "comprueba el driver de NVIDIA con nvidia-smi"
    } elseif ($f[4] -ne "True") {
        Bad "la rueda de torch no trae codigo para $($f[2])" "hace falta otra version de CUDA"
    } else {
        Pass "$($f[0]) - $($f[2]) - capability $($f[3])"
        # Coherencia entre la precision configurada y lo que soporta la tarjeta.
        $major = [int]($f[3] -split "\.")[0]
        Check "precision configurada"
        if ($dtype -eq "bfloat16" -and $major -lt 8) {
            Bad "dtype = bfloat16 pero esta GPU no lo tiene nativo" "pon dtype = `"float16`" en la configuracion"
        } else {
            Pass $dtype
        }
    }
}

# --- transformers ----------------------------------------------------------
Check "transformers"
$tvProbe = Invoke-Native $python @("-c", "import transformers; print(transformers.__version__)")
if ($tvProbe.Code -ne 0) {
    Bad "no instalado" "install.ps1"
} else {
    $tv = $tvProbe.Output.Trim()
    $clean = ($tv -replace '[^\d.].*$','')
    if ([version]$clean -lt [version]"5.13") {
        Bad "$tv es demasiado antigua" "hace falta 5.13+: ahi vive AutoModelForRNNT"
    } else {
        Pass $tv
    }
}

# --- modelos en cache ------------------------------------------------------
Check "modelos descargados"
if ($hfHome) { $hubRoot = Join-Path $hfHome "hub" } else { $hubRoot = Join-Path $env:USERPROFILE ".cache\huggingface\hub" }
$asrDir = Join-Path $hubRoot "models--nvidia--nemotron-3.5-asr-streaming-0.6b"
if (Test-Path $asrDir) {
    $size = (Get-ChildItem $asrDir -Recurse -File -EA SilentlyContinue | Measure-Object Length -Sum).Sum / 1GB
    Pass ("ASR {0:N2} GB" -f $size)
} else {
    Bad "falta el modelo de ASR en $hubRoot" "python scripts\fetch_models.py"
}

Check "modelo de traduccion"
$mtDir = Join-Path $hubRoot "models--facebook--nllb-200-distilled-600M"
if (Test-Path $mtDir) {
    $size = (Get-ChildItem $mtDir -Recurse -File -EA SilentlyContinue | Measure-Object Length -Sum).Sum / 1GB
    Pass ("NLLB {0:N2} GB" -f $size)
} else {
    Write-Host "AUSENTE" -ForegroundColor Yellow -NoNewline
    Write-Host "  solo hace falta si activas la traduccion" -ForegroundColor DarkGray
}

# --- carpeta de salida -----------------------------------------------------
Check "carpeta de salida"
if ($outDir -and [System.IO.Path]::IsPathRooted($outDir)) {
    New-Item -ItemType Directory -Force -Path $outDir | Out-Null
    Pass $outDir
} else {
    Bad "no es una ruta absoluta: $outDir" "eligela en la app o pon una ruta completa"
}

# --- captura de audio ------------------------------------------------------
Check "captura WASAPI"
$cli = Join-Path $Root "target\debug\asr-cli.exe"
if (-not (Test-Path $cli)) { $cli = Join-Path $Root "target\release\asr-cli.exe" }
if (Test-Path $cli) {
    $devices = Invoke-Native $cli @("devices")
    if ($devices.Code -eq 0) {
        $n = @($devices.Output -split "`n" | Where-Object { $_ -match "^\s+id: " }).Count
        Pass "$n dispositivos"
    } else {
        Bad "asr-cli devices fallo" "mira la salida de: $cli devices"
    }
} else {
    Write-Host "OMITIDO" -ForegroundColor Yellow -NoNewline
    Write-Host "  compila con: cargo build --workspace" -ForegroundColor DarkGray
}

# --- la prueba de verdad ---------------------------------------------------
if ($script:Failed -eq 0) {
    Write-Host ""
    Write-Host "  Prueba de la tuberia completa (carga el modelo, tarda un poco)" -ForegroundColor White
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
    Write-Host "  Todo en orden." -ForegroundColor Green
    Write-Host ""
    exit 0
}
Write-Host "  $($script:Failed) comprobacion(es) fallaron." -ForegroundColor Red
Write-Host ""
exit 1
