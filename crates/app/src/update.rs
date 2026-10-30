//! Mises à jour depuis les Releases GitHub, sur le modèle de Ronnie : une vérification en
//! arrière-plan trouve une version plus récente ; acceptée, le fichier de la plateforme est
//! téléchargé, comparé à l'empreinte SHA-256 publiée par GitHub, puis installé :
//! macOS : le .pkg, par l'installeur système (mot de passe administrateur demandé une fois) ;
//! Windows : l'exe remplace celui en cours ; Linux : l'AppImage remplace celle en cours.
//! L'app propose ensuite de redémarrer.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const REPO: &str = "tharos-devs/AudioPlayer";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
// Le fichier fait quelques dizaines de Mo : au-delà, ce n'est pas le bon.
const MAX_DOWNLOAD: u64 = 300 * 1024 * 1024;
#[cfg(target_os = "macos")]
const INSTALLED_APP: &str = "/Applications/AudioPlayer.app";

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    assets: Vec<Asset>,
}

#[derive(Clone, Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    /// « sha256:<hex> », calculé par GitHub au dépôt du fichier.
    #[serde(default)]
    digest: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Available {
    pub version: String,
    /// Page de la version, avec ses notes.
    pub url: String,
    // `None` : rien d'installable ici (Linux hors AppImage) ; la page reste proposée.
    asset: Option<Asset>,
}

#[derive(Clone, Debug, Default)]
pub enum State {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(Available),
    /// Téléchargement : octets reçus et total (0 : inconnu).
    Downloading(u64, u64),
    Installing,
    Installed(String),
    Failed(String),
}

/// Partagé avec les fils qui font le travail réseau ; l'interface lit `state()`.
#[derive(Clone, Default)]
pub struct Updater {
    state: Arc<Mutex<State>>,
    // Dernière version proposée : « Retry » après un échec de téléchargement ou
    // d'installation la réinstalle au lieu de revérifier.
    offered: Arc<Mutex<Option<Available>>>,
    // Mise à jour installée mais fenêtre fermée (« Later ») : elle reste installée et
    // réapparaît à la prochaine vérification demandée.
    hidden: Arc<std::sync::atomic::AtomicBool>,
}

impl Updater {
    pub fn state(&self) -> State {
        self.state.lock().unwrap().clone()
    }

    fn set(&self, state: State) {
        *self.state.lock().unwrap() = state;
    }

    pub fn dismiss(&self) {
        match self.state() {
            State::Checking | State::Downloading(..) | State::Installing => {}
            State::Installed(_) => self.hidden.store(true, std::sync::atomic::Ordering::Relaxed),
            _ => self.set(State::Idle),
        }
    }

