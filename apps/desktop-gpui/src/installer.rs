//! [INPUT]: 发布清单、签名密钥、更新归档和现有应用。
//! [OUTPUT]: 验签下载、补丁重建和平台安装。
//! [POS]: 应用进程准备更新，macOS 安装进程执行替换。
//! [PROTOCOL]: Update this header on change, then check AGENTS.md

use std::{
    collections::HashMap,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    time::Duration,
};

use base64::Engine as _;
use futures_util::StreamExt as _;
use minisign_verify::{PublicKey, Signature, StreamVerifier};
use semver::Version;
use serde::Deserialize;

pub(crate) const RELEASES_PAGE_URL: &str = "https://github.com/joeseesun/capwise/releases";
const UPDATE_MANIFEST_URL: &str =
    "https://github.com/joeseesun/capwise/releases/latest/download/latest.json";
/// The minisign public key every update must be signed with. The private half
/// never enters the repository; see docs/release.md. An empty file means this
/// build was made before the key existed, so it never looks for updates.
const UPDATER_PUBLIC_KEY: &str = include_str!("../packaging/updater.pub");
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const METADATA_TIMEOUT: Duration = Duration::from_secs(20);
const CHUNK_TIMEOUT: Duration = Duration::from_secs(60);

const STAGING_SLUG: &str = "capwise";

pub(crate) fn update_platform() -> Result<String, String> {
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    platform_for(arch)
}

#[cfg(target_os = "linux")]
fn platform_for(arch: &str) -> Result<String, String> {
    cap_utils::linux_package::updater_target(arch)
}

#[cfg(not(target_os = "linux"))]
fn platform_for(arch: &str) -> Result<String, String> {
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "windows"
    };
    Ok(format!("{os}-{arch}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArtifactKind {
    AppArchive,
    NsisInstaller,
    Deb,
    AppImage,
}

impl ArtifactKind {
    pub(crate) fn for_platform(platform: &str) -> Result<Self, String> {
        if platform.starts_with("darwin-") {
            Ok(Self::AppArchive)
        } else if platform.starts_with("windows-") {
            Ok(Self::NsisInstaller)
        } else if platform.starts_with("linux-") && platform.contains("-deb") {
            Ok(Self::Deb)
        } else if platform.starts_with("linux-") && platform.contains("-appimage") {
            Ok(Self::AppImage)
        } else {
            Err(format!("Unsupported update platform: {platform}"))
        }
    }

    fn file_name(self) -> &'static str {
        match self {
            Self::AppArchive => "Capwise.app.tar.gz",
            Self::NsisInstaller => "Capwise-setup.exe",
            Self::Deb => "Capwise.deb",
            Self::AppImage => "Capwise.AppImage",
        }
    }

    fn matches(self, header: &[u8]) -> bool {
        match self {
            Self::AppArchive => header.starts_with(&[0x1f, 0x8b]),
            Self::NsisInstaller => header.starts_with(b"MZ"),
            Self::Deb => header.starts_with(b"!<arch>\n"),
            Self::AppImage => header.starts_with(b"\x7fELF"),
        }
    }
}

#[derive(Deserialize)]
struct RemoteAsset {
    url: String,
    signature: String,
    #[serde(default)]
    deltas: HashMap<String, RemoteDelta>,
}

#[derive(Deserialize)]
struct RemoteDelta {
    url: String,
    signature: String,
}

/// `latest.json` on the newest GitHub release: one version with an asset per
/// update platform. A flat `url` and `signature` serve every platform.
#[derive(Deserialize)]
struct RemoteManifest {
    version: String,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    signature: Option<String>,
    #[serde(default)]
    platforms: HashMap<String, RemoteAsset>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Release {
    pub(crate) version: Version,
    pub(crate) url: String,
    pub(crate) signature: String,
    pub(crate) notes: Option<String>,
    pub(crate) deltas: Vec<Delta>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct Delta {
    pub(crate) from: Version,
    pub(crate) url: String,
    pub(crate) signature: String,
}

fn https_url(url: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(url.trim())
        .map_err(|error| format!("Invalid update download URL: {error}"))?;
    if url.scheme() != "https" {
        return Err("Update downloads must use HTTPS".to_string());
    }
    Ok(url)
}

fn parse_release(body: &[u8], platform: &str) -> Result<Option<Release>, String> {
    let mut remote: RemoteManifest = serde_json::from_slice(body)
        .map_err(|error| format!("Invalid update response: {error}"))?;
    let version = Version::parse(remote.version.trim().trim_start_matches('v'))
        .map_err(|error| format!("Invalid update version {:?}: {error}", remote.version))?;
    let (url, signature, deltas) = match remote.platforms.remove(platform) {
        Some(asset) => (asset.url, asset.signature, asset.deltas),
        None if remote.platforms.is_empty() => match (remote.url, remote.signature) {
            (Some(url), Some(signature)) => (url, signature, HashMap::new()),
            (None, _) => return Err("The update has no download URL".to_string()),
            (_, None) => return Err("The update has no signature".to_string()),
        },
        None => return Ok(None),
    };
    let url = https_url(&url)?;
    if signature.trim().is_empty() {
        return Err("The update has no signature".to_string());
    }
    let mut deltas = deltas
        .into_iter()
        .filter_map(|(from, delta)| {
            let from = Version::parse(from.trim().trim_start_matches('v')).ok()?;
            let url = https_url(&delta.url).ok()?;
            let signature = delta.signature.trim();
            (!signature.is_empty()).then(|| Delta {
                from,
                url: url.into(),
                signature: signature.to_string(),
            })
        })
        .collect::<Vec<_>>();
    deltas.sort_by(|a, b| a.from.cmp(&b.from));
    Ok(Some(Release {
        version,
        url: url.into(),
        signature: signature.trim().to_string(),
        notes: remote.notes.filter(|notes| !notes.trim().is_empty()),
        deltas,
    }))
}

pub(crate) fn updates_configured() -> bool {
    !UPDATER_PUBLIC_KEY.trim().is_empty()
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("Capwise/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|error| error.to_string())
}

pub(crate) async fn fetch_release() -> Result<Option<Release>, String> {
    if !updates_configured() {
        return Err("This build has no update signing key".to_string());
    }
    let platform = update_platform()?;
    let response = client()?
        .get(UPDATE_MANIFEST_URL)
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .timeout(METADATA_TIMEOUT)
        .send()
        .await
        .map_err(|error| error.to_string())?;

    // Before the first release, latest/download has nothing to redirect to.
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }

    let body = response
        .error_for_status()
        .map_err(|error| error.to_string())?
        .bytes()
        .await
        .map_err(|error| error.to_string())?;
    parse_release(&body, &platform)
}

fn decode_base64_text(value: &str, what: &str) -> Result<String, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value.trim())
        .map_err(|error| format!("Invalid {what} encoding: {error}"))?;
    String::from_utf8(bytes).map_err(|_| format!("Invalid {what} text"))
}

fn trusted_key(text: &str) -> Result<PublicKey, String> {
    PublicKey::decode(text.trim()).map_err(|error| format!("Invalid update key: {error}"))
}

