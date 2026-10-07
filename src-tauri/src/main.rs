//! OWS Hub — backend Tauri (biblioteca local estilo launcher).
//! Comandos: carpeta de biblioteca, descarga con progreso,
//! extracción de ZIP, búsqueda del .exe real y lanzamiento.
//!
//! Flujo Wilder Gambit (automático, 1 clic):
//!   download_installer (ZIP real de itch.io vía servidor)
//!   → extract_zip (todo el contenido)
//!   → find_game_exe (detecta el .exe jugable, ignora crash-handler/stockfish)
//!   → launch_game (ejecuta sin que el usuario haga nada más)

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, Manager};

/// %APPDATA%/OWS/library — raíz de juegos instalados.
fn library_root(app: &AppHandle) -> Result<PathBuf, String> {
    let base = app.path().app_data_dir().map_err(|e| e.to_string())?;
    Ok(base.join("OWS").join("library"))
}

#[tauri::command]
async fn get_library_dir(app: AppHandle) -> Result<String, String> {
    let dir = library_root(&app)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.to_string_lossy().to_string())
}

fn sanitize_slug(raw: &str) -> String {
    let clean: String = raw
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let clean = clean.trim_matches('-').to_string();
    if clean.is_empty() {
        "game".to_string()
    } else {
        clean
    }
}

fn sanitize_filename(raw: &str) -> String {
    let base = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("installer.zip")
        .trim();
    let clean: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ' | '(' | ')') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let clean = clean.trim().trim_matches('.').to_string();
    if clean.is_empty() {
        "installer.zip".to_string()
    } else {
        clean
    }
}

/// Extrae filename="..." de Content-Disposition (soporta filename*=UTF-8''...).
fn filename_from_content_disposition(value: Option<&reqwest::header::HeaderValue>) -> Option<String> {
    let v = value?.to_str().ok()?;
    // filename*=UTF-8''Wilder-Gambit-v0.1.0-Windows.zip
    if let Some(idx) = v.find("filename*=") {
        let rest = v[idx + "filename*=".len()..].trim().trim_matches('"').trim();
        // formato: UTF-8''nombre — nos quedamos con lo que hay tras ''
        let name = rest.split("''").last().unwrap_or(rest).trim();
        let name = name.split(';').next().unwrap_or(name).trim().trim_matches('"');
        if !name.is_empty() {
            return Some(sanitize_filename(name));
        }
    }
    if let Some(idx) = v.find("filename=") {
        let rest = v[idx + "filename=".len()..].trim();
        let rest = rest.trim_start_matches('"');
        let end = rest.find(['"', ';']).unwrap_or(rest.len());
        let name = rest[..end].trim();
        if !name.is_empty() {
            return Some(sanitize_filename(name));
        }
    }
    None
}

fn looks_like_installer_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".zip")
        || lower.ends_with(".exe")
        || lower.ends_with(".msi")
        || lower.ends_with(".apk")
        || lower.ends_with(".aab")
        || lower.ends_with(".x86_64")
}