    pub fn hidden(&self) -> bool {
        self.hidden.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Erreur venue d'ailleurs (redémarrage impossible…), affichée par la fenêtre.
    pub fn fail(&self, message: String) {
        self.set(State::Failed(message));
    }

    /// Cherche une version plus récente en arrière-plan. Déjà installée : elle est remontrée.
    pub fn check(&self) {
        if matches!(self.state(), State::Installed(_)) {
            self.hidden.store(false, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        if matches!(self.state(), State::Checking | State::Downloading(..) | State::Installing) {
            return;
        }
        // Une nouvelle vérification remplace la version proposée : « Retry » ne doit pas
        // réinstaller une offre devenue ancienne.
        *self.offered.lock().unwrap() = None;
        self.set(State::Checking);
        let updater = self.clone();
        std::thread::spawn(move || {
            updater.set(match latest() {
                Ok(Some(available)) => State::Available(available),
                Ok(None) => State::UpToDate,
                Err(error) => State::Failed(error),
            });
        });
    }

    /// Après un échec : réinstalle la version proposée, sinon revérifie.
    pub fn retry(&self) {
        let offered = self.offered.lock().unwrap().clone();
        match offered {
            Some(available) => self.install(available),
            None => {
                self.set(State::Idle);
                self.check();
            }
        }
    }

    /// Télécharge et installe `available` en arrière-plan.
    pub fn install(&self, available: Available) {
        let Some(asset) = available.asset.clone() else {
            return self.set(State::Failed("no file to install for this system".into()));
        };
        *self.offered.lock().unwrap() = Some(available.clone());
        self.set(State::Downloading(0, 0));
        let updater = self.clone();
        std::thread::spawn(move || {
            let result = download(&asset, &updater).and_then(|bytes| {
                updater.set(State::Installing);
                install(&asset, &bytes)
            });
            if result.is_ok() {
                *updater.offered.lock().unwrap() = None;
            }
            updater.set(match result {
                Ok(()) => State::Installed(available.version),
                Err(error) => State::Failed(error),
            });
        });
    }
}

impl Available {
    pub fn installable(&self) -> bool {
        self.asset.is_some()
    }
}

fn agent(total: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(total))
        .build()
        .into()
}

const USER_AGENT: &str = concat!("AudioPlayer/", env!("CARGO_PKG_VERSION"));

/// La dernière version publiée, si elle est plus récente que celle-ci.
fn latest() -> Result<Option<Available>, String> {
    let response = agent(Duration::from_secs(30))
        .get(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", USER_AGENT)
        .call();
    let release: Release = match response {
        // Aucune version publiée : rien de plus récent.
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        other => other.map_err(|error| format!("GitHub unreachable: {error}"))?,
    }
    .body_mut()
        .read_json()
        .map_err(|error| error.to_string())?;
    let version = release.tag_name.trim_start_matches('v').to_owned();
    if !is_newer(&version, VERSION) {
        return Ok(None);
    }
    let asset = platform_suffix().and_then(|suffix| release.assets.into_iter().find(|a| a.name.ends_with(suffix)));
    // Sans fichier pour ce système (publication incomplète), la page de la version reste proposée.
    Ok(Some(Available { version, url: release.html_url, asset }))
}

/// Fin du nom du fichier de la Release pour ce système ; `None` si l'app ne peut pas se
/// remplacer elle-même (Linux hors AppImage : lancée depuis les sources ou un paquet).
fn platform_suffix() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some(".pkg")
    } else if cfg!(windows) {
        Some("-x64.exe")
    } else {
        appimage().map(|_| "-x86_64.AppImage")
    }
}

/// Le fichier AppImage dont ce processus est lancé : son environnement le donne.
fn appimage() -> Option<PathBuf> {
    let (file, dir) = (std::env::var_os("APPIMAGE")?, std::env::var_os("APPDIR")?);
    let exe = std::env::current_exe().ok()?;
    exe.starts_with(&dir).then(|| PathBuf::from(file))
}

/// (majeur, mineur, correctif, pré-version telle que « 1.2.0-beta »).
fn parse_version(v: &str) -> Option<(u64, u64, u64, bool)> {
    let (core, pre) = match v.split_once(['-', '+']) {
        Some((core, rest)) => (core, v.as_bytes()[core.len()] == b'-' && !rest.is_empty()),
        None => (v, false),
    };
    let mut parts = core.split('.').map(str::parse::<u64>);
    Some((parts.next()?.ok()?, parts.next()?.ok()?, parts.next()?.ok()?, pre))
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        // Mêmes numéros : la version finale est plus récente que ses pré-versions.
        (Some((a, b, c, pa)), Some((x, y, z, px))) => (a, b, c) > (x, y, z) || ((a, b, c) == (x, y, z) && px && !pa),
        _ => false,
    }
}

fn download(asset: &Asset, updater: &Updater) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    // Sans l'empreinte de GitHub, le fichier ne peut pas être vérifié : on ne l'installe pas.
    let expected = asset
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"))
        .ok_or("the release publishes no SHA-256 digest for this file")?
        .to_owned();
    let mut response = agent(Duration::from_secs(900))
        .get(&asset.browser_download_url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|error| format!("download failed: {error}"))?;
    let total: u64 = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut reader = response.body_mut().with_config().limit(MAX_DOWNLOAD).reader();
    let mut bytes = Vec::with_capacity(total.min(MAX_DOWNLOAD) as usize);
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buffer).map_err(|error| format!("download failed: {error}"))?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..n]);
        updater.set(State::Downloading(bytes.len() as u64, total));
    }
    let actual: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    if !actual.eq_ignore_ascii_case(&expected) {
        return Err("the downloaded file is corrupted (SHA-256 mismatch)".into());
    }
    Ok(bytes)
}

