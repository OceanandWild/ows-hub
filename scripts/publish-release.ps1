<#
  OWS Hub — publica los artefactos de `tauri build` como GitHub Release
  y genera el manifiesto estático del updater (latest.json).

  Acepta:
    -Tag        etiqueta de la release (ej: v3.1.4). Vacío = "v<tauri.conf.json>"
    -DryRun     solo genera/valida el manifiesto sin subir nada
#>
param(
  [string]$Tag = "",
  [switch]$DryRun
)

$ErrorActionPreference = "Stop"
$root    = Split-Path -Parent $PSScriptRoot
$bundle  = Join-Path $root "src-tauri\target\release\bundle"
$cfg     = Get-Content (Join-Path $root "src-tauri\tauri.conf.json") -Raw | ConvertFrom-Json
$version = [string]$cfg.version
if (-not $Tag) { $Tag = "v$version" }
if (-not $env:GH_REPO) { $env:GH_REPO = "OceanandWild/ows-hub" }

# ── 1) Recolectar artefactos (instalador + firma) ──
if (-not (Test-Path -LiteralPath $bundle)) {
  throw "No existe '$bundle'. Ejecuta `npx tauri build` primero."
}

$assets = Get-ChildItem -LiteralPath $bundle -Recurse -File |
  Where-Object { $_.Extension -in ".exe", ".msi" -and $_.Name -notmatch "unins" } |
  Sort-Object Name

if (-not $assets) { throw "No se encontraron .exe/.msi en '$bundle'." }

# El updater de Tauri v2 necesita la firma (.sig) junto al binario.
$platforms = [ordered]@{}
$upload = @()

foreach ($a in $assets) {
  $upload += $a.FullName
  $sig = "$($a.FullName).sig"
  if (-not (Test-Path -LiteralPath $sig)) {
    Write-Host "[publish] AVISO: sin firma para $($a.Name) — no irá al updater." -ForegroundColor Yellow
    continue
  }
  $upload += $sig
  $key = if ($a.Extension -eq ".msi") { "windows-x86_64" } else { "windows-x86_64" }
  $platforms[$key] = @{
    signature = ([IO.File]::ReadAllText($sig)).Trim()
    url       = "https://github.com/$($env:GH_REPO)/releases/download/$Tag/$([Uri]::EscapeDataString($a.Name))"
  }
}

if (-not $platforms.Count) {
  throw "Ningún artefacto tiene firma .sig: la release no sería instalable desde el updater."
}

$latest = [ordered]@{
  version   = $version
  notes     = "OWS Hub $Tag — instalador de Windows. Las descargas del navegador requieren esta app."
  pub_date  = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
  platforms = $platforms
}
$latestPath = Join-Path $root "latest.json"
[IO.File]::WriteAllText($latestPath, ($latest | ConvertTo-Json -Depth 8))
$upload    += $latestPath

Write-Host "[publish] Release $Tag ($($assets.Count) artefactos):"
$assets | ForEach-Object { Write-Host "  - $($_.Name) ($([math]::Round($_.Length/1MB,1)) MB)" }
Write-Host "[publish] Manifiesto: $latestPath"

if ($DryRun) { Write-Host "[publish] DryRun: no se subió nada." -ForegroundColor Yellow; exit 0 }

# ── 2) Crear/actualizar la release ──
$owner = $env:GH_REPO.Split("/")[0]
$repo  = $env:GH_REPO.Split("/")[1]
$api   = "https://api.github.com/repos/$owner/$repo"
$hdr   = @{ Authorization = "Bearer $env:GITHUB_TOKEN"; Accept = "application/vnd.github+json"; "User-Agent" = "OWS-Hub-publish" }

$existing = $null
try { $existing = Invoke-RestMethod -Uri "$api/releases/tags/$Tag" -Headers $hdr } catch { }

if (-not $existing) {
  $body = @{
    tag_name = $Tag
    name     = "OWS Hub $Tag"
    body     = "OWS Hub $Tag`n`nInstalador de Windows (NSIS) + manifiesto del updater (``latest.json``).`n`nEsta app es **obligatoria** para continuar con las descargas del ecosistema: en el navegador las descargas estan bloqueadas."
    draft    = $false
    prerelease = $false
  } | ConvertTo-Json
  $release = Invoke-RestMethod -Uri "$api/releases" -Method Post -Headers $hdr -ContentType "application/json" -Body ([Text.Encoding]::UTF8.GetBytes($body))
  Write-Host "[publish] Release creada: $($release.html_url)"
} else {
  $release = $existing
  Write-Host "[publish] Release existente: $($release.html_url)"
}

# La plantilla de subida viene en release.upload_url (".../assets{?name,label}").
$uploadUrl = ([string]$release.upload_url) -replace '\{.*\}$', ''

foreach ($f in $upload) {
  $name = Split-Path -Leaf $f
  # Reemplaza el asset anterior si existe (mismo nombre).
  $existing = @($release.assets) | Where-Object { $_.name -eq $name } | Select-Object -First 1
  if ($existing) {
    Invoke-RestMethod -Uri $existing.url -Method Delete -Headers $hdr -ErrorAction SilentlyContinue | Out-Null
  }
  Write-Host "[publish] Subiendo $name ($([math]::Round((Get-Item $f).Length/1MB,1)) MB) ..."
  $bytes = [IO.File]::ReadAllBytes($f)
  Invoke-RestMethod -Uri "$uploadUrl`?name=$([Uri]::EscapeDataString($name))" -Method Post `
    -Headers $hdr -ContentType "application/octet-stream" -Body $bytes | Out-Null
}

Write-Host "[publish] OK -> $($release.html_url)" -ForegroundColor Green
Write-Host "[publish] Updater: https://github.com/$($env:GH_REPO)/releases/latest/download/latest.json"