/// Descarga el ZIP real del juego a library/<slug>/ emitiendo
/// eventos `ows-download-progress` { slug, downloaded, total }.
///
/// El endpoint del servidor (`/ows-launch-projects/:slug/download`)
/// ya resuelve itch.io vía ITCH_API_KEY y devuelve el ZIP binario
/// con `Content-Disposition: attachment; filename="*.zip"`.
/// Aquí respetamos ese filename; si la URL es genérica (`.../download`
/// sin extensión) caemos a `<slug>.zip` para que `extract_zip` funcione.
#[tauri::command]
async fn download_installer(app: AppHandle, slug: String, url: String) -> Result<String, String> {
    let slug = sanitize_slug(&slug);
    let dir = library_root(&app)?.join(&slug);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    // Timeouts anti-cuelgue (v3.4.4): sin esto, si el servidor muere a mitad
    // de la descarga (deploy de Render, hibernación), reqwest espera PARA
    // SIEMPRE y el Gestor queda en "Conectando…" al 0% eternamente.
    //   - connect: el servidor tiene 30 s para aceptar la conexión.
    //   - stall: si pasa 60 s sin llegar NI UN byte, se aborta con error
    //     claro (el Gestor lo muestra y ofrece Reintentar).
    // El total NO tiene límite: un juego de 472 MB en una conexión lenta
    // tarda igual, mientras siga llegando algo.
    const CONNECT_TIMEOUT_SECS: u64 = 30;
    const STALL_TIMEOUT_SECS: u64 = 60;
    let res = {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(CONNECT_TIMEOUT_SECS))
            .build()
            .map_err(|e| format!("download client: {e}"))?;
        client
            .get(&url)
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() || e.is_timeout() {
                    format!("el servidor no responde (¿actualizándose? probá en unos segundos)")
                } else {
                    format!("download request: {e}")
                }
            })?
    };
    if !res.status().is_success() {
        return Err(format!("HTTP {}", res.status()));
    }

    let total = res.content_length().unwrap_or(0);
    let content_type = res
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    let disp_name = filename_from_content_disposition(
        res.headers().get(reqwest::header::CONTENT_DISPOSITION),
    );

    let url_name_raw = url
        .rsplit('/')
        .next()
        .unwrap_or("")
        .split('?')
        .next()
        .unwrap_or("")
        .trim();
    let url_name = if looks_like_installer_file(url_name_raw) {
        Some(sanitize_filename(url_name_raw))
    } else {
        None
    };

    let mut filename = disp_name
        .clone()
        .filter(|n| looks_like_installer_file(n))
        .or(url_name)
        .or(disp_name)
        .unwrap_or_else(|| format!("{slug}.zip"));

    // Si el servidor dice que es un zip pero el nombre no tiene extensión,
    // forzamos .zip (caso `.../download` sin Content-Disposition).
    if (content_type.contains("zip") || content_type.contains("octet-stream"))
        && !filename.contains('.')
    {
        filename.push_str(".zip");
    }
    if filename.eq_ignore_ascii_case("download") || filename.eq_ignore_ascii_case("installer.bin") {
        filename = format!("{slug}.zip");
    }

    let dest = dir.join(&filename);

    let mut stream = res.bytes_stream();
    let mut file = tokio::fs::File::create(&dest)
        .await
        .map_err(|e| e.to_string())?;

    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;
    let mut downloaded: u64 = 0;
    loop {
        // Watchdog: cada chunk tiene STALL_TIMEOUT_SECS para llegar. Si el
        // servidor se cuelga a mitad del stream, esto corta en vez de
        // quedarse en "Descargando… 0%" para siempre.
        let chunk = match tokio::time::timeout(
            std::time::Duration::from_secs(STALL_TIMEOUT_SECS),
            stream.next(),
        )
        .await
        {
            Ok(Some(Ok(chunk))) => chunk,
            Ok(Some(Err(e))) => return Err(format!("download chunk: {e}")),
            Ok(None) => break,
            Err(_) => {
                drop(file);
                let _ = std::fs::remove_file(&dest);
                return Err("el servidor dejó de enviar datos a mitad de la descarga (¿se está actualizando?). Reintentá en unos segundos".to_string());
            }
        };
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;
        let _ = app.emit(
            "ows-download-progress",
            serde_json::json!({ "slug": slug, "downloaded": downloaded, "total": total }),
        );
    }
    file.flush().await.map_err(|e| e.to_string())?;
    drop(file);

    // ── Validación: debe ser un ZIP real, no una página HTML de error ──
    let meta = std::fs::metadata(&dest).map_err(|e| e.to_string())?;
    if meta.len() == 0 {
        let _ = std::fs::remove_file(&dest);
        return Err("descarga vacía (0 bytes)".to_string());
    }
    let mut head = [0u8; 4];
    {
        use std::io::Read;
        let mut f = std::fs::File::open(&dest).map_err(|e| e.to_string())?;
        let n = f.read(&mut head).map_err(|e| e.to_string())?;
        if n < 4 || &head[0..2] != b"PK" {
            let _ = std::fs::remove_file(&dest);
            if content_type.contains("text/html") || (n >= 1 && head[0] == b'<') {
                return Err(
                    "el servidor devolvió HTML en vez del ZIP (¿juego sin instalador o itch restringido?)"
                        .to_string(),
                );
            }
            return Err("el archivo descargado no es un ZIP válido".to_string());
        }
    }

    Ok(dest.to_string_lossy().to_string())
}