fn decode_signature(encoded: &str) -> Result<Signature, String> {
    Signature::decode(&decode_base64_text(encoded, "update signature")?)
        .map_err(|error| format!("Invalid update signature: {error}"))
}

enum Verifier<'a> {
    Stream(Box<StreamVerifier<'a>>),
    Buffered,
}

impl<'a> Verifier<'a> {
    fn new(key: &'a PublicKey, signature: &'a Signature) -> Result<Self, String> {
        match key.verify_stream(signature) {
            Ok(verifier) => Ok(Self::Stream(Box::new(verifier))),
            Err(minisign_verify::Error::UnsupportedLegacyMode) => Ok(Self::Buffered),
            Err(error) => Err(format!("The update was not signed by Capwise: {error}")),
        }
    }

    fn update(&mut self, chunk: &[u8]) {
        if let Self::Stream(verifier) = self {
            verifier.update(chunk);
        }
    }

    fn finish(self, key: &PublicKey, signature: &Signature, file: &Path) -> Result<(), String> {
        let verified = match self {
            Self::Stream(mut verifier) => verifier.finalize(),
            Self::Buffered => {
                let bytes = std::fs::read(file).map_err(|error| error.to_string())?;
                key.verify(&bytes, signature, true)
            }
        };
        verified.map_err(|error| format!("The download failed signature verification: {error}"))
    }
}

fn verify_file(file: &Path, key: &PublicKey, signature: &Signature) -> Result<(), String> {
    let mut verifier = Verifier::new(key, signature)?;
    if matches!(verifier, Verifier::Stream(_)) {
        let mut reader = std::fs::File::open(file).map_err(|error| error.to_string())?;
        let mut buffer = vec![0_u8; 256 * 1024];
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|error| error.to_string())?;
            if read == 0 {
                break;
            }
            verifier.update(&buffer[..read]);
        }
    }
    verifier.finish(key, signature, file)
}

pub(crate) fn check_artifact_kind(file: &Path, kind: ArtifactKind) -> Result<(), String> {
    if kind == ArtifactKind::AppArchive
        && file.is_dir()
        && file.join("Contents/Info.plist").is_file()
    {
        return Ok(());
    }
    let mut header = [0_u8; 8];
    let read = std::fs::File::open(file)
        .and_then(|mut reader| reader.read(&mut header))
        .map_err(|error| error.to_string())?;
    if kind.matches(&header[..read]) {
        Ok(())
    } else {
        Err("The downloaded file is not a Capwise package for this platform".to_string())
    }
}

fn staging_root() -> PathBuf {
    if let Some(directory) = std::env::var_os("CAP_GPUI_UPDATE_STAGING_DIR")
        && !directory.is_empty()
    {
        return PathBuf::from(directory);
    }
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("com.capwise.app")
        .join("updates")
}

fn prune_other_versions(product_dir: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(product_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path != keep
            && let Err(error) = std::fs::remove_dir_all(&path)
        {
            tracing::warn!(path = %path.display(), %error, "could not remove a stale update download");
        }
    }
}

pub(crate) async fn download(
    kind: ArtifactKind,
    release: &Release,
    mut progress: impl FnMut(u64, Option<u64>) + Send,
) -> Result<PathBuf, String> {
    #[cfg(target_os = "macos")]
    let running = running_app_bundle()
        .ok()
        .zip(Version::parse(env!("CARGO_PKG_VERSION")).ok());
    #[cfg(not(target_os = "macos"))]
    let running = None;
    download_into(
        &staging_root(),
        UPDATER_PUBLIC_KEY,
        kind,
        release,
        running,
        &mut progress,
    )
    .await
}

#[cfg(target_os = "macos")]
pub(crate) fn running_app_bundle() -> Result<PathBuf, String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    executable
        .ancestors()
        .find(|path| path.extension().is_some_and(|extension| extension == "app"))
        .map(Path::to_path_buf)
        .ok_or_else(|| "This copy of Capwise isn't running from an app bundle".to_string())
}

async fn download_into(
    root: &Path,
    public_key: &str,
    kind: ArtifactKind,
    release: &Release,
    running: Option<(PathBuf, Version)>,
    progress: &mut (impl FnMut(u64, Option<u64>) + Send),
) -> Result<PathBuf, String> {
    let key = trusted_key(public_key)?;
    let signature = decode_signature(&release.signature)?;
    let product_dir = root.join(STAGING_SLUG);
    let version_dir = product_dir.join(release.version.to_string());
    let destination = version_dir.join(kind.file_name());

    if destination.is_file() && verify_file(&destination, &key, &signature).is_ok() {
        prune_other_versions(&product_dir, &version_dir);
        return Ok(destination);
    }

    std::fs::create_dir_all(&version_dir).map_err(|error| error.to_string())?;
    prune_other_versions(&product_dir, &version_dir);

    #[cfg(not(target_os = "macos"))]
    let _ = running;
    #[cfg(target_os = "macos")]
    if kind == ArtifactKind::AppArchive
        && !version_dir.join(SKIP_DELTA_MARKER).exists()
        && let Some((bundle, current)) = running
        && let Some(delta) = release.deltas.iter().find(|delta| delta.from == current)
    {
        match download_delta(&version_dir, &key, delta, release, &bundle, progress).await {
            Ok(app) => return Ok(app),
            Err(error) => {
                tracing::warn!(from = %current, to = %release.version, "update patch failed, downloading the full update: {error}");
            }
        }
    }

    fetch_verified(&release.url, &destination, &key, &signature, progress).await?;
    Ok(destination)
}

