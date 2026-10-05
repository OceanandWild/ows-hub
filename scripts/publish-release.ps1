<#
  OWS Hub - publica los artefactos de `tauri build` como GitHub Release
  y genera el manifiesto estatico del updater (latest.json).

  Acepta:
    -Tag        etiqueta de la release (ej: v3.1.4). Vacio = "v<tauri.conf.json>"
    -DryRun     solo valida/genera el manifiesto, sin subir nada

  Requiere:  GITHUB_TOKEN (scope 'repo'), GH_REPO (por defecto OceanandWild/ows-hub)
#>
param(
  [string]$Tag = "",
  [switch]$DryRun
)

$ErrorActionPreference = "Stop"
# Nombres alternativos de secrets (repo ows-hub): KEY = clave de firma,
# PASSWORD = su password, GH_PAT = token con scope repo. Se aceptan con
# cualquiera de los dos nombres para no obligar a renombrar nada.
if (-not $env:TAURI_SIGNING_PRIVATE_KEY -and $env:KEY) { $env:TAURI_SIGNING_PRIVATE_KEY = $env:KEY }
if (-not $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD -and $env:PASSWORD) { $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = $env:PASSWORD }
if (-not $env:GITHUB_TOKEN -and $env:GH_PAT) { $env:GITHUB_TOKEN = $env:GH_PAT }
$root    = Split-Path -Parent $PSScriptRoot
$bundle  = Join-Path $root "src-tauri\target\release\bundle"
$cfg     = Get-Content (Join-Path $root "src-tauri\tauri.conf.json") -Raw | ConvertFrom-Json
$version = [string]$cfg.version
if (-not $Tag) { $Tag = "v$version" }
if (-not $env:GH_REPO) { $env:GH_REPO = "OceanandWild/ows-hub" }

# --- 1) Recolectar artefactos ---
if (-not (Test-Path -LiteralPath $bundle)) {
  throw "No existe '$bundle'. Ejecuta `npx tauri build` primero."
}

# Solo los artefactos de la version actual: el directorio bundle/nsis acumula
# instaladores de versiones anteriores.
$assets = Get-ChildItem -LiteralPath $bundle -Recurse -File |
  Where-Object {
    $_.Extension -in ".exe", ".msi" -and
    $_.Name -notmatch "unins" -and
    $_.Name -like "*$version*"
  } |
  Sort-Object Name

if (-not $assets) { throw "No se encontraron .exe/.msi de la version $version en '$bundle'." }

# El updater de Tauri v2 necesita la firma (.sig) junto al binario.
# Sin clave de firma (TAURI_SIGNING_PRIVATE_KEY) no hay .sig: la release se
# publica igual (la descarga del navegador funciona) pero sin latest.json,
# porque el updater no podria validar el binario.
# GitHub sustituye espacios y caracteres raros del nombre del asset
# ("OWS Hub_x64-setup.exe" -> "OWS.Hub_x64-setup.exe"). Publicamos con guiones
# para que la URL de descarga sea estable y legible.
function Get-AssetName([string]$fileName) {
  $n = $fileName -replace '\s+', '-'
  $n = $n -replace '[^\w\.\-]', ''
  return $n
}

$platforms = [ordered]@{}
$upload = @()

foreach ($a in $assets) {
  $assetName = Get-AssetName $a.Name
  $upload += @{ Path = $a.FullName; Name = $assetName }
  $sig = "$($a.FullName).sig"
  if (-not (Test-Path -LiteralPath $sig)) {
    # El bundler solo deja .sig junto al binario cuando genera updater
    # artifacts; si falta pero hay clave de firma, se firma a mano con
    # `tauri signer sign` (el updater verifica los bytes del instalador
    # contra latest.json, así que vale igual). Sin clave no hay updater.
    $keyPath = $env:TAURI_SIGNING_PRIVATE_KEY_PATH
    if ([string]::IsNullOrWhiteSpace($keyPath)) { $keyPath = Join-Path $env:USERPROFILE ".tauri\ows-hub.key" }
    $hasKey = (Test-Path -LiteralPath $keyPath) -or (-not [string]::IsNullOrWhiteSpace($env:TAURI_SIGNING_PRIVATE_KEY))
    if ($hasKey) {
      Write-Host "[publish] firmando $($a.Name) a mano..." -ForegroundColor Cyan
      $signArgs = @('tauri', 'signer', 'sign', $a.FullName, '--app-version', $version)
      if (Test-Path -LiteralPath $keyPath) { $signArgs += @('--private-key-path', $keyPath) }
      & npx @signArgs
      if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $sig)) { throw "No se pudo firmar $($a.Name)" }
    } else {
      Write-Host "[publish] sin firma para $($a.Name) - no ira al updater." -ForegroundColor Yellow
      continue
    }
  }
  $upload += @{ Path = $sig; Name = "$assetName.sig" }
  $platforms["windows-x86_64"] = @{
    signature = ([IO.File]::ReadAllText($sig)).Trim()
    url       = "https://github.com/$($env:GH_REPO)/releases/download/$Tag/$assetName"
  }
}