/// Extrae un ZIP a la carpeta destino (protegido contra zip-slip).
#[tauri::command]
fn extract_zip(zip_path: String, dest_dir: String) -> Result<String, String> {
    let file = std::fs::File::open(&zip_path)
        .map_err(|e| format!("no se pudo abrir el ZIP: {e}"))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("ZIP inválido (¿descarga incompleta?): {e}"))?;
    std::fs::create_dir_all(&dest_dir).map_err(|e| e.to_string())?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let out = match entry.enclosed_name() {
            Some(p) => PathBuf::from(&dest_dir).join(p),
            None => continue,
        };
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
        } else {
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut out_file = std::fs::File::create(&out).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut out_file).map_err(|e| e.to_string())?;
        }
    }
    Ok(dest_dir)
}

fn exe_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

fn is_ignored_exe(path: &Path) -> bool {
    let lower = path.to_string_lossy().to_lowercase();
    // Crash-handler de Unity, desinstaladores y binarios internos no son el juego.
    if lower.contains("unitycrashhandler")
        || lower.contains("crashhandler")
        || lower.contains("unins")
        || lower.contains("uninstall")
    {
        return true;
    }
    // Motor de ajedrez interno de Wilder Gambit y plugins: nunca lanzar.
    if lower.contains("streamingassets") || lower.contains("stockfish") || lower.contains("monobleedingedge") {
        return true;
    }
    // Carpetas internas de Unity
    if lower.contains("_data/") || lower.contains("_data\\") {
        return true;
    }
    false
}

fn has_data_sibling(exe: &Path) -> bool {
    let stem = exe_stem(exe);
    if stem.is_empty() {
        return false;
    }
    match exe.parent() {
        Some(parent) => parent.join(format!("{stem}_Data")).is_dir(),
        None => false,
    }
}