#[cfg(target_os = "macos")]
/// Builds the new bundle from the installed one and a small signed patch.
/// The rebuilt `Capwise.app` sits in the version folder, ready for the same
/// swap a full archive gets.
async fn download_delta(
    version_dir: &Path,
    key: &PublicKey,
    delta: &Delta,
    release: &Release,
    bundle: &Path,
    progress: &mut (impl FnMut(u64, Option<u64>) + Send),
) -> Result<PathBuf, String> {
    let signature = decode_signature(&delta.signature)?;
    let patch = version_dir.join(format!("Capwise-from-{}.delta", delta.from));
    if !(patch.is_file() && verify_file(&patch, key, &signature).is_ok()) {
        let _ = std::fs::remove_file(&patch);
        fetch_verified(&delta.url, &patch, key, &signature, progress).await?;
    }

    let app = version_dir.join(DELTA_APP_NAME);
    if app.exists() {
        std::fs::remove_dir_all(&app).map_err(|error| error.to_string())?;
    }
    let from = delta.from.to_string();
    let to = release.version.to_string();
    let source = bundle.to_owned();
    let output = app.clone();
    let patch_file = patch.clone();
    let rebuilt = tokio::task::spawn_blocking(move || {
        crate::update_delta::apply(&patch_file, &source, &output, &from, &to)?;
        check_rebuilt_app(&output, &source)
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|result| result);
    if let Err(error) = rebuilt {
        let _ = std::fs::remove_dir_all(&app);
        let _ = std::fs::remove_file(&patch);
        return Err(error);
    }
    Ok(app)
}

#[cfg(target_os = "macos")]
const DELTA_APP_NAME: &str = "Capwise.app";
#[cfg(target_os = "macos")]
const SKIP_DELTA_MARKER: &str = "skip-delta";

/// A bundle rebuilt from a patch that then failed to install is thrown away
/// with its patch, and this version downloads the full archive from now on,
/// so a bad patch can never leave the update stuck.
#[cfg(target_os = "macos")]
pub(crate) fn discard_patched_update(package: &Path) {
    if package.file_name() != Some(std::ffi::OsStr::new(DELTA_APP_NAME)) {
        return;
    }
    let Some(version_dir) = package.parent() else {
        return;
    };
    let _ = std::fs::remove_dir_all(package);
    if let Ok(entries) = std::fs::read_dir(version_dir) {
        for entry in entries.flatten() {
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "delta")
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    let _ = std::fs::write(version_dir.join(SKIP_DELTA_MARKER), b"");
}

#[cfg(target_os = "macos")]
fn check_rebuilt_app(app: &Path, installed: &Path) -> Result<(), String> {
    let _ = std::process::Command::new("/usr/bin/xattr")
        .arg("-cr")
        .arg(app)
        .status();
    verify_replacement_signature(app, installed)
}

#[cfg(target_os = "macos")]
fn verify_replacement_signature(app: &Path, installed: &Path) -> Result<(), String> {
    let verified = std::process::Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(app)
        .output()
        .map_err(|error| error.to_string())?;
    if !verified.status.success() {
        return Err(format!(
            "The update failed code signature verification: {}",
            String::from_utf8_lossy(&verified.stderr).trim()
        ));
    }
    let requirement = designated_requirement(installed)?;
    if !satisfies_requirement(app, &requirement)? {
        return Err("The update is not signed like the installed app".to_string());
    }
    Ok(())
}

const DOWNLOAD_ATTEMPTS: u32 = 6;

fn retry_delay(attempt: u32) -> Duration {
    if cfg!(test) {
        Duration::from_millis(10)
    } else {
        Duration::from_secs(2_u64.pow(attempt.min(5)))
    }
}

enum FetchError {
    Retry(String),
    Fatal(String),
}

/// Downloads `url` to `destination` through a `.partial` file that survives
/// dropped connections and app restarts: each attempt asks only for the bytes
/// still missing, so a flaky connection to GitHub never starts a 250 MB
/// download over.
async fn fetch_verified(
    url: &str,
    destination: &Path,
    key: &PublicKey,
    signature: &Signature,
    progress: &mut (impl FnMut(u64, Option<u64>) + Send),
) -> Result<(), String> {
    let mut name = destination
        .file_name()
        .ok_or_else(|| "The download has no file name".to_string())?
        .to_os_string();
    name.push(".partial");
    let partial = destination.with_file_name(name);
    let mut attempt = 0;
    loop {
        attempt += 1;
        match stream_to_file(url, &partial, key, signature, progress).await {
            Ok(()) => break,
            Err(FetchError::Retry(error)) if attempt < DOWNLOAD_ATTEMPTS => {
                tracing::warn!(attempt, "update download interrupted, resuming: {error}");
                tokio::time::sleep(retry_delay(attempt)).await;
            }
            Err(FetchError::Retry(error)) => return Err(error),
            Err(FetchError::Fatal(error)) => {
                let _ = std::fs::remove_file(&partial);
                return Err(error);
            }
        }
    }
    std::fs::rename(&partial, destination).map_err(|error| error.to_string())
}

fn content_range_total(response: &reqwest::Response, start: u64) -> Option<u64> {
    let value = response
        .headers()
        .get(reqwest::header::CONTENT_RANGE)?
        .to_str()
        .ok()?;
    let (range, total) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (first, _) = range.split_once('-')?;
    (first.parse::<u64>().ok()? == start).then_some(())?;
    total.parse().ok()
}

async fn stream_to_file(
    url: &str,
    partial: &Path,
    key: &PublicKey,
    signature: &Signature,
    progress: &mut (impl FnMut(u64, Option<u64>) + Send),
) -> Result<(), FetchError> {
    use FetchError::{Fatal, Retry};

    let existing = std::fs::metadata(partial)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let mut request = client()
        .map_err(Fatal)?
        .get(url)
        .header(reqwest::header::ACCEPT, "application/octet-stream");
    if existing > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={existing}-"));
    }
    let response = request
        .send()
        .await
        .map_err(|error| Retry(error.to_string()))?;
    let status = response.status();
    if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        let _ = std::fs::remove_file(partial);
        return Err(Retry("The partial download was out of date".to_string()));
    }
    let response = response.error_for_status().map_err(|error| {
        if error
            .status()
            .is_some_and(|status| status.is_client_error())
        {
            Fatal(error.to_string())
        } else {
            Retry(error.to_string())
        }
    })?;

    let resumed = existing > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT;
    let (mut downloaded, total) = if resumed {
        match content_range_total(&response, existing) {
            Some(total) => (existing, Some(total)),
            None => {
                let _ = std::fs::remove_file(partial);
                return Err(Retry("The server resumed at the wrong place".to_string()));
            }
        }
    } else {
        (0, response.content_length())
    };

    let mut verifier = Verifier::new(key, signature).map_err(Fatal)?;
    let mut file = if resumed {
        let mut reader = std::fs::File::open(partial).map_err(|error| Fatal(error.to_string()))?;
        let mut buffer = vec![0_u8; 256 * 1024];
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|error| Fatal(error.to_string()))?;
            if read == 0 {
                break;
            }
            verifier.update(&buffer[..read]);
        }
        std::fs::OpenOptions::new()
            .append(true)
            .open(partial)
            .map_err(|error| Fatal(error.to_string()))?
    } else {
        std::fs::File::create(partial).map_err(|error| Fatal(error.to_string()))?
    };
    let mut stream = response.bytes_stream();
    progress(downloaded, total);

    loop {
        let next = tokio::time::timeout(CHUNK_TIMEOUT, stream.next())
            .await
            .map_err(|_| Retry("The download stopped responding".to_string()))?;
        let Some(chunk) = next else {
            break;
        };
        let chunk = chunk.map_err(|error| Retry(error.to_string()))?;
        file.write_all(&chunk)
            .map_err(|error| Fatal(error.to_string()))?;
        verifier.update(&chunk);
        downloaded = downloaded.saturating_add(chunk.len() as u64);
        progress(downloaded, total);
    }

    file.sync_all().map_err(|error| Fatal(error.to_string()))?;
    drop(file);
    if total.is_some_and(|total| total != downloaded) {
        return Err(Retry("The download ended early".to_string()));
    }
    verifier.finish(key, signature, partial).map_err(Fatal)
}