#[cfg(target_os = "macos")]
fn install(asset: &Asset, bytes: &[u8]) -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("audioplayer-update-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let pkg = dir.join(&asset.name);
    let result = (|| {
        std::fs::write(&pkg, bytes).map_err(|e| e.to_string())?;
        // L'installeur système, avec la fenêtre de mot de passe administrateur de macOS.
        let script = format!(
            "do shell script \"/usr/sbin/installer -pkg \" & quoted form of \"{}\" & \" -target /\" with administrator privileges",
            pkg.display().to_string().replace('\\', "\\\\").replace('"', "\\\"")
        );
        let output = Command::new("/usr/bin/osascript").args(["-e", &script]).output().map_err(|e| e.to_string())?;
        if output.status.success() {
            return Ok(());
        }
        let message = String::from_utf8_lossy(&output.stderr);
        // -128 : « Annuler » dans la fenêtre du mot de passe.
        Err(if message.contains("-128") { "installation cancelled".into() } else { message.trim().to_owned() })
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

#[cfg(windows)]
fn install(_: &Asset, bytes: &[u8]) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let new = exe.with_file_name(format!(".AudioPlayer-update-{}.exe", std::process::id()));
    std::fs::write(&new, bytes).map_err(|e| format!("cannot write next to {}: {e}", exe.display()))?;
    let result = self_replace::self_replace(&new).map_err(|e| format!("could not replace the executable: {e}"));
    let _ = std::fs::remove_file(&new);
    result
}

/// La nouvelle AppImage est écrite à côté puis renommée par-dessus : l'app en cours garde
/// l'ancienne (encore montée) jusqu'à sa fermeture.
#[cfg(all(unix, not(target_os = "macos")))]
fn install(_: &Asset, bytes: &[u8]) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let file = appimage().ok_or("not running from an AppImage")?;
    let dir = file.parent().ok_or("unknown AppImage folder")?;
    let temp = dir.join(format!(".AudioPlayer-update-{}", std::process::id()));
    let result = (|| {
        std::fs::write(&temp, bytes).map_err(|e| format!("cannot write in {}: {e}", dir.display()))?;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        std::fs::rename(&temp, &file).map_err(|e| format!("cannot replace {}: {e}", file.display()))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Relance l'app mise à jour une fois celle-ci fermée ; l'appelant ferme ensuite l'app.
pub fn relaunch() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    return start_after_exit(Path::new("/usr/bin/open"), &["-n", INSTALLED_APP]);
    #[cfg(not(target_os = "macos"))]
    {
        let program = appimage().or_else(|| std::env::current_exe().ok()).ok_or("unknown executable")?;
        start_after_exit(&program, &[])
    }
}

/// Lancée tant que cette instance tourne encore, la nouvelle risquerait d'être prise pour
/// elle (macOS ramène simplement l'app au premier plan) : on attend sa fin (10 s au plus).
#[cfg(unix)]
fn start_after_exit(program: &Path, args: &[&str]) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    const SCRIPT: &str = r#"pid=$1; shift; i=0
while kill -0 "$pid" 2>/dev/null && [ "$i" -lt 100 ]; do sleep 0.1; i=$((i + 1)); done
exec "$@""#;
    Command::new("/bin/sh")
        .args(["-c", SCRIPT, "sh", &std::process::id().to_string()])
        .arg(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("cannot restart: {e}"))
}

#[cfg(windows)]
fn start_after_exit(program: &Path, _: &[&str]) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let script = format!(
        // Relancée seulement une fois l'instance actuelle fermée (60 s au plus), jamais à côté
        // d'elle : deux instances ouvriraient la même base.
        "$p = {0}; Wait-Process -Id $p -Timeout 60 -ErrorAction SilentlyContinue; \
         if (-not (Get-Process -Id $p -ErrorAction SilentlyContinue)) {{ Start-Process -FilePath '{1}' }}",
        std::process::id(),
        program.display().to_string().replace('\'', "''")
    );
    Command::new("powershell")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("cannot restart: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.3.0", "0.2.0"));
        assert!(is_newer("1.0.0", "0.10.0"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("garbage", "0.2.0"));
        assert!(is_newer("0.3.0", "0.3.0-beta.1"), "final after its pre-release");
        assert!(!is_newer("0.3.0-beta.1", "0.3.0"));
    }
}