fn exe_file_size(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

/// Busca el .exe jugable dentro de la carpeta extraída.
///
/// Prioridad:
///  1. `preferred` exacto (ej. "Wilder Gambit.exe") si existe.
///  2. .exe en la raíz (sin crash-handler) con carpeta `<nombre>_Data` al lado (caso Unity).
///  3. .exe en raíz más grande (descarta uninstallers pequeños).
///  4. Búsqueda recursiva (máx. 4 niveles, salta StreamingAssets) con la misma prioridad.
#[tauri::command]
fn find_game_exe(dest_dir: String, preferred: Option<String>) -> Result<String, String> {
    let root = PathBuf::from(&dest_dir);
    if !root.is_dir() {
        return Err(format!("no existe la carpeta instalada: {dest_dir}"));
    }

    // 1) Preferido exacto
    if let Some(pref) = preferred {
        let pref = pref.trim();
        if !pref.is_empty() {
            let candidate = root.join(pref);
            if candidate.is_file() && !is_ignored_exe(&candidate) {
                return Ok(candidate.to_string_lossy().to_string());
            }
            // Intento insensible a mayúsculas en la raíz
            if let Ok(entries) = std::fs::read_dir(&root) {
                let want = pref.to_lowercase();
                for e in entries.flatten() {
                    let p = e.path();
                    if p.is_file()
                        && p.extension().map(|x| x.eq_ignore_ascii_case("exe")).unwrap_or(false)
                        && p.file_name()
                            .and_then(|n| n.to_str())
                            .map(|n| n.to_lowercase() == want)
                            .unwrap_or(false)
                    {
                        return Ok(p.to_string_lossy().to_string());
                    }
                }
            }
        }
    }

    // 2) y 3) .exe en la raíz
    let mut root_exes: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&root) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_file()
                && p.extension().map(|x| x.eq_ignore_ascii_case("exe")).unwrap_or(false)
                && !is_ignored_exe(&p)
            {
                root_exes.push(p);
            }
        }
    }
    if root_exes.len() == 1 {
        return Ok(root_exes[0].to_string_lossy().to_string());
    }
    if !root_exes.is_empty() {
        // Prefiere el que tenga _Data al lado (build Unity real)
        let mut with_data: Vec<&PathBuf> = root_exes.iter().filter(|p| has_data_sibling(p)).collect();
        if with_data.len() == 1 {
            return Ok(with_data[0].to_string_lossy().to_string());
        }
        if !with_data.is_empty() {
            with_data.sort_by_key(|p| std::cmp::Reverse(exe_file_size(p)));
            return Ok(with_data[0].to_string_lossy().to_string());
        }
        // Sin _Data: el más grande (evita uninstallers / helpers pequeños)
        root_exes.sort_by_key(|p| std::cmp::Reverse(exe_file_size(p)));
        return Ok(root_exes[0].to_string_lossy().to_string());
    }

    // 4) Recursivo (carpetas con nombre de versión, builds anidados, etc.)
    let mut stack: Vec<(PathBuf, u8)> = vec![(root.clone(), 0)];
    let mut found: Vec<(PathBuf, u8)> = Vec::new();
    while let Some((dir, depth)) = stack.pop() {
        if depth > 4 {
            continue;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_lowercase();
                if name == "streamingassets"
                    || name == "__macosx"
                    || name.starts_with('.')
                    || name == "monobleedingedge"
                {
                    continue;
                }
                stack.push((p, depth + 1));
            } else if p.is_file()
                && p.extension().map(|x| x.eq_ignore_ascii_case("exe")).unwrap_or(false)
                && !is_ignored_exe(&p)
            {
                found.push((p, depth));
            }
        }
    }
    if found.is_empty() {
        return Err("no se encontró ningún .exe del juego en la carpeta extraída".to_string());
    }
    // Prefiere con _Data, luego menor profundidad, luego mayor tamaño
    found.sort_by(|a, b| {
        let da = has_data_sibling(&a.0);
        let db = has_data_sibling(&b.0);
        db.cmp(&da)
            .then(a.1.cmp(&b.1))
            .then(exe_file_size(&b.0).cmp(&exe_file_size(&a.0)))
    });
    Ok(found[0].0.to_string_lossy().to_string())
}

/// Lanza el .exe del juego (cwd = carpeta del exe, como un launcher).
#[tauri::command]
fn launch_game(exe_path: String) -> Result<(), String> {
    let exe = Path::new(&exe_path);
    if !exe.is_file() {
        return Err(format!("no existe el ejecutable: {exe_path}"));
    }
    let cwd = exe.parent().unwrap_or(Path::new("."));
    std::process::Command::new(exe)
        .current_dir(cwd)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ═══════════════════════════════════════════════════════
// DESINSTALAR
//
// Borrar desde el frontend es la operación más peligrosa del Hub: `dir` viene
// del WebView y podría ser cualquier ruta del disco. Por eso solo se acepta
// una carpeta que sea HIJA DIRECTA de una raíz de biblioteca conocida (la
// por defecto %APPDATA%/OWS/library o la personalizada en el setup), nunca la
// raíz ni una carpeta fuera, y nunca a través de un enlace simbólico.
// ═══════════════════════════════════════════════════════

/// Tamaño total en disco de una carpeta (bytes). No sigue enlaces simbólicos.
fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            match e.file_type() {
                Ok(ft) if ft.is_symlink() => continue, // nunca atravesar un link
                Ok(ft) if ft.is_dir() => stack.push(p),
                Ok(_) => {
                    if let Ok(m) = e.metadata() {
                        total += m.len();
                    }
                }
                Err(_) => continue,
            }
        }
    }
    total
}

/// Resuelve la ruta real (absoluta, con symlinks resueltos) para comparar.
fn real_path(p: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(p).map_err(|e| format!("no se pudo resolver {}: {e}", p.display()))
}