$latestPath = Join-Path $root "latest.json"
if ($platforms.Count) {
  $latest = [ordered]@{
    version   = $version
    notes     = "OWS Hub $Tag - instalador de Windows. Las descargas del navegador requieren esta app."
    pub_date  = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    platforms = $platforms
  }
  [IO.File]::WriteAllText($latestPath, ($latest | ConvertTo-Json -Depth 8))
  $upload += @{ Path = $latestPath; Name = "latest.json" }
  Write-Host "[publish] Manifiesto del updater: $latestPath"
} else {
  if (Test-Path -LiteralPath $latestPath) { Remove-Item -LiteralPath $latestPath -Force }
  Write-Host "[publish] SIN clave de firma: se publica la release sin latest.json." -ForegroundColor Yellow
  Write-Host "[publish] El updater quedara inactivo hasta configurar TAURI_SIGNING_PRIVATE_KEY." -ForegroundColor Yellow
}

Write-Host "[publish] Release $Tag ($($assets.Count) artefactos):"
$assets | ForEach-Object { Write-Host "  - $(Get-AssetName $_.Name) ($([math]::Round($_.Length/1MB,1)) MB)" }

if ($DryRun) { Write-Host "[publish] DryRun: no se subio nada." -ForegroundColor Yellow; exit 0 }

# --- 2) Crear / actualizar la release ---
$owner = $env:GH_REPO.Split("/")[0]
$repo  = $env:GH_REPO.Split("/")[1]
$api   = "https://api.github.com/repos/$owner/$repo"
$hdr   = @{ Authorization = "Bearer $env:GITHUB_TOKEN"; Accept = "application/vnd.github+json"; "User-Agent" = "OWS-Hub-publish" }

$existing = $null
try { $existing = Invoke-RestMethod -Uri "$api/releases/tags/$Tag" -Headers $hdr } catch { }

$notes = @(
  "OWS Hub $Tag",
  "",
  "Instalador de Windows (NSIS) + MSI multi-idioma.",
  "",
  "Esta app es **obligatoria** para continuar con las descargas del ecosistema: en el navegador las descargas estan bloqueadas.",
  "",
  "- [Re releases del Hub](https://github.com/$($env:GH_REPO)/releases)"
)
if (-not $platforms.Count) {
  $notes += @("", "> Publicada sin firma de updater: la descarga del instalador funciona, pero la auto-actualizacion quedara inactiva hasta configurar la clave de firma.")
}

if (-not $existing) {
  $body = @{ tag_name = $Tag; name = "OWS Hub $Tag"; body = ($notes -join "`n"); draft = $false; prerelease = $false } | ConvertTo-Json
  $release = Invoke-RestMethod -Uri "$api/releases" -Method Post -Headers $hdr -ContentType "application/json" -Body ([Text.Encoding]::UTF8.GetBytes($body))
  Write-Host "[publish] Release creada: $($release.html_url)"
} else {
  $release = $existing
  Write-Host "[publish] Release existente: $($release.html_url)"
}

# La plantilla de subida viene en release.upload_url (".../assets{?name,label}").
$uploadUrl = ([string]$release.upload_url) -replace '\{.*\}$', ''

foreach ($item in $upload) {
  $f    = if ($item -is [hashtable]) { $item.Path } else { $item }
  $name = if ($item -is [hashtable]) { $item.Name } else { Get-AssetName (Split-Path -Leaf $f) }
  # Reemplaza el asset anterior si existe (mismo nombre).
  $old = @($release.assets) | Where-Object { $_.name -eq $name } | Select-Object -First 1
  if ($old) { Invoke-RestMethod -Uri $old.url -Method Delete -Headers $hdr -ErrorAction SilentlyContinue | Out-Null }
  Write-Host "[publish] Subiendo $name ($([math]::Round((Get-Item $f).Length/1MB,1)) MB) ..."
  $bytes = [IO.File]::ReadAllBytes($f)
  Invoke-RestMethod -Uri "$uploadUrl`?name=$([Uri]::EscapeDataString($name))" -Method Post `
    -Headers $hdr -ContentType "application/octet-stream" -Body $bytes | Out-Null
}

Write-Host "[publish] OK -> $($release.html_url)" -ForegroundColor Green
if ($platforms.Count) {
  Write-Host "[publish] Updater: https://github.com/$($env:GH_REPO)/releases/latest/download/latest.json"
}