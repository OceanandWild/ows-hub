<#
  OWS Hub — copia el frontend del panel (web/OWS) a ./app

  El frontend vive en el monorepo `owsdatabase` (web/OWS) y es la fuente
  única: aquí solo se copia para que Tauri pueda embeberlo
  (tauri.conf.json -> build.frontendDist = "../app").

  En GitHub Actions la copia se hace desde un checkout de owsdatabase
  (ver .github/workflows/release.yml, paso "Fetch frontend").
#>
param(
  [string]$Source = "",
  [string]$Dest   = ""
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

if (-not $Dest)   { $Dest   = Join-Path $root "app" }
if (-not $Source) { $Source = Join-Path (Split-Path -Parent $root) "web\OWS" }

if (-not (Test-Path -LiteralPath $Source)) {
  Write-Host "[sync-app] Sin frontend en '$Source' — se conserva el existente en '$Dest'." -ForegroundColor Yellow
  if (Test-Path -LiteralPath $Dest) { exit 0 }
  throw "No hay frontend para copiar: ni '$Source' ni '$Dest' existen."
}

if (Test-Path -LiteralPath $Dest) { Remove-Item -LiteralPath $Dest -Recurse -Force }
New-Item -ItemType Directory -Path $Dest -Force | Out-Null

# Copy-Item recursivo; -Force pisa, y no seguimos junctions.
Get-ChildItem -LiteralPath $Source -Force | ForEach-Object {
  Copy-Item -LiteralPath $_.FullName -Destination $Dest -Recurse -Force
}

$files = (Get-ChildItem -LiteralPath $Dest -Recurse -File -Force | Measure-Object).Count
Write-Host "[sync-app] Frontend copiado: $Source -> $Dest ($files archivos)" -ForegroundColor Green