/// Valida `target` contra la lista de raíces ya canonicalizadas.
/// Solo acepta hijas directas (una carpeta = un juego).
fn resolve_game_dir(target: &Path, roots: &[PathBuf]) -> Result<PathBuf, String> {
    for root in roots {
        if target.parent() != Some(root.as_path()) {
            continue;
        }
        if !target.starts_with(root) {
            continue;
        }
        if !target.is_dir() {
            return Err("esa carpeta de juego ya no existe".to_string());
        }
        return Ok(target.to_path_buf());
    }

    let mut permitidas: Vec<String> = roots.iter().map(|r| r.to_string_lossy().to_string()).collect();
    permitidas.dedup();
    Err(format!(
        "La carpeta '{}' no esta dentro de una biblioteca del Hub (bibliotecas: {}).",
        target.display(),
        permitidas.join(" | ")
    ))
}

/// Devuelve la carpeta a borrar solo si es una carpeta de juego legítima.
/// `library_bases` son las carpetas que el frontend declara como biblioteca
/// (la personalizada del setup y/o la detectada); todas se tratan igual.
fn safe_game_dir(
    app: &AppHandle,
    dir: &str,
    library_bases: Option<&[String]>,
) -> Result<PathBuf, String> {
    let raw = dir.trim();
    if raw.is_empty() {
        return Err("no se indico que juego desinstalar".to_string());
    }
    let target = real_path(Path::new(raw))?;

    let mut roots: Vec<PathBuf> = Vec::new();
    let default_root = library_root(app)?;
    if default_root.exists() {
        roots.push(real_path(&default_root)?);
    }
    for base in library_bases.unwrap_or(&[]) {
        let b = PathBuf::from(base.trim());
        if b.is_dir() {
            roots.push(real_path(&b)?);
        }
    }
    // Dedup por ruta canonica (la detectada y la guardada suelen coincidir).
    let mut uniq: Vec<PathBuf> = Vec::new();
    for r in roots {
        if !uniq.contains(&r) {
            uniq.push(r);
        }
    }
    if uniq.is_empty() {
        return Err("no se pudo resolver la biblioteca de juegos del Hub".to_string());
    }

    resolve_game_dir(&target, &uniq)
}

/// Tamaño en disco de la carpeta de un juego (bytes). 0 si no existe.
#[tauri::command]
fn game_dir_size(dir: String) -> Result<u64, String> {
    let p = PathBuf::from(dir.trim());
    if !p.is_dir() {
        return Ok(0);
    }
    Ok(dir_size(&p))
}