#[cfg(target_os = "macos")]
pub(crate) fn verify_update_source(
    running: &Path,
    package: &Path,
    version: &Version,
) -> Result<(), String> {
    let requirement = designated_requirement(&running_app_bundle()?)?;
    if !satisfies_requirement(running, &requirement)? {
        return Err("The installed app is not signed like the installer".to_string());
    }
    check_artifact_kind(package, ArtifactKind::AppArchive)?;
    if package.is_dir()
        && package
            != std::path::absolute(
                staging_root()
                    .join(STAGING_SLUG)
                    .join(version.to_string())
                    .join(DELTA_APP_NAME),
            )
            .map_err(|error| error.to_string())?
    {
        return Err("The rebuilt update is outside the update cache".to_string());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn install_app_bundle(
    archive: &Path,
    running: &Path,
    version: &Version,
) -> Result<PathBuf, String> {
    install_app_bundle_in(archive, running, Path::new("/Applications"), version)
}

#[cfg(target_os = "macos")]
fn install_app_bundle_in(
    archive: &Path,
    running: &Path,
    applications: &Path,
    version: &Version,
) -> Result<PathBuf, String> {
    let relocate = read_only_volume(running).map_err(|error| error.to_string())?;
    let destination = if relocate {
        applications.join("Capwise.app")
    } else {
        running.to_owned()
    };
    let parent = destination
        .parent()
        .ok_or_else(|| "The app has no parent folder".to_string())?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let staging = match unique_directory_in(parent, ".capwise-update") {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            unique_directory_in(&std::env::temp_dir(), "capwise-update")
                .map_err(|error| error.to_string())?
        }
        Err(error) => return Err(error.to_string()),
    };

    let unpacked = if archive.is_dir() {
        verify_replacement_signature(archive, running)
            .and_then(|()| move_app_into(archive, &staging))
    } else {
        extract_app_archive(archive, &staging)
    };
    let installed = unpacked.and_then(|app| {
        verify_replacement_signature(&app, running)?;
        if macos_bundle_version(&app)? != *version {
            return Err("The update bundle version does not match the release".to_string());
        }
        if relocate && can_reuse_installed_app(&app, &destination)? {
            return Ok(());
        }
        match replace_directory(&app, &destination) {
            Ok(()) => Ok(()),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::CrossesDevices
                ) =>
            {
                replace_directory_with_privileges(&app, &destination)
            }
            Err(error) => Err(error.to_string()),
        }
    });
    if let Err(error) = std::fs::remove_dir_all(&staging)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, "could not remove the update staging folder");
    }
    installed?;

    let _ = std::process::Command::new("/usr/bin/touch")
        .arg(&destination)
        .status();
    Ok(destination)
}

#[cfg(target_os = "macos")]
fn read_only_volume(path: &Path) -> std::io::Result<bool> {
    let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    let mut info = std::mem::MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::statfs(path.as_ptr(), info.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { info.assume_init() }.f_flags & libc::MNT_RDONLY as u32 != 0)
}

#[cfg(target_os = "macos")]
fn can_reuse_installed_app(replacement: &Path, installed: &Path) -> Result<bool, String> {
    let metadata = match std::fs::symlink_metadata(installed) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.is_dir() {
        return Err(format!(
            "Another item already exists at {}",
            installed.display()
        ));
    }

    let requirement = designated_requirement(replacement)?;
    if !satisfies_requirement(installed, &requirement)? {
        return Err(format!(
            "A different app already exists at {}",
            installed.display()
        ));
    }

    Ok(macos_bundle_version(installed)? >= macos_bundle_version(replacement)?)
}

#[cfg(target_os = "macos")]
fn designated_requirement(bundle: &Path) -> Result<String, String> {
    let output = std::process::Command::new("/usr/bin/codesign")
        .args(["--display", "-r", "-"])
        .arg(bundle)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "Could not read the macOS signing requirement of {}",
            bundle.display()
        ));
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("designated => "))
        .map(str::to_string)
        .ok_or_else(|| format!("{} has no macOS signing requirement", bundle.display()))
}

#[cfg(target_os = "macos")]
fn satisfies_requirement(bundle: &Path, requirement: &str) -> Result<bool, String> {
    let architecture = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x86_64"
    };
    Ok(std::process::Command::new("/usr/bin/codesign")
        .args(["--verify", "--strict", "-a", architecture, "-R"])
        .arg(format!("={requirement}"))
        .arg(bundle)
        .output()
        .map_err(|error| error.to_string())?
        .status
        .success())
}

#[cfg(target_os = "macos")]
fn macos_bundle_version(bundle: &Path) -> Result<Version, String> {
    let version = std::process::Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", "Print :CFBundleShortVersionString"])
        .arg(bundle.join("Contents/Info.plist"))
        .output()
        .map_err(|error| error.to_string())?;
    if !version.status.success() {
        return Err(format!(
            "Could not read the version of {}",
            bundle.display()
        ));
    }
    Version::parse(String::from_utf8_lossy(&version.stdout).trim())
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "macos")]
fn move_app_into(app: &Path, staging: &Path) -> Result<PathBuf, String> {
    let name = app
        .file_name()
        .ok_or_else(|| "The update has no app name".to_string())?;
    let staged = staging.join(name);
    match std::fs::rename(app, &staged) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
            let output = std::process::Command::new("/usr/bin/ditto")
                .arg(app)
                .arg(&staged)
                .output()
                .map_err(|error| format!("Could not stage the update: {error}"))?;
            if !output.status.success() {
                return Err(format!(
                    "Could not stage the update: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
        }
        Err(error) => return Err(format!("Could not stage the update: {error}")),
    }
    single_app_bundle(staging)
}

#[cfg(target_os = "macos")]
fn extract_app_archive(archive: &Path, staging: &Path) -> Result<PathBuf, String> {
    let output = std::process::Command::new("/usr/bin/tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(staging)
        .output()
        .map_err(|error| format!("Could not unpack the update: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Could not unpack the update: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    single_app_bundle(staging)
}

#[cfg(any(target_os = "macos", test))]
fn single_app_bundle(directory: &Path) -> Result<PathBuf, String> {
    let bundles = std::fs::read_dir(directory)
        .map_err(|error| error.to_string())?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|extension| extension == "app")
                && path.join("Contents/Info.plist").is_file()
        })
        .collect::<Vec<_>>();
    match bundles.as_slice() {
        [bundle] => Ok(bundle.clone()),
        _ => Err("The update archive does not contain exactly one app".to_string()),
    }
}

#[cfg(target_os = "macos")]
fn unique_directory_in(parent: &Path, prefix: &str) -> std::io::Result<PathBuf> {
    for attempt in 0..32_u32 {
        let candidate = parent.join(format!("{prefix}-{}-{attempt}", std::process::id()));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "no free staging folder name",
    ))
}