/// Quita el atributo "solo lectura" de todo el árbol.
///
/// OJO: en Windows `std::fs::Permissions::set_readonly()` SOLO cambia el valor
/// en memoria (modifica un bit de un struct y no llama al sistema), así que no
/// sirve para nada aquí. Hay que hablar con Windows de verdad: `attrib` forma
/// parte del sistema y quita el atributo recursivamente.
fn clear_readonly(path: &Path) {
    let target = path.display().to_string();
    // El directorio y todo lo que hay debajo, archivos y carpetas.
    let _ = std::process::Command::new("attrib")
        .arg("-R")
        .arg(&target)
        .arg("/S")
        .arg("/D")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    // `attrib` con comodín cubre el contenido; esto cubre la propia raíz.
    let _ = std::process::Command::new("attrib")
        .arg("-R")
        .arg(format!("{target}\\*"))
        .arg("/S")
        .arg("/D")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// ¿Hay un .exe de esta carpeta en ejecución?
///
/// Truco sin dependencias: un ejecutable cargado por Windows no se puede abrir
/// para escritura. Un archivo solo lectura también falla, así que se descarta
/// ese caso para no dar falsos positivos.
fn is_game_running(dir: &Path) -> bool {
    let mut stack = vec![(dir.to_path_buf(), 0u8)];
    let mut checked = 0usize;
    while let Some((d, depth)) = stack.pop() {
        if depth > 2 {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            match e.file_type() {
                Ok(ft) if ft.is_symlink() => continue,
                Ok(ft) if ft.is_dir() => stack.push((p, depth + 1)),
                Ok(_) => {
                    let is_exe = p
                        .extension()
                        .map(|x| x.eq_ignore_ascii_case("exe"))
                        .unwrap_or(false);
                    if !is_exe {
                        continue;
                    }
                    checked += 1;
                    if checked > 40 {
                        return false; // no escanear un árbol enorme
                    }
                    let Ok(meta) = std::fs::metadata(&p) else { continue };
                    if meta.permissions().readonly() {
                        continue;
                    }
                    if std::fs::OpenOptions::new().write(true).open(&p).is_err() {
                        return true;
                    }
                }
                Err(_) => continue,
            }
        }
    }
    false
}

/// Traduce el error de Windows a algo que el usuario entienda.
/// Se mira `raw_os_error()` (numero) y NO el texto: en Windows el mensaje sale
/// en el idioma del sistema ("Acceso denegado", "Access denied"...).
fn friendly_uninstall_error(e: &std::io::Error, running: bool) -> String {
    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const ERROR_LOCK_VIOLATION: i32 = 33;
    match e.raw_os_error() {
        Some(ERROR_ACCESS_DENIED) => {
            if running {
                "No se pudo borrar: el juego sigue abierto. Ciérralo (y su lanzador) y reinténtalo.".to_string()
            } else {
                "No se pudo borrar: Windows denegó el acceso. Cierra el juego y el explorador de archivos, y reinténtalo.".to_string()
            }
        }
        Some(ERROR_SHARING_VIOLATION) | Some(ERROR_LOCK_VIOLATION) => {
            "No se pudo borrar: hay un archivo del juego en uso. Cierra el juego y reinténtalo.".to_string()
        }
        _ => {
            let code = e.raw_os_error().map(|c| c.to_string()).unwrap_or_default();
            if code.is_empty() {
                format!("No se pudo borrar la carpeta del juego: {e}")
            } else {
                format!("No se pudo borrar la carpeta del juego: {e} (codigo {code})")
            }
        }
    }
}

/// Sufijo de las carpetas apartadas para borrado diferido.
const PENDING_SUFFIX: &str = ".__borrando__";

/// Al arrancar el Hub, limpia lo que quedó a medias en una desinstalación
/// anterior (el juego ya salía de la biblioteca, pero la carpeta no se pudo
/// borrar porque estaba en uso). Se ejecuta con la app cerrada, así que ya no
/// hay Handles: el borrado sí funciona.
#[tauri::command]
fn sweep_pending_removals(app: AppHandle) -> Result<usize, String> {
    let mut pending: Vec<PathBuf> = Vec::new();
    let mut roots: Vec<PathBuf> = vec![library_root(&app)?];

    for root in roots.iter_mut() {
        let Ok(entries) = std::fs::read_dir(&root) else { continue };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with(PENDING_SUFFIX) {
                pending.push(e.path());
            }
        }
    }

    let mut done = 0usize;
    for p in pending {
        clear_readonly(&p);
        if std::fs::remove_dir_all(&p).is_ok() {
            done += 1;
        }
    }
    Ok(done)
}

/// Borra la carpeta de un juego instalado y devuelve lo que quedó libre.
///
/// Borrado en dos fases porque en Windows un archivo abierto (el juego en
/// marcha, un antivirus, el explorador) bloquea `remove_dir_all` con
/// ERROR_ACCESS_DENIED y el juego queda "a medias":
///   1) reintentos con `clear_readonly` + espera.
///   2) si no se puede, se aparta la carpeta a `<slug>.__borrando__`; así el
///      juego desaparece de la biblioteca al instante y `sweep_pending_removals`
///      la borra de verdad al siguiente arranque (con la app ya cerrada).
#[tauri::command]
async fn uninstall_game(
    app: AppHandle,
    dir: String,
    library_bases: Option<Vec<String>>,
) -> Result<serde_json::Value, String> {
    let target = safe_game_dir(&app, &dir, library_bases.as_deref())?;
    let bytes = dir_size(&target);
    let name = target
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    let _ = app.emit(
        "ows-uninstall-progress",
        serde_json::json!({ "phase": "removing", "dir": target.to_string_lossy(), "bytes": bytes }),
    );

    let to_remove = target.clone();
    let outcome: Result<(bool, String), String> =
        tauri::async_runtime::spawn_blocking(move || {
            // Los ZIP de juego traen archivos en solo lectura y otros procesos
            // pueden tenerlos abiertos: se limpia el atributo antes de intentar.
            clear_readonly(&to_remove);
            let mut last: Option<std::io::Error> = None;

            for attempt in 0..5u64 {
                match std::fs::remove_dir_all(&to_remove) {
                    Ok(()) => return Ok((true, String::new())),
                    Err(e) => {
                        last = Some(e);
                        std::thread::sleep(std::time::Duration::from_millis(250 * (attempt + 1)));
                        clear_readonly(&to_remove);
                    }
                }
            }

            // Fase 2: apartar la carpeta. Es un rename, que Windows suele
            // permitir aunque el contenido esté en uso.
            let parked = PathBuf::from(format!(
                "{}{}",
                to_remove.to_string_lossy(),
                PENDING_SUFFIX
            ));
            if std::fs::rename(&to_remove, &parked).is_ok() {
                return Ok((false, parked.to_string_lossy().to_string()));
            }

            // Ni borrar ni renombrar: hay un bloqueo real.
            let err = last.unwrap_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::Other, "borrado fallido")
            });
            Err(friendly_uninstall_error(&err, is_game_running(&to_remove)))
        })
        .await
        .map_err(|e| e.to_string())?;

    let (borrado, pendiente) = outcome?;
    let _ = app.emit(
        "ows-uninstall-progress",
        serde_json::json!({ "phase": "done", "dir": name }),
    );
    Ok(serde_json::json!({
        "dir": name,
        "bytes": bytes,
        "deleted": borrado,
        "pending": pendiente,
    }))
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            get_library_dir,
            download_installer,
            extract_zip,
            find_game_exe,
            launch_game,
            game_dir_size,
            uninstall_game,
            sweep_pending_removals
        ])
        .run(tauri::generate_context!())
        .expect("error while running OWS Hub");
}

#[cfg(test)]
mod tests {
    use super::*;

    // tmp/ows-hub-test/{root/juego/sub, otra-raiz/juego2}. Se construye una
    // sola vez (los tests corren en paralelo: nada de remove_dir_all aquí).
    fn make_tree() -> PathBuf {
        static ONCE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        ONCE.get_or_init(|| {
            let base = std::env::temp_dir().join("ows-hub-test");
            let root = base.join("root");
            let game = root.join("juego");
            let _ = std::fs::create_dir_all(game.join("sub"));
            let _ = std::fs::write(game.join("sub").join("data.bin"), vec![0u8; 2048]);
            let _ = std::fs::write(root.join("sibling.txt"), b"x");
            let _ = std::fs::create_dir_all(base.join("otra-raiz").join("juego2"));
            std::fs::canonicalize(&base).unwrap()
        })
        .clone()
    }

    #[test]
    fn acepta_hija_directa_de_la_biblioteca() {
        let base = make_tree();
        let roots = vec![std::fs::canonicalize(base.join("root")).unwrap()];
        let game = std::fs::canonicalize(base.join("root").join("juego")).unwrap();
        assert!(resolve_game_dir(&game, &roots).is_ok());
    }

    #[test]
    fn acepta_una_de_varias_roots() {
        let base = make_tree();
        let roots = vec![
            std::fs::canonicalize(base.join("root")).unwrap(),
            std::fs::canonicalize(base.join("otra-raiz")).unwrap(),
        ];
        let game = std::fs::canonicalize(base.join("otra-raiz").join("juego2")).unwrap();
        assert!(resolve_game_dir(&game, &roots).is_ok());
    }