#[cfg(any(target_os = "macos", test))]
fn replace_directory(new: &Path, destination: &Path) -> std::io::Result<()> {
    if !destination.exists() {
        return std::fs::rename(new, destination);
    }
    let backup = previous_app_path(destination);
    if backup.exists() {
        std::fs::remove_dir_all(&backup)?;
    }
    std::fs::rename(destination, &backup)?;
    if let Err(error) = std::fs::rename(new, destination) {
        if let Err(restore) = std::fs::rename(&backup, destination) {
            tracing::error!(%restore, "could not restore the previous app after a failed update");
        }
        return Err(error);
    }
    if let Err(error) = std::fs::remove_dir_all(&backup) {
        tracing::warn!(%error, "could not remove the previous app after updating");
    }
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
const PRIVILEGED_REPLACE_SCRIPT: &str = r#"destination="$1"
replacement="$2"
backup="$3"
/bin/rm -rf "$backup" || exit 1
if [ -e "$destination" ]; then
    /bin/mv -f "$destination" "$backup" || exit 1
fi
if /bin/mv -f "$replacement" "$destination"; then
    /bin/rm -rf "$backup"
    exit 0
fi
/bin/rm -rf "$destination"
if [ -e "$backup" ]; then
    /bin/mv -f "$backup" "$destination"
fi
exit 1"#;

#[cfg(any(target_os = "macos", test))]
fn previous_app_path(destination: &Path) -> PathBuf {
    let name = destination
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    destination.with_file_name(format!(".{name}.previous-{}", std::process::id()))
}

#[cfg(target_os = "macos")]
fn replace_directory_with_privileges(new: &Path, destination: &Path) -> Result<(), String> {
    let output = std::process::Command::new("/usr/bin/osascript")
        .args([
            "-e",
            "on run argv",
            "-e",
            "do shell script \"/bin/sh -c \" & quoted form of (item 1 of argv) & \" cap-update \" & quoted form of (item 2 of argv) & \" \" & quoted form of (item 3 of argv) & \" \" & quoted form of (item 4 of argv) with administrator privileges",
            "-e",
            "end run",
        ])
        .arg(PRIVILEGED_REPLACE_SCRIPT)
        .arg(destination)
        .arg(new)
        .arg(previous_app_path(destination))
        .output()
        .map_err(|error| format!("Could not ask for permission to install: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "Capwise couldn't replace {}. {}",
            destination.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[cfg(windows)]
pub(crate) fn launch_installer(installer: &Path, arguments: &[&str]) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::{
        Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
        w,
    };

    let wide = |value: &std::ffi::OsStr| {
        value
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    };
    let file = wide(installer.as_os_str());
    let parameters = wide(std::ffi::OsStr::new(&arguments.join(" ")));
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            w!("open"),
            file.as_ptr(),
            parameters.as_ptr(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if result as isize > 32 {
        Ok(())
    } else {
        Err(format!(
            "Windows could not start the installer (code {})",
            result as isize
        ))
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn install_deb(package: &Path) -> Result<(), String> {
    let status = std::process::Command::new("pkexec")
        .arg("dpkg")
        .arg("-i")
        .arg(package)
        .status()
        .map_err(|error| format!("Could not ask for permission to install: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(crate::i18n::tr::dialog::update::package_not_installed().to_string())
    }
}

#[cfg(any(target_os = "linux", all(test, unix)))]
pub(crate) fn install_appimage(image: &Path, destination: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;

    let parent = destination
        .parent()
        .ok_or_else(|| "The AppImage has no parent folder".to_string())?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let name = destination
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let staged = parent.join(format!(".{name}.update-{}", std::process::id()));
    let copied = std::fs::copy(image, &staged)
        .and_then(|_| std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)))
        .and_then(|()| std::fs::rename(&staged, destination));
    if let Err(error) = copied {
        let _ = std::fs::remove_file(&staged);
        return Err(format!(
            "Could not install {}: {error}",
            destination.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};

    use super::*;

    const TEST_KEY: &str = "untrusted comment: minisign public key E7620F1842B4E81F\nRWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3\n";
    const TEST_SIGNATURE: &str = "untrusted comment: signature from minisign secret key\nRUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=\ntrusted comment: timestamp:1556193335\tfile:test\ny/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==\n";

    fn encode(text: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(text)
    }

    fn scratch(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "cap-gpui-installer-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn serve_once(body: &'static [u8]) -> (String, std::thread::JoinHandle<usize>) {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let url = format!("http://{}/artifact", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let mut served = 0;
            for stream in listener.incoming().take(1) {
                let mut stream = stream.unwrap();
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(body).unwrap();
                served += 1;
            }
            served
        });
        (url, server)
    }

    fn release(url: String) -> Release {
        Release {
            version: Version::new(9, 9, 9),
            url,
            signature: encode(TEST_SIGNATURE),
            notes: None,
            deltas: Vec::new(),
        }
    }

    #[test]
    fn the_shipped_update_key_is_a_public_minisign_key() {
        assert!(!UPDATER_PUBLIC_KEY.contains("secret key"));
        if updates_configured() {
            let key = trusted_key(UPDATER_PUBLIC_KEY).unwrap();
            assert!(
                key.untrusted_comment()
                    .is_some_and(|comment| comment.contains("public key"))
            );
        }
    }

    #[test]
    fn the_update_manifest_is_the_latest_github_release() {
        assert_eq!(
            UPDATE_MANIFEST_URL,
            "https://github.com/joeseesun/capwise/releases/latest/download/latest.json"
        );
    }

    #[test]
    fn release_metadata_requires_https_and_a_signature() {
        let release = parse_release(
            br#"{"version":"v0.7.0","notes":"","url":"https://example.com/1","signature":"c2ln"}"#,
            "darwin-aarch64",
        )
        .unwrap()
        .unwrap();
        assert_eq!(release.version, Version::new(0, 7, 0));
        assert_eq!(release.notes, None);
        for body in [
            &br#"{"version":"0.7.0","url":"http://example.com/a","signature":"c2ln"}"#[..],
            br#"{"version":"0.7.0","url":"https://example.com/a","signature":" "}"#,
            br#"{"version":"0.7.0","url":"https://example.com/a"}"#,
            br#"{"version":"seven","url":"https://example.com/a","signature":"c2ln"}"#,
            br#"{"version":"0.7.0","platforms":{"darwin-aarch64":{"url":"http://example.com/a","signature":"c2ln"}}}"#,
            b"<html>",
        ] {
            assert!(parse_release(body, "darwin-aarch64").is_err());
        }
    }

    #[test]
    fn manifests_pick_this_platforms_asset() {
        let body = br#"{
            "version": "0.2.0",
            "notes": "Faster captions",
            "pub_date": "2026-11-08T00:00:00Z",
            "platforms": {
                "darwin-aarch64": {
                    "url": "https://github.com/joeseesun/capwise/releases/download/v0.2.0/Capwise_0.2.0_aarch64.app.tar.gz",
                    "signature": "YXJt",
                    "deltas": {
                        "0.1.9": {
                            "url": "https://github.com/joeseesun/capwise/releases/download/v0.2.0/Capwise_0.1.9_to_0.2.0_aarch64.delta",
                            "signature": "OQ=="
                        },
                        "0.1.8": {
                            "url": "http://insecure.example/patch.delta",
                            "signature": "OA=="
                        },
                        "not-a-version": {
                            "url": "https://example.com/x.delta",
                            "signature": "eA=="
                        }
                    }
                },
                "darwin-x86_64": {
                    "url": "https://github.com/joeseesun/capwise/releases/download/v0.2.0/Capwise_0.2.0_x64.app.tar.gz",
                    "signature": "aW50ZWw="
                }
            }
        }"#;
        let arm = parse_release(body, "darwin-aarch64").unwrap().unwrap();
        assert_eq!(arm.version, Version::new(0, 2, 0));
        assert_eq!(arm.signature, "YXJt");
        assert!(arm.url.ends_with("Capwise_0.2.0_aarch64.app.tar.gz"));
        assert_eq!(arm.notes.as_deref(), Some("Faster captions"));
        assert_eq!(arm.deltas.len(), 1);
        assert_eq!(arm.deltas[0].from, Version::new(0, 1, 9));
        assert!(
            arm.deltas[0]
                .url
                .ends_with("Capwise_0.1.9_to_0.2.0_aarch64.delta")
        );
        let intel = parse_release(body, "darwin-x86_64").unwrap().unwrap();
        assert_eq!(intel.signature, "aW50ZWw=");
        assert!(intel.deltas.is_empty());
        assert_eq!(parse_release(body, "windows-x86_64").unwrap(), None);
    }

    #[test]
    fn release_script_signatures_verify_against_the_matching_key_only() {
        const RELEASE_KEY: &str = "untrusted comment: minisign public key 0EE3701F23B76A14\nRWQUarcjH3DjDho8clfzZBZuLdM1mLs0hNnMEeUaLGBwqbA+6eA4hM7c\n";
        const RELEASE_SIGNATURE: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIG1pbmlzaWduIHNlY3JldCBrZXkKUlVRVWFyY2pIM0RqRHM4MExNNFU0bGoxcTVtOFFuWG1xMkhZbDI5VU01cCsrZk03UStwQ3BBUVhzWEcxUnoySVBLdXdad2g3UVJyYW01dUJvVXllVVFRZG0rV0ZRMmhiQndrPQp0cnVzdGVkIGNvbW1lbnQ6IENhcHdpc2UgMC4yLjAgZGFyd2luLWFhcmNoNjQKS28vaUdFbGtFZ2RuY3h1N0Z0Y0c1K01lbHVQWUwyazhzUGFiOGdseDNqdXhaTkN6YUhRcE0wbjZYSHJSeFNNQ1dadFhBaUIzc1c0MFA0QXdtdUNKQ1E9PQo=";
        let directory = scratch("release-signature");
        let file = directory.join("Capwise.app.tar.gz");
        std::fs::write(&file, b"capwise-update").unwrap();
        let signature = decode_signature(RELEASE_SIGNATURE).unwrap();

        verify_file(&file, &trusted_key(RELEASE_KEY).unwrap(), &signature).unwrap();
        assert!(verify_file(&file, &trusted_key(TEST_KEY).unwrap(), &signature).is_err());
        std::fs::write(&file, b"capwise-updatE").unwrap();
        assert!(verify_file(&file, &trusted_key(RELEASE_KEY).unwrap(), &signature).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn update_platforms_map_to_their_artifact_kinds() {
        assert_eq!(
            ArtifactKind::for_platform("linux-x86_64-appimage"),
            Ok(ArtifactKind::AppImage)
        );
        assert_eq!(
            ArtifactKind::for_platform("linux-x86_64-deb"),
            Ok(ArtifactKind::Deb)
        );
        assert_eq!(
            ArtifactKind::for_platform("darwin-aarch64"),
            Ok(ArtifactKind::AppArchive)
        );
        assert_eq!(
            ArtifactKind::for_platform("windows-x86_64"),
            Ok(ArtifactKind::NsisInstaller)
        );
        assert!(ArtifactKind::for_platform("linux-x86_64-rpm").is_err());
        if !cfg!(target_os = "linux") {
            let platform = update_platform().unwrap();
            assert!(ArtifactKind::for_platform(&platform).is_ok(), "{platform}");
        }
    }

    #[test]
    fn signatures_verify_while_streaming_and_reject_tampering() {
        let key = trusted_key(TEST_KEY).unwrap();
        let signature = decode_signature(&encode(TEST_SIGNATURE)).unwrap();
        let directory = scratch("verify");
        let file = directory.join("artifact");

        std::fs::write(&file, b"test").unwrap();
        verify_file(&file, &key, &signature).unwrap();

        std::fs::write(&file, b"tesT").unwrap();
        assert!(verify_file(&file, &key, &signature).is_err());

        let mut verifier = Verifier::new(&key, &signature).unwrap();
        verifier.update(b"te");
        verifier.update(b"st");
        verifier.finish(&key, &signature, &file).unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn downloads_are_verified_staged_and_reused() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let root = scratch("download");
        let stale = root.join("capwise").join("0.0.1");
        std::fs::create_dir_all(&stale).unwrap();

        let (url, server) = serve_once(b"test");
        let mut updates = Vec::new();
        let downloaded = runtime
            .block_on(download_into(
                &root,
                TEST_KEY,
                ArtifactKind::AppImage,
                &release(url),
                None,
                &mut |done, total| updates.push((done, total)),
            ))
            .unwrap();
        assert_eq!(server.join().unwrap(), 1);
        assert_eq!(downloaded, root.join("capwise/9.9.9/Capwise.AppImage"));
        assert_eq!(std::fs::read(&downloaded).unwrap(), b"test");
        assert_eq!(updates.first(), Some(&(0, Some(4))));
        assert_eq!(updates.last(), Some(&(4, Some(4))));
        assert!(!stale.exists());
        assert!(check_artifact_kind(&downloaded, ArtifactKind::AppImage).is_err());

        let reused = runtime
            .block_on(download_into(
                &root,
                TEST_KEY,
                ArtifactKind::AppImage,
                &release("http://127.0.0.1:9/unreachable".to_string()),
                None,
                &mut |_, _| panic!("a verified cached download must not be fetched again"),
            ))
            .unwrap();
        assert_eq!(reused, downloaded);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn artifact_kinds_are_recognised_by_their_headers() {
        let root = scratch("kinds");
        for (kind, header) in [
            (ArtifactKind::AppArchive, &b"\x1f\x8b\x08\x00"[..]),
            (ArtifactKind::NsisInstaller, &b"MZ\x90\x00"[..]),
            (ArtifactKind::Deb, &b"!<arch>\ndebian"[..]),
            (ArtifactKind::AppImage, &b"\x7fELF\x02\x01"[..]),
        ] {
            let file = root.join("artifact");
            std::fs::write(&file, header).unwrap();
            check_artifact_kind(&file, kind).unwrap();
            std::fs::write(&file, b"<html>").unwrap();
            assert!(check_artifact_kind(&file, kind).is_err());
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    fn serve_script(
        responses: Vec<(Option<&'static str>, Vec<u8>)>,
    ) -> (String, std::thread::JoinHandle<usize>) {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let url = format!("http://{}/artifact", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let mut served = 0;
            for (expected_range, response) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 4096];
                let read = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
                let range = request
                    .lines()
                    .find_map(|line| line.strip_prefix("range: "))
                    .map(str::trim)
                    .map(str::to_string);
                assert_eq!(range.as_deref(), expected_range);
                stream.write_all(&response).unwrap();
                served += 1;
            }
            served
        });
        (url, server)
    }

    #[test]
    fn interrupted_downloads_resume_where_they_stopped() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let root = scratch("resume");
        let (url, server) = serve_script(vec![
            (
                None,
                b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nte".to_vec(),
            ),
            (
                Some("bytes=2-"),
                b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 2-3/4\r\nContent-Length: 2\r\nConnection: close\r\n\r\nst".to_vec(),
            ),
        ]);
        let mut updates = Vec::new();
        let downloaded = runtime
            .block_on(download_into(
                &root,
                TEST_KEY,
                ArtifactKind::AppImage,
                &release(url),
                None,
                &mut |done, total| updates.push((done, total)),
            ))
            .unwrap();
        assert_eq!(server.join().unwrap(), 2);
        assert_eq!(std::fs::read(&downloaded).unwrap(), b"test");
        assert!(updates.contains(&(2, Some(4))));
        assert_eq!(updates.last(), Some(&(4, Some(4))));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_server_that_ignores_the_range_restarts_the_download() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let root = scratch("no-range");
        let version_dir = root.join("capwise").join("9.9.9");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(version_dir.join("Capwise.AppImage.partial"), b"xx").unwrap();
        let (url, server) = serve_script(vec![(
            Some("bytes=2-"),
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntest".to_vec(),
        )]);
        let downloaded = runtime
            .block_on(download_into(
                &root,
                TEST_KEY,
                ArtifactKind::AppImage,
                &release(url),
                None,
                &mut |_, _| {},
            ))
            .unwrap();
        assert_eq!(server.join().unwrap(), 1);
        assert_eq!(std::fs::read(&downloaded).unwrap(), b"test");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Rebuilds a real shipped release from the one before it:
    /// CAPWISE_DELTA_OLD=<old.app> CAPWISE_DELTA_NEW=<new.app>
    /// CAPWISE_DELTA_FILE=<patch> CAPWISE_DELTA_VERSIONS=<from>:<to>
    /// cargo test --bin cap-gpui real_release_delta -- --ignored
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "needs two extracted releases and their patch"]
    fn real_release_delta_rebuilds_the_signed_bundle() {
        let var = |name| PathBuf::from(std::env::var_os(name).expect(name));
        let (old, new, patch) = (
            var("CAPWISE_DELTA_OLD"),
            var("CAPWISE_DELTA_NEW"),
            var("CAPWISE_DELTA_FILE"),
        );
        let versions = std::env::var("CAPWISE_DELTA_VERSIONS").unwrap();
        let (from, to) = versions.split_once(':').unwrap();
        let root = scratch("real-delta");
        let rebuilt = root.join("Capwise.app");
        let started = std::time::Instant::now();
        crate::update_delta::apply(&patch, &old, &rebuilt, from, to).unwrap();
        check_rebuilt_app(&rebuilt, &old).unwrap();
        eprintln!("rebuilt and verified in {:?}", started.elapsed());
        let tree = |bundle: &Path| {
            std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg("find . -print0 | sort -z | xargs -0 stat -f '%N %p %z %Y' && find . -type f -print0 | sort -z | xargs -0 shasum -a 256")
                .current_dir(bundle)
                .output()
                .unwrap()
                .stdout
        };
        assert!(
            tree(&rebuilt) == tree(&new),
            "the rebuilt bundle differs from the release"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_patch_that_fails_falls_back_to_the_full_update() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let root = scratch("delta-fallback");
        let installed = root.join("Installed.app");
        std::fs::create_dir_all(installed.join("Contents")).unwrap();
        let response =
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntest".to_vec();
        let (url, server) = serve_script(vec![(None, response.clone()), (None, response)]);
        let mut update = release(url.clone());
        update.deltas = vec![Delta {
            from: Version::new(9, 9, 8),
            url,
            signature: encode(TEST_SIGNATURE),
        }];
        let downloaded = runtime
            .block_on(download_into(
                &root,
                TEST_KEY,
                ArtifactKind::AppArchive,
                &update,
                Some((installed, Version::new(9, 9, 8))),
                &mut |_, _| {},
            ))
            .unwrap();
        assert_eq!(server.join().unwrap(), 2);
        assert_eq!(downloaded, root.join("capwise/9.9.9/Capwise.app.tar.gz"));
        let version_dir = root.join("capwise/9.9.9");
        assert!(!version_dir.join("Capwise-from-9.9.8.delta").exists());
        assert!(!version_dir.join("Capwise.app").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_patched_bundle_that_failed_to_install_is_never_patched_again() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let root = scratch("delta-discard");
        let version_dir = root.join("capwise/9.9.9");
        std::fs::create_dir_all(version_dir.join("Capwise.app/Contents")).unwrap();
        std::fs::write(version_dir.join("Capwise-from-9.9.8.delta"), b"patch").unwrap();
        discard_patched_update(&version_dir.join("Capwise.app"));
        assert!(!version_dir.join("Capwise.app").exists());
        assert!(!version_dir.join("Capwise-from-9.9.8.delta").exists());

        let installed = root.join("Installed.app");
        std::fs::create_dir_all(installed.join("Contents")).unwrap();
        let (url, server) = serve_script(vec![(
            None,
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntest".to_vec(),
        )]);
        let mut update = release(url.clone());
        update.deltas = vec![Delta {
            from: Version::new(9, 9, 8),
            url,
            signature: encode(TEST_SIGNATURE),
        }];
        let downloaded = runtime
            .block_on(download_into(
                &root,
                TEST_KEY,
                ArtifactKind::AppArchive,
                &update,
                Some((installed, Version::new(9, 9, 8))),
                &mut |_, _| {},
            ))
            .unwrap();
        assert_eq!(server.join().unwrap(), 1);
        assert_eq!(downloaded, version_dir.join("Capwise.app.tar.gz"));
        std::fs::remove_dir_all(root).unwrap();
    }

    fn bundle_tree(bundle: &Path) -> Vec<u8> {
        std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("find . -print0 | sort -z | xargs -0 stat -f '%N %p %z %Y' && find . -type f -print0 | sort -z | xargs -0 shasum -a 256")
            .current_dir(bundle)
            .output()
            .unwrap()
            .stdout
    }

    /// The whole patch path on two shipped releases: download through the
    /// updater, rebuild, then the same swap a full archive gets.
    /// CAPWISE_DELTA_OLD, CAPWISE_DELTA_NEW, CAPWISE_DELTA_FILE,
    /// CAPWISE_DELTA_VERSIONS=<from>:<to>, plus CAPWISE_DELTA_PUBLIC_KEY and
    /// CAPWISE_DELTA_SIGNATURE (a throwaway minisign pair over the patch).
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "needs two extracted releases and a signed patch"]
    fn real_release_patch_downloads_rebuilds_and_installs() {
        let var = |name| PathBuf::from(std::env::var_os(name).expect(name));
        let versions = std::env::var("CAPWISE_DELTA_VERSIONS").unwrap();
        let (from, to) = versions.split_once(':').unwrap();
        let (from, to) = (Version::parse(from).unwrap(), Version::parse(to).unwrap());
        let public_key = std::fs::read_to_string(var("CAPWISE_DELTA_PUBLIC_KEY")).unwrap();
        let signature = encode(&std::fs::read_to_string(var("CAPWISE_DELTA_SIGNATURE")).unwrap());
        let patch = std::fs::read(var("CAPWISE_DELTA_FILE")).unwrap();

        let root = scratch("real-install");
        let installed = root.join("Applications/Capwise.app");
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        assert!(
            std::process::Command::new("/usr/bin/ditto")
                .arg(var("CAPWISE_DELTA_OLD"))
                .arg(&installed)
                .status()
                .unwrap()
                .success()
        );

        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            patch.len()
        )
        .into_bytes();
        response.extend_from_slice(&patch);
        let (url, server) = serve_script(vec![(None, response)]);
        let update = Release {
            version: to.clone(),
            url: "http://127.0.0.1:9/never-fetched".to_string(),
            signature: signature.clone(),
            notes: None,
            deltas: vec![Delta {
                from: from.clone(),
                url,
                signature,
            }],
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let package = runtime
            .block_on(download_into(
                &root.join("cache"),
                &public_key,
                ArtifactKind::AppArchive,
                &update,
                Some((installed.clone(), from)),
                &mut |_, _| {},
            ))
            .unwrap();
        assert_eq!(server.join().unwrap(), 1);
        assert!(package.ends_with(format!("{to}/Capwise.app")));
        check_artifact_kind(&package, ArtifactKind::AppArchive).unwrap();

        let applications = root.join("Applications");
        let result = install_app_bundle_in(&package, &installed, &applications, &to).unwrap();
        assert_eq!(result, installed);
        assert!(!package.exists());
        assert!(
            bundle_tree(&installed) == bundle_tree(&var("CAPWISE_DELTA_NEW")),
            "the installed bundle differs from the release"
        );
        for args in [
            vec!["/usr/bin/codesign", "--verify", "--deep", "--strict"],
            vec!["/usr/sbin/spctl", "--assess", "--type", "execute"],
            vec!["/usr/bin/xcrun", "stapler", "validate"],
        ] {
            let status = std::process::Command::new(args[0])
                .args(&args[1..])
                .arg(&installed)
                .status()
                .unwrap();
            assert!(status.success(), "{args:?} rejected the installed app");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Resumes a real GitHub download: CAPWISE_RESUME_FROM=<the published
    /// archive for this Mac's latest release>.
    #[test]
    #[ignore = "downloads from GitHub"]
    fn a_real_github_download_resumes_mid_file() {
        let local = PathBuf::from(std::env::var_os("CAPWISE_RESUME_FROM").unwrap());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let mut update = runtime.block_on(fetch_release()).unwrap().unwrap();
        update.deltas.clear();
        let bytes = std::fs::read(&local).unwrap();
        let prefilled = bytes.len() as u64 - 5 * 1024 * 1024;
        let root = scratch("github-resume");
        let version_dir = root.join("capwise").join(update.version.to_string());
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(
            version_dir.join("Capwise.app.tar.gz.partial"),
            &bytes[..prefilled as usize],
        )
        .unwrap();
        let mut first = None;
        let downloaded = runtime
            .block_on(download_into(
                &root,
                UPDATER_PUBLIC_KEY,
                ArtifactKind::AppArchive,
                &update,
                None,
                &mut |done, total| {
                    first.get_or_insert((done, total));
                },
            ))
            .unwrap();
        assert_eq!(first, Some((prefilled, Some(bytes.len() as u64))));
        assert_eq!(std::fs::read(&downloaded).unwrap(), bytes);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tampered_downloads_are_discarded() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let root = scratch("tampered");
        let (url, server) = serve_once(b"tesT");
        let error = runtime
            .block_on(download_into(
                &root,
                TEST_KEY,
                ArtifactKind::AppImage,
                &release(url),
                None,
                &mut |_, _| {},
            ))
            .unwrap_err();
        assert_eq!(server.join().unwrap(), 1);
        assert!(error.contains("signature verification"));
        let version_dir = root.join("capwise").join("9.9.9");
        assert!(!version_dir.join("Capwise.AppImage").exists());
        assert!(!version_dir.join("Capwise.AppImage.partial").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn app_bundles_are_swapped_atomically_with_a_single_bundle_check() {
        let root = scratch("bundle");
        let staging = root.join("staging");
        let new_app = staging.join("Cap.app");
        std::fs::create_dir_all(new_app.join("Contents")).unwrap();
        std::fs::write(new_app.join("Contents/Info.plist"), "new").unwrap();
        assert_eq!(single_app_bundle(&staging).unwrap(), new_app);

        let destination = root.join("Applications").join("Cap.app");
        std::fs::create_dir_all(destination.join("Contents")).unwrap();
        std::fs::write(destination.join("Contents/Info.plist"), "old").unwrap();
        replace_directory(&new_app, &destination).unwrap();
        assert_eq!(
            std::fs::read_to_string(destination.join("Contents/Info.plist")).unwrap(),
            "new"
        );
        assert_eq!(
            std::fs::read_dir(root.join("Applications"))
                .unwrap()
                .count(),
            1
        );

        let other = staging.join("Other.app");
        std::fs::create_dir_all(other.join("Contents")).unwrap();
        std::fs::write(other.join("Contents/Info.plist"), "other").unwrap();
        let again = staging.join("Again.app");
        std::fs::create_dir_all(again.join("Contents")).unwrap();
        std::fs::write(again.join("Contents/Info.plist"), "again").unwrap();
        assert!(single_app_bundle(&staging).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn the_privileged_swap_restores_the_previous_app_when_the_move_fails() {
        let root = scratch("privileged");
        let applications = root.join("Applications");
        let destination = applications.join("Cap.app");
        let staged = root.join("staging").join("Cap.app");
        let backup = previous_app_path(&destination);
        let swap = |replacement: &Path| {
            std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(PRIVILEGED_REPLACE_SCRIPT)
                .arg("cap-update")
                .arg(&destination)
                .arg(replacement)
                .arg(&backup)
                .status()
                .unwrap()
        };
        let plist = |app: &Path| std::fs::read_to_string(app.join("Contents/Info.plist")).unwrap();
        let write_app = |app: &Path, contents: &str| {
            std::fs::create_dir_all(app.join("Contents")).unwrap();
            std::fs::write(app.join("Contents/Info.plist"), contents).unwrap();
        };

        write_app(&destination, "old");
        assert!(!swap(&root.join("missing.app")).success());
        assert_eq!(plist(&destination), "old");
        assert!(!backup.exists());

        write_app(&staged, "new");
        assert!(swap(&staged).success());
        assert_eq!(plist(&destination), "new");
        assert!(!backup.exists());
        assert!(!staged.exists());

        std::fs::remove_dir_all(&destination).unwrap();
        write_app(&staged, "fresh");
        assert!(swap(&staged).success());
        assert_eq!(plist(&destination), "fresh");
        assert_eq!(std::fs::read_dir(&applications).unwrap().count(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn appimages_are_replaced_in_place_and_made_executable() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = scratch("appimage");
        let source = root.join("download.AppImage");
        std::fs::write(&source, b"\x7fELFnew").unwrap();
        let destination = root.join("Applications").join("Cap.AppImage");
        install_appimage(&source, &destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"\x7fELFnew");
        assert_eq!(
            std::fs::metadata(&destination)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        std::fs::write(&source, b"\x7fELFnewer").unwrap();
        install_appimage(&source, &destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"\x7fELFnewer");
        assert_eq!(
            std::fs::read_dir(root.join("Applications"))
                .unwrap()
                .count(),
            1
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