    #[test]
    fn rechaza_la_raiz_misma() {
        let base = make_tree();
        let root = std::fs::canonicalize(base.join("root")).unwrap();
        assert!(resolve_game_dir(&root, &[root.clone()]).is_err());
    }

    #[test]
    fn rechaza_nietos_y_hermanos() {
        let base = make_tree();
        let roots = vec![std::fs::canonicalize(base.join("root")).unwrap()];
        // nieto: root/juego/sub  -> no es hijo directo
        let sub = std::fs::canonicalize(base.join("root").join("juego").join("sub")).unwrap();
        assert!(resolve_game_dir(&sub, &roots).is_err());
        // hermano: base/otra-raiz -> fuera de la raiz
        let fuera = std::fs::canonicalize(base.join("otra-raiz")).unwrap();
        assert!(resolve_game_dir(&fuera, &roots).is_err());
    }

    #[test]
    fn dir_size_cuenta_los_archivos() {
        let base = make_tree();
        let game = base.join("root").join("juego");
        assert_eq!(dir_size(&game), 2048);
    }

    #[test]
    fn clear_readonly_habilita_el_borrado() {
        // Contrato real de clear_readonly: quitar el atributo de verdad.
        // (En Windows moderno borrar un archivo solo lectura suele funcionar
        // igual, asi que no se exige que remove_dir_all falle antes.)
        let base = std::env::temp_dir().join("ows-hub-test-ro");
        let game = base.join("game");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(game.join("data")).unwrap();
        let file = game.join("data").join("asset.bin");
        std::fs::write(&file, vec![7u8; 4096]).unwrap();

        // Marcar en solo lectura de verdad (attrib, no Permissions::set_readonly,
        // que en Windows solo cambia el valor en memoria).
        let st = std::process::Command::new("attrib")
            .arg("+R")
            .arg(file.display().to_string())
            .stdout(std::process::Stdio::null())
            .status();
        if !st.map(|s| s.success()).unwrap_or(false) {
            return; // entorno sin attrib: nada que verificar
        }
        assert!(
            std::fs::metadata(&file).unwrap().permissions().readonly(),
            "attrib +R deberia marcar el archivo como solo lectura"
        );

        clear_readonly(&game);
        assert!(
            !std::fs::metadata(&file).unwrap().permissions().readonly(),
            "clear_readonly debe quitar el atributo de verdad"
        );
        let _ = std::fs::remove_dir_all(&game);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn carpeta_apartada_para_borrado_diferido() {
        // Fase 2: si no se puede borrar, la carpeta se aparta con el sufijo
        // y sweep_pending_removals la limpia al siguiente arranque.
        let base = std::env::temp_dir().join("ows-hub-test-park");
        let root = base.join("library");
        let game = root.join("juego");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(game.join("data")).unwrap();
        std::fs::write(game.join("data").join("a.bin"), vec![1u8; 128]).unwrap();

        let parked = PathBuf::from(format!(
            "{}{}",
            game.to_string_lossy(),
            PENDING_SUFFIX
        ));
        std::fs::rename(&game, &parked).unwrap();
        assert!(!game.exists(), "la carpeta original debe desaparecer ya");
        assert!(parked.exists());
        assert!(parked.to_string_lossy().ends_with(PENDING_SUFFIX));

        // El sweep la borra (logica aislada de safe_game_dir para poder testear).
        clear_readonly(&parked);
        assert!(std::fs::remove_dir_all(&parked).is_ok());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn friendly_uninstall_error_traduce_por_codigo_no_por_texto() {
        // El texto sale en el idioma del sistema: hay que mirar el codigo.
        let denied = std::io::Error::from_raw_os_error(5);
        let running = friendly_uninstall_error(&denied, true);
        assert!(running.contains("abierto"), "esperado mensaje de juego abierto: {running}");

        let sharing = std::io::Error::from_raw_os_error(32);
        assert!(friendly_uninstall_error(&sharing, false).contains("en uso"));

        let otro = std::io::Error::from_raw_os_error(3);
        assert!(friendly_uninstall_error(&otro, false).contains("codigo 3"));
    }
}