use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use dialoguer::{Input, Select};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    env, fs, io,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Parser)]
#[command(name = "locker")]
#[command(about = "Curated artifact locker for publishing, syncing, and browsing tool artifacts")]
struct Cli {
    #[arg(long, global = true, value_name = "PATH")]
    root: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Init,
    Add(AddArgs),
    List(ListArgs),
    Show(ShowArgs),
    ResolveUrl(ShowArgs),
    Verify,
    Publish(PublishArgs),
    Sync(SyncArgs),
    Doctor,
    Tui,
}

#[derive(Args)]
struct AddArgs {
    source: String,
    #[arg(long)]
    platform: Option<String>,
    #[arg(long)]
    category: Option<String>,
    #[arg(long)]
    filename: Option<String>,
    #[arg(long)]
    version: Option<String>,
    #[arg(long = "provenance-kind")]
    provenance_kind: Option<String>,
    #[arg(long = "source-url")]
    source_url: Option<String>,
    #[arg(long = "source-repo")]
    source_repo: Option<String>,
    #[arg(long = "source-tag")]
    source_tag: Option<String>,
    #[arg(long = "source-commit")]
    source_commit: Option<String>,
    #[arg(long = "archive-path")]
    archive_path: Option<String>,
    #[arg(long = "build-method")]
    build_method: Option<String>,
    #[arg(long)]
    notes: Option<String>,
    #[arg(long)]
    inactive: bool,
    #[arg(long, short = 'y')]
    yes: bool,
}

#[derive(Args)]
struct ShowArgs {
    filename: String,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct ListArgs {
    #[arg(long)]
    platform: Option<String>,
    #[arg(long)]
    category: Option<String>,
    #[arg(long)]
    active: Option<bool>,
    #[arg(long)]
    synced: Option<bool>,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct PublishArgs {
    tag: String,
    #[arg(long)]
    title: Option<String>,
    #[arg(long = "notes-file")]
    notes_file: Option<PathBuf>,
}

#[derive(Args, Clone)]
struct SyncArgs {
    #[arg(long)]
    platform: Option<String>,
    #[arg(long)]
    category: Option<String>,
    #[arg(long = "only")]
    only_filename: Option<String>,
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Clone, Serialize)]
struct ArtifactView {
    filename: String,
    platform: String,
    category: String,
    version: String,
    provenance_kind: String,
    active: bool,
    staged: bool,
    present: bool,
    verified: bool,
    stale: bool,
    staged_path: String,
    expected_local_path: Option<String>,
    recorded_local_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    artifacts: Vec<Artifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ProvenanceKind {
    Download,
    Built,
    Local,
}

impl ProvenanceKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Download => "download",
            Self::Built => "built",
            Self::Local => "local",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Provenance {
    kind: ProvenanceKind,
    uri: Option<String>,
    repo: Option<String>,
    tag: Option<String>,
    commit: Option<String>,
    asset_name: Option<String>,
    archive_path: Option<String>,
    build_method: Option<String>,
    notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Artifact {
    platform: String,
    category: String,
    filename: String,
    version: String,
    provenance: Provenance,
    sha256: String,
    object_name: String,
    active: bool,
}

impl Artifact {
    fn staged_path(&self, repo: &RepoPaths) -> PathBuf {
        repo.release_dir.join(&self.object_name)
    }

    fn object_name(&self) -> &str {
        &self.object_name
    }

    fn provenance_kind(&self) -> &'static str {
        self.provenance.kind.as_str()
    }

    fn pulled_oci_path(&self, output_dir: &Path) -> PathBuf {
        output_dir.join(&self.object_name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LocalMetadata {
    artifacts: Vec<LocalArtifactRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LocalArtifactRecord {
    filename: String,
    platform: String,
    category: String,
    version: String,
    provenance: Provenance,
    sha256: String,
    object_name: String,
    local_path: String,
    synced_at_epoch: u64,
}

#[derive(Clone)]
struct RepoPaths {
    root: PathBuf,
    manifest: PathBuf,
    checksums: PathBuf,
    release_dir: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AppConfig {
    catalog_root: Option<PathBuf>,
    payloads_dir: Option<PathBuf>,
    default_backend: Option<String>,
    github_owner: Option<String>,
    github_repo: Option<String>,
    github_base_url: Option<String>,
    github_manifest_url: Option<String>,
    github_checksums_url: Option<String>,
    oci_repository: Option<String>,
    oci_manifest_tag: Option<String>,
    oci_checksums_tag: Option<String>,
    oci_plain_http: Option<bool>,
}

impl RepoPaths {
    fn discover(root_override: Option<&Path>) -> Result<Self> {
        let root = if let Some(root) = root_override {
            root.to_path_buf()
        } else if let Ok(root) = env::var("LOCKER_ROOT") {
            PathBuf::from(root)
        } else if let Some(root) = load_config().ok().and_then(|config| config.catalog_root) {
            expand_home_path(root)
        } else if let Some(root) = repo_checkout_root()? {
            root
        } else {
            default_catalog_root()
        };
        Ok(Self::from_root(root))
    }

    fn from_root(root: PathBuf) -> Self {
        Self {
            manifest: root.join("manifests/artifacts.yaml"),
            checksums: root.join("checksums/sha256sums.txt"),
            release_dir: root.join("staging/release-assets"),
            root,
        }
    }

    fn ensure_layout(&self) -> Result<()> {
        fs::create_dir_all(self.manifest_parent()?)?;
        fs::create_dir_all(self.checksums_parent()?)?;
        fs::create_dir_all(&self.release_dir)?;
        Ok(())
    }

    fn init_if_missing(&self) -> Result<()> {
        self.ensure_layout()?;
        if !self.manifest.exists() {
            save_manifest(self, &Manifest { artifacts: vec![] })?;
        }
        if !self.checksums.exists() {
            fs::write(&self.checksums, "")?;
        }
        Ok(())
    }

    fn ensure_initialized(&self) -> Result<()> {
        if self.manifest.exists() && self.checksums.exists() {
            return Ok(());
        }
        bail!(
            "locker catalog is not initialized at {}. Run `locker --root {} init` or set LOCKER_ROOT to an existing catalog root.",
            self.root.display(),
            self.root.display()
        );
    }

    fn manifest_parent(&self) -> Result<&Path> {
        self.manifest
            .parent()
            .ok_or_else(|| anyhow!("manifest path has no parent"))
    }

    fn checksums_parent(&self) -> Result<&Path> {
        self.checksums
            .parent()
            .ok_or_else(|| anyhow!("checksums path has no parent"))
    }
}

#[derive(Clone)]
struct PayloadPaths {
    root: PathBuf,
    linux_dir: PathBuf,
    windows_dir: PathBuf,
    windows_bin_dir: PathBuf,
    windows_scripts_dir: PathBuf,
    windows_webshells_dir: PathBuf,
    metadata_dir: PathBuf,
    metadata_file: PathBuf,
}

impl PayloadPaths {
    fn discover() -> Result<Self> {
        let root = if let Ok(root) = env::var("PAYLOADS_DIR") {
            PathBuf::from(root)
        } else if let Some(root) = load_config().ok().and_then(|config| config.payloads_dir) {
            expand_home_path(root)
        } else {
            home_dir().join("tools/payloads")
        };
        let windows_dir = root.join("windows");
        let metadata_dir = root.join(".locker");
        Ok(Self {
            linux_dir: root.join("linux"),
            windows_bin_dir: windows_dir.join("bin"),
            windows_scripts_dir: windows_dir.join("scripts"),
            windows_webshells_dir: windows_dir.join("webshells"),
            metadata_file: metadata_dir.join("artifacts.json"),
            root,
            windows_dir,
            metadata_dir,
        })
    }

    fn ensure_dirs(&self) -> Result<()> {
        fs::create_dir_all(&self.linux_dir)?;
        fs::create_dir_all(&self.windows_bin_dir)?;
        fs::create_dir_all(&self.windows_scripts_dir)?;
        fs::create_dir_all(&self.windows_webshells_dir)?;
        fs::create_dir_all(&self.metadata_dir)?;
        Ok(())
    }

    fn destination_for(&self, artifact: &Artifact) -> Result<PathBuf> {
        match (artifact.platform.as_str(), artifact.category.as_str()) {
            ("linux", _) => Ok(self.linux_dir.join(&artifact.filename)),
            ("windows", "bin") => Ok(self.windows_bin_dir.join(&artifact.filename)),
            ("windows", "scripts") => Ok(self.windows_scripts_dir.join(&artifact.filename)),
            ("windows", "webshells") => Ok(self.windows_webshells_dir.join(&artifact.filename)),
            ("windows", _) => Ok(self.windows_dir.join(&artifact.filename)),
            _ => bail!(
                "unsupported artifact mapping: {}/{}",
                artifact.platform,
                artifact.category
            ),
        }
    }
}

#[derive(Clone, Copy)]
enum BackendKind {
    GithubReleases,
    OciRegistry,
}

impl BackendKind {
    fn from_env() -> Self {
        let backend = env::var("LOCKER_BACKEND")
            .ok()
            .or_else(|| load_config().ok().and_then(|config| config.default_backend))
            .unwrap_or_else(|| "oci-registry".into());
        match backend.as_str() {
            "oci" | "oci-registry" => Self::OciRegistry,
            _ => Self::GithubReleases,
        }
    }
}

trait PublishBackend {
    fn publish(&self, repo: &RepoPaths, manifest: &Manifest, args: &PublishArgs) -> Result<()>;
}

trait SyncBackend {
    fn sync(&self, payloads: &PayloadPaths, args: &SyncArgs) -> Result<()>;
    fn resolve_url(&self, filename: &str, platform: Option<&str>) -> Result<String>;
}

struct GithubReleasesBackend;
struct OciRegistryBackend;

const OCI_ARTIFACT_TYPE_FILE: &str = "application/vnd.artifact-catalog.file.v1";
const OCI_ARTIFACT_TYPE_MANIFEST: &str = "application/vnd.artifact-catalog.manifest.v1";
const OCI_ARTIFACT_TYPE_CHECKSUMS: &str = "application/vnd.artifact-catalog.checksums.v1";

fn github_owner() -> String {
    env::var("ARTIFACT_CATALOG_OWNER")
        .or_else(|_| env::var("GITHUB_OWNER"))
        .ok()
        .or_else(|| load_config().ok().and_then(|config| config.github_owner))
        .unwrap_or_else(|| "CameronCandau".into())
}

fn github_repo() -> String {
    env::var("ARTIFACT_CATALOG_REPO")
        .or_else(|_| env::var("GITHUB_REPO"))
        .ok()
        .or_else(|| load_config().ok().and_then(|config| config.github_repo))
        .unwrap_or_else(|| "Artifact-Catalog".into())
}

fn github_base_url() -> String {
    env::var("ARTIFACT_CATALOG_BASE_URL")
        .ok()
        .or_else(|| load_config().ok().and_then(|config| config.github_base_url))
        .unwrap_or_else(|| {
            format!(
                "https://github.com/{}/{}/releases/latest/download",
                github_owner(),
                github_repo()
            )
        })
}

fn github_manifest_url() -> String {
    env::var("ARTIFACT_CATALOG_MANIFEST_URL")
        .ok()
        .or_else(|| {
            load_config()
                .ok()
                .and_then(|config| config.github_manifest_url)
        })
        .unwrap_or_else(|| format!("{}/artifacts.yaml", github_base_url()))
}

fn github_checksums_url() -> String {
    env::var("ARTIFACT_CATALOG_CHECKSUMS_URL")
        .ok()
        .or_else(|| {
            load_config()
                .ok()
                .and_then(|config| config.github_checksums_url)
        })
        .unwrap_or_else(|| format!("{}/sha256sums.txt", github_base_url()))
}

fn oci_repository() -> Result<String> {
    env::var("ARTIFACT_CATALOG_OCI_REPOSITORY")
        .or_else(|_| env::var("OCI_REPOSITORY"))
        .ok()
        .or_else(|| load_config().ok().and_then(|config| config.oci_repository))
        .ok_or_else(|| {
            anyhow!(
                "OCI repository is required; set ARTIFACT_CATALOG_OCI_REPOSITORY or OCI_REPOSITORY (example: public.ecr.aws/alias/artifact-catalog)"
            )
        })
}

fn oci_manifest_tag() -> String {
    env::var("ARTIFACT_CATALOG_OCI_MANIFEST_TAG")
        .or_else(|_| env::var("OCI_MANIFEST_TAG"))
        .ok()
        .or_else(|| {
            load_config()
                .ok()
                .and_then(|config| config.oci_manifest_tag)
        })
        .unwrap_or_else(|| "artifacts-manifest".into())
}

fn oci_checksums_tag() -> String {
    env::var("ARTIFACT_CATALOG_OCI_CHECKSUMS_TAG")
        .or_else(|_| env::var("OCI_CHECKSUMS_TAG"))
        .ok()
        .or_else(|| {
            load_config()
                .ok()
                .and_then(|config| config.oci_checksums_tag)
        })
        .unwrap_or_else(|| "artifacts-sha256sums".into())
}

fn oci_plain_http() -> bool {
    env_truthy("ARTIFACT_CATALOG_OCI_PLAIN_HTTP")
        || env_truthy("OCI_PLAIN_HTTP")
        || load_config()
            .ok()
            .and_then(|config| config.oci_plain_http)
            .unwrap_or(false)
}

fn env_truthy(name: &str) -> bool {
    env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn oci_reference(tag: &str) -> Result<String> {
    Ok(format!(
        "{}:{}",
        oci_repository()?.trim_end_matches('/'),
        tag
    ))
}

fn oci_resolved_url(tag: &str) -> Result<String> {
    Ok(format!("oci://{}", oci_reference(tag)?))
}

fn ensure_oras_available() -> Result<()> {
    let output = Command::new("oras").arg("version").output();
    match output {
        Ok(result) if result.status.success() => Ok(()),
        Ok(result) => bail!(
            "oras is required for the OCI backend: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        ),
        Err(err) => bail!("oras is required for the OCI backend: {err}"),
    }
}

fn oras_base_command() -> Result<Command> {
    ensure_oras_available()?;
    let mut command = Command::new("oras");
    if oci_plain_http() {
        command.arg("--plain-http");
    }
    Ok(command)
}

fn run_oras(repo_root: &Path, args: &[String]) -> Result<String> {
    let mut command = oras_base_command()?;
    let output = command
        .current_dir(repo_root)
        .args(args)
        .output()
        .with_context(|| format!("running oras {}", args.join(" ")))?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn layer_media_type_for(filename: &str) -> &'static str {
    let lower = filename.to_ascii_lowercase();
    if lower.ends_with(".ps1")
        || lower.ends_with(".txt")
        || lower.ends_with(".md")
        || lower.ends_with(".yaml")
        || lower.ends_with(".yml")
        || lower.ends_with(".json")
        || lower.ends_with(".sh")
        || lower.ends_with(".py")
    {
        "text/plain"
    } else {
        "application/octet-stream"
    }
}

fn temp_work_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()))
}

fn parse_checksums(raw: &str) -> Result<BTreeMap<String, String>> {
    let mut parsed = BTreeMap::new();
    for (idx, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (sha256, object_name) = line
            .split_once("  ")
            .ok_or_else(|| anyhow!("invalid checksum line {}: {}", idx + 1, line))?;
        if sha256.len() != 64 || !sha256.chars().all(|ch| ch.is_ascii_hexdigit()) {
            bail!("invalid SHA256 in checksum line {}: {}", idx + 1, sha256);
        }
        if object_name.trim().is_empty() {
            bail!("missing object name in checksum line {}", idx + 1);
        }
        if parsed
            .insert(object_name.to_string(), sha256.to_ascii_lowercase())
            .is_some()
        {
            bail!("duplicate checksum entry for {}", object_name);
        }
    }
    Ok(parsed)
}

fn validate_manifest_checksums(
    manifest: &Manifest,
    checksums: &BTreeMap<String, String>,
) -> Result<()> {
    let expected: BTreeMap<String, String> = manifest
        .artifacts
        .iter()
        .map(|artifact| {
            (
                artifact.object_name.clone(),
                artifact.sha256.to_ascii_lowercase(),
            )
        })
        .collect();
    if expected.len() != manifest.artifacts.len() {
        bail!("manifest contains duplicate object_name entries");
    }
    if expected != *checksums {
        bail!("checksum file does not match manifest entries");
    }
    Ok(())
}

fn pull_oci_file(tag: &str, expected_filename: &str) -> Result<Vec<u8>> {
    let tmpdir = temp_work_dir("locker-oci-manifest");
    fs::create_dir_all(&tmpdir)?;
    let reference = oci_reference(tag)?;
    let pull_args = vec![
        "pull".to_string(),
        "--output".to_string(),
        tmpdir.display().to_string(),
        reference.clone(),
    ];
    let result = (|| {
        run_oras(&tmpdir, &pull_args)?;
        let path = tmpdir.join(expected_filename);
        fs::read(&path).with_context(|| {
            format!(
                "reading pulled OCI file {} from {}",
                expected_filename, reference
            )
        })
    })();
    let _ = fs::remove_dir_all(&tmpdir);
    result
}

fn load_remote_oci_manifest() -> Result<Manifest> {
    let raw = String::from_utf8(pull_oci_file(&oci_manifest_tag(), "artifacts.yaml")?)
        .context("remote OCI manifest is not valid UTF-8")?;
    let manifest: Manifest = serde_json::from_str(&raw).context("parsing pulled OCI manifest")?;
    ensure_valid_manifest(&manifest)?;
    Ok(manifest)
}

fn load_remote_oci_checksums() -> Result<BTreeMap<String, String>> {
    let raw = String::from_utf8(pull_oci_file(&oci_checksums_tag(), "sha256sums.txt")?)
        .context("remote OCI checksums are not valid UTF-8")?;
    parse_checksums(&raw)
}

fn push_unique_tag(tags: &mut Vec<String>, tag: String) {
    if !tags.iter().any(|existing| existing == &tag) {
        tags.push(tag);
    }
}

fn oci_metadata_tags(args: &PublishArgs) -> (Vec<String>, Vec<String>) {
    let mut manifest_tags = vec![oci_manifest_tag()];
    let mut checksum_tags = vec![oci_checksums_tag()];
    push_unique_tag(&mut manifest_tags, format!("{}-manifest", args.tag));
    push_unique_tag(&mut checksum_tags, format!("{}-sha256sums", args.tag));
    (manifest_tags, checksum_tags)
}

impl PublishBackend for GithubReleasesBackend {
    fn publish(&self, repo: &RepoPaths, _manifest: &Manifest, args: &PublishArgs) -> Result<()> {
        let client = Client::builder().build()?;
        let token = env::var("GITHUB_TOKEN").context("GITHUB_TOKEN is required")?;
        let owner = github_owner();
        let repo_name = github_repo();
        let api = format!("https://api.github.com/repos/{owner}/{repo_name}");
        let release_url = format!("{api}/releases/tags/{}", args.tag);

        let release_resp = client
            .get(&release_url)
            .header("Accept", "application/vnd.github+json")
            .bearer_auth(&token)
            .header("User-Agent", "locker")
            .send()?;

        let release_json = if release_resp.status().as_u16() == 404 {
            let body = args
                .notes_file
                .as_ref()
                .map(fs::read_to_string)
                .transpose()?
                .unwrap_or_default();
            let create_payload = serde_json::json!({
                "tag_name": args.tag,
                "name": args.title.clone().unwrap_or_else(|| args.tag.clone()),
                "body": body,
                "draft": false,
                "prerelease": false
            });
            let response = client
                .post(format!("{api}/releases"))
                .header("Accept", "application/vnd.github+json")
                .bearer_auth(&token)
                .header("User-Agent", "locker")
                .json(&create_payload)
                .send()?;
            if !response.status().is_success() {
                let status = response.status();
                let text = response.text().unwrap_or_default();
                bail!(
                    "GitHub create release failed with HTTP {}: {}",
                    status.as_u16(),
                    text
                );
            }
            response.json::<Value>()?
        } else if release_resp.status().is_success() {
            release_resp.json::<Value>()?
        } else {
            let status = release_resp.status();
            let text = release_resp.text().unwrap_or_default();
            bail!(
                "GitHub release lookup failed with HTTP {}: {}",
                status.as_u16(),
                text
            );
        };

        let release_id = release_json["id"]
            .as_u64()
            .ok_or_else(|| anyhow!("missing release id"))?;
        let upload_url = release_json["upload_url"]
            .as_str()
            .ok_or_else(|| anyhow!("missing upload_url"))?
            .split('{')
            .next()
            .ok_or_else(|| anyhow!("invalid upload_url"))?
            .to_string();

        let assets_api = format!("{api}/releases/{release_id}/assets");
        let assets: Vec<Value> = client
            .get(&assets_api)
            .header("Accept", "application/vnd.github+json")
            .bearer_auth(&token)
            .header("User-Agent", "locker")
            .send()?
            .error_for_status()?
            .json()?;

        let mut files = vec![
            ("artifacts.yaml".to_string(), repo.manifest.clone()),
            ("sha256sums.txt".to_string(), repo.checksums.clone()),
        ];
        for entry in fs::read_dir(&repo.release_dir)? {
            let path = entry?.path();
            if path.is_file() {
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| anyhow!("invalid staged asset name"))?
                    .to_string();
                files.push((name, path));
            }
        }

        for (asset_name, path) in files {
            if let Some(existing) = assets
                .iter()
                .find(|a| a["name"].as_str() == Some(&asset_name))
            {
                if let Some(id) = existing["id"].as_u64() {
                    client
                        .delete(format!("{api}/releases/assets/{id}"))
                        .header("Accept", "application/vnd.github+json")
                        .bearer_auth(&token)
                        .header("User-Agent", "locker")
                        .send()?
                        .error_for_status()?;
                }
            }

            let data = fs::read(&path)?;
            let response = client
                .post(format!("{upload_url}?name={asset_name}"))
                .header("Accept", "application/vnd.github+json")
                .bearer_auth(&token)
                .header("User-Agent", "locker")
                .header("Content-Type", "application/octet-stream")
                .body(data)
                .send()?;
            if !response.status().is_success() {
                let status = response.status();
                let text = response.text().unwrap_or_default();
                bail!(
                    "GitHub upload failed for {asset_name} with HTTP {}: {}",
                    status.as_u16(),
                    text
                );
            }
            println!("[+] Uploaded {asset_name}");
        }

        println!(
            "[+] Release ready: https://github.com/{owner}/{repo_name}/releases/tag/{}",
            args.tag
        );
        Ok(())
    }
}

impl SyncBackend for GithubReleasesBackend {
    fn sync(&self, payloads: &PayloadPaths, args: &SyncArgs) -> Result<()> {
        let count = github_sync_with_progress(payloads, args, |message| println!("[+] {message}"))?;
        println!("[+] Sync complete ({count} artifact(s))");
        Ok(())
    }

    fn resolve_url(&self, filename: &str, platform: Option<&str>) -> Result<String> {
        let repo = RepoPaths::discover(None)?;
        let manifest = load_manifest(&repo)?;
        let artifact = manifest
            .artifacts
            .into_iter()
            .find(|a| {
                a.filename == filename && a.active && platform.is_none_or(|p| a.platform == p)
            })
            .ok_or_else(|| anyhow!("artifact not found: {filename}"))?;
        Ok(format!(
            "{}/{}",
            github_base_url().trim_end_matches('/'),
            artifact.object_name()
        ))
    }
}

impl PublishBackend for OciRegistryBackend {
    fn publish(&self, repo: &RepoPaths, _manifest: &Manifest, args: &PublishArgs) -> Result<()> {
        let repository = oci_repository()?;
        let (manifest_tags, checksum_tags) = oci_metadata_tags(args);
        let staged_count = fs::read_dir(&repo.release_dir)?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .count();
        let total = staged_count + manifest_tags.len() + checksum_tags.len();
        let mut files = Vec::new();
        for tag in manifest_tags {
            files.push((
                tag,
                repo.manifest.clone(),
                OCI_ARTIFACT_TYPE_MANIFEST,
                "artifacts.yaml".to_string(),
            ));
        }
        for tag in checksum_tags {
            files.push((
                tag,
                repo.checksums.clone(),
                OCI_ARTIFACT_TYPE_CHECKSUMS,
                "sha256sums.txt".to_string(),
            ));
        }
        for entry in fs::read_dir(&repo.release_dir)? {
            let path = entry?.path();
            if path.is_file() {
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| anyhow!("invalid staged asset name"))?
                    .to_string();
                files.push((name.clone(), path, OCI_ARTIFACT_TYPE_FILE, name));
            }
        }

        println!("[+] Publishing OCI artifacts to {repository}");
        for (idx, (tag, path, artifact_type, layer_name)) in files.into_iter().enumerate() {
            let reference = oci_reference(&tag)?;
            let layer_spec = format!("{}:{}", path.display(), layer_media_type_for(&layer_name));
            let args = vec![
                "push".to_string(),
                "--artifact-type".to_string(),
                artifact_type.to_string(),
                reference.clone(),
                layer_spec,
            ];
            println!("[+] Uploading {}/{}: {}", idx + 1, total, tag);
            run_oras(&repo.root, &args)?;
            println!("[+] Published {reference}");
        }
        println!("[+] OCI catalog ready under {repository}");
        Ok(())
    }
}

impl SyncBackend for OciRegistryBackend {
    fn sync(&self, payloads: &PayloadPaths, args: &SyncArgs) -> Result<()> {
        payloads.ensure_dirs()?;
        let manifest = load_remote_oci_manifest()?;
        let checksums = load_remote_oci_checksums()?;
        validate_manifest_checksums(&manifest, &checksums)?;
        let total_available = manifest.artifacts.len();
        let mut artifacts: Vec<Artifact> = manifest
            .artifacts
            .into_iter()
            .filter(|artifact| artifact.active)
            .filter(|artifact| {
                args.platform
                    .as_ref()
                    .is_none_or(|platform| &artifact.platform == platform)
            })
            .filter(|artifact| {
                args.category
                    .as_ref()
                    .is_none_or(|category| &artifact.category == category)
            })
            .filter(|artifact| {
                args.only_filename
                    .as_ref()
                    .is_none_or(|name| &artifact.filename == name)
            })
            .collect();
        artifacts.sort_by(|a, b| {
            (&a.platform, &a.category, &a.filename).cmp(&(&b.platform, &b.category, &b.filename))
        });

        let total = artifacts.len();
        if total == 0 {
            if let Some(name) = &args.only_filename {
                println!("[+] no active OCI artifacts matched filename filter: {name}");
            } else if let Some(platform) = &args.platform {
                println!("[+] no active OCI artifacts matched platform filter: {platform}");
            } else if let Some(category) = &args.category {
                println!("[+] no active OCI artifacts matched category filter: {category}");
            } else {
                println!(
                    "[+] no active OCI artifacts found in remote manifest ({total_available} total entries)"
                );
            }
            return Ok(());
        }

        let mut metadata =
            load_local_metadata(payloads).unwrap_or(LocalMetadata { artifacts: vec![] });
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        for (idx, artifact) in artifacts.into_iter().enumerate() {
            let step = idx + 1;
            let dest_path = payloads.destination_for(&artifact)?;
            let reference = oci_reference(artifact.object_name())?;
            println!(
                "[+] syncing {step}/{total}: {} -> {}",
                artifact.filename,
                dest_path.display()
            );

            if args.dry_run {
                continue;
            }

            let tmpdir = temp_work_dir("locker-oci-pull");
            fs::create_dir_all(&tmpdir)?;
            let pull_args = vec![
                "pull".to_string(),
                "--output".to_string(),
                tmpdir.display().to_string(),
                reference.clone(),
            ];
            run_oras(&tmpdir, &pull_args).map_err(|err| {
                let _ = fs::remove_dir_all(&tmpdir);
                err
            })?;
            let pulled_path = artifact.pulled_oci_path(&tmpdir);
            let digest = sha256_hex(&pulled_path).map_err(|err| {
                let _ = fs::remove_dir_all(&tmpdir);
                err
            })?;
            let expected_sha = checksums
                .get(artifact.object_name())
                .ok_or_else(|| anyhow!("missing checksum entry for {}", artifact.object_name()))?;
            if digest != *expected_sha {
                let _ = fs::remove_dir_all(&tmpdir);
                bail!(
                    "SHA256 mismatch for {}: expected {}, got {}",
                    artifact.filename,
                    expected_sha,
                    digest
                );
            }

            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&pulled_path, &dest_path)?;
            if should_be_executable(&artifact.filename) {
                make_executable(&dest_path)?;
            }
            let _ = fs::remove_dir_all(&tmpdir);

            upsert_local_metadata(
                &mut metadata,
                LocalArtifactRecord {
                    filename: artifact.filename.clone(),
                    platform: artifact.platform.clone(),
                    category: artifact.category.clone(),
                    version: artifact.version.clone(),
                    provenance: artifact.provenance.clone(),
                    sha256: artifact.sha256.clone(),
                    object_name: artifact.object_name.clone(),
                    local_path: dest_path.display().to_string(),
                    synced_at_epoch: now,
                },
            );
            println!("[+] synced {step}/{total}: {}", artifact.filename);
        }

        if !args.dry_run {
            save_local_metadata(payloads, &metadata)?;
        }

        println!("[+] OCI sync complete ({total} artifact(s))");
        Ok(())
    }

    fn resolve_url(&self, filename: &str, platform: Option<&str>) -> Result<String> {
        let repo = RepoPaths::discover(None)?;
        let manifest = load_manifest(&repo)?;
        let artifact = manifest
            .artifacts
            .into_iter()
            .find(|a| {
                a.filename == filename && a.active && platform.is_none_or(|p| a.platform == p)
            })
            .ok_or_else(|| anyhow!("artifact not found: {filename}"))?;
        oci_resolved_url(artifact.object_name())
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let repo = RepoPaths::discover(cli.root.as_deref())?;
    unsafe {
        env::set_var("LOCKER_ROOT", &repo.root);
    }
    match cli.command {
        Commands::Init => init_command(&repo),
        Commands::Add(args) => add_command(&repo, args),
        Commands::List(args) => list_command(&repo, &args),
        Commands::Show(args) => show_command(&repo, &args),
        Commands::ResolveUrl(args) => resolve_url_command(&args.filename),
        Commands::Verify => verify_command(&repo),
        Commands::Publish(args) => publish_command(&repo, &args),
        Commands::Sync(args) => sync_command(args),
        Commands::Doctor => doctor_command(&repo),
        Commands::Tui => tui_command(&repo),
    }
}

fn backend_publish() -> Box<dyn PublishBackend> {
    match BackendKind::from_env() {
        BackendKind::GithubReleases => Box::new(GithubReleasesBackend),
        BackendKind::OciRegistry => Box::new(OciRegistryBackend),
    }
}

fn backend_sync() -> Box<dyn SyncBackend> {
    match BackendKind::from_env() {
        BackendKind::GithubReleases => Box::new(GithubReleasesBackend),
        BackendKind::OciRegistry => Box::new(OciRegistryBackend),
    }
}

fn load_manifest(repo: &RepoPaths) -> Result<Manifest> {
    repo.ensure_initialized()?;
    let raw = fs::read_to_string(&repo.manifest).context("reading manifest")?;
    Ok(serde_json::from_str(&raw).context("parsing manifest")?)
}

fn save_manifest(repo: &RepoPaths, manifest: &Manifest) -> Result<()> {
    repo.ensure_layout()?;
    ensure_valid_manifest(manifest)?;
    fs::write(
        &repo.manifest,
        serde_json::to_string_pretty(manifest)? + "\n",
    )?;
    Ok(())
}

fn parse_provenance_kind(value: &str) -> Result<ProvenanceKind> {
    match value.to_ascii_lowercase().as_str() {
        "download" | "upstream" | "release" => Ok(ProvenanceKind::Download),
        "built" => Ok(ProvenanceKind::Built),
        "local" => Ok(ProvenanceKind::Local),
        _ => bail!("unsupported provenance kind: {value}"),
    }
}

fn infer_provenance_kind(source: &str) -> ProvenanceKind {
    if source.starts_with("http://") || source.starts_with("https://") {
        ProvenanceKind::Download
    } else {
        ProvenanceKind::Local
    }
}

fn is_weak_version(version: &str) -> bool {
    matches!(
        version.to_ascii_lowercase().as_str(),
        "manual" | "latest" | "custom"
    )
}

fn looks_like_hex_commit(value: &str) -> bool {
    value.len() >= 7 && value.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn normalized_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn looks_like_placeholder_repo(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("owner/repo") || lower.contains("example.invalid")
}

fn looks_like_placeholder_url(value: &str) -> bool {
    value.to_ascii_lowercase().contains("example.invalid")
}

fn looks_like_placeholder_text(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    lower.is_empty()
        || matches!(
            lower.as_str(),
            "todo" | "tbd" | "placeholder" | "replace-me" | "example"
        )
}

fn derived_object_name(platform: &str, category: &str, filename: &str) -> String {
    format!("{platform}--{category}--{filename}")
}

fn looks_like_sha256(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn artifact_metadata_problems(artifact: &Artifact) -> Vec<String> {
    let mut problems = Vec::new();

    for (field, value) in [
        ("platform", artifact.platform.as_str()),
        ("category", artifact.category.as_str()),
        ("filename", artifact.filename.as_str()),
        ("version", artifact.version.as_str()),
        ("object_name", artifact.object_name.as_str()),
        ("sha256", artifact.sha256.as_str()),
    ] {
        if value.trim().is_empty() {
            problems.push(format!("{} has empty {}", artifact.filename, field));
        }
    }

    if !looks_like_sha256(&artifact.sha256) {
        problems.push(format!(
            "{} has invalid sha256 metadata: {}",
            artifact.filename, artifact.sha256
        ));
    }

    let expected_object_name =
        derived_object_name(&artifact.platform, &artifact.category, &artifact.filename);
    if artifact.object_name != expected_object_name {
        problems.push(format!(
            "{} has invalid object_name {}; expected {}",
            artifact.filename, artifact.object_name, expected_object_name
        ));
    }
    if artifact.object_name.contains('/') || artifact.object_name.contains('\\') {
        problems.push(format!(
            "{} object_name must not contain path separators: {}",
            artifact.filename, artifact.object_name
        ));
    }
    if artifact.object_name.contains("..") {
        problems.push(format!(
            "{} object_name must not contain traversal segments: {}",
            artifact.filename, artifact.object_name
        ));
    }

    match artifact.provenance.kind {
        ProvenanceKind::Download => {
            let uri = artifact.provenance.uri.as_deref().map(str::trim);
            if uri.is_none_or(|value| value.is_empty()) {
                problems.push(format!(
                    "{} is marked download but provenance.uri is missing",
                    artifact.filename
                ));
            } else if uri.is_some_and(looks_like_placeholder_url) {
                problems.push(format!(
                    "{} is marked download but provenance.uri is placeholder-like: {}",
                    artifact.filename,
                    artifact.provenance.uri.as_deref().unwrap_or_default()
                ));
            }
        }
        ProvenanceKind::Built => {
            let repo = artifact.provenance.repo.as_deref().map(str::trim);
            if repo.is_none_or(|value| value.is_empty() || looks_like_placeholder_repo(value)) {
                problems.push(format!(
                    "{} is marked built but provenance.repo is missing or placeholder-like",
                    artifact.filename
                ));
            }
            let commit = artifact.provenance.commit.as_deref().map(str::trim);
            if commit.is_none_or(|value| !looks_like_hex_commit(value) || value == "abcdef1234567")
            {
                problems.push(format!(
                    "{} is marked built but provenance.commit is not pinned",
                    artifact.filename
                ));
            }
            let build_method = artifact.provenance.build_method.as_deref().map(str::trim);
            if build_method.is_none_or(looks_like_placeholder_text) {
                problems.push(format!(
                    "{} is marked built but build_method is missing",
                    artifact.filename
                ));
            }
        }
        ProvenanceKind::Local => {
            let uri = artifact.provenance.uri.as_deref().map(str::trim);
            if uri.is_none_or(|value| value.is_empty()) {
                problems.push(format!(
                    "{} is marked local but provenance.uri is missing",
                    artifact.filename
                ));
            } else if uri
                .is_some_and(|value| value.starts_with("http://") || value.starts_with("https://"))
            {
                problems.push(format!(
                    "{} is marked local but provenance.uri is not a local path: {}",
                    artifact.filename,
                    artifact.provenance.uri.as_deref().unwrap_or_default()
                ));
            }
        }
    }

    problems
}

fn artifact_metadata_warnings(artifact: &Artifact) -> Vec<String> {
    let mut warnings = Vec::new();

    if is_weak_version(&artifact.version) {
        warnings.push(format!(
            "{} uses weak version metadata: {}",
            artifact.filename, artifact.version
        ));
    }

    match artifact.provenance.kind {
        ProvenanceKind::Download => {
            if artifact
                .provenance
                .repo
                .as_deref()
                .is_none_or(|repo| repo.trim().is_empty())
            {
                warnings.push(format!(
                    "{} is marked download but provenance.repo is missing",
                    artifact.filename
                ));
            }
            if artifact
                .provenance
                .tag
                .as_deref()
                .is_none_or(|tag| tag.trim().is_empty())
            {
                warnings.push(format!(
                    "{} is marked download but provenance.tag is missing",
                    artifact.filename
                ));
            } else if artifact.provenance.tag.as_deref().is_some_and(|tag| {
                matches!(
                    tag.trim().to_ascii_lowercase().as_str(),
                    "latest" | "main" | "master"
                )
            }) {
                warnings.push(format!(
                    "{} is marked download but provenance.tag is weak: {}",
                    artifact.filename,
                    artifact.provenance.tag.as_deref().unwrap_or_default()
                ));
            }
        }
        ProvenanceKind::Built => {
            if artifact.provenance.uri.is_none() {
                warnings.push(format!(
                    "{} is marked built but provenance.uri is missing",
                    artifact.filename
                ));
            }
            if artifact
                .provenance
                .notes
                .as_deref()
                .is_none_or(|notes| notes.trim().is_empty())
            {
                warnings.push(format!(
                    "{} is marked built but notes are missing",
                    artifact.filename
                ));
            }
        }
        ProvenanceKind::Local => {
            if artifact.provenance.repo.is_some()
                || artifact.provenance.tag.is_some()
                || artifact.provenance.commit.is_some()
            {
                warnings.push(format!(
                    "{} is marked local but also carries upstream provenance fields",
                    artifact.filename
                ));
            }
        }
    }

    warnings
}

fn manifest_metadata_problems(manifest: &Manifest) -> Vec<String> {
    let mut problems = Vec::new();
    let mut object_names = BTreeMap::new();

    for artifact in &manifest.artifacts {
        problems.extend(artifact_metadata_problems(artifact));
        *object_names
            .entry(artifact.object_name.clone())
            .or_insert(0usize) += 1;
    }

    for (object_name, count) in object_names {
        if count > 1 {
            problems.push(format!(
                "manifest contains duplicate object_name entries: {} ({count} entries)",
                object_name
            ));
        }
    }

    problems
}

fn ensure_valid_manifest(manifest: &Manifest) -> Result<()> {
    let problems = manifest_metadata_problems(manifest);
    if problems.is_empty() {
        return Ok(());
    }
    bail!("manifest validation failed: {}", problems.join("; "));
}

fn infer_github_repo(source: &str) -> Option<String> {
    let parsed = url::Url::parse(source).ok()?;
    let mut segments = parsed.path_segments()?;
    let owner = segments.next()?;
    let repo = segments.next()?;
    Some(format!("https://github.com/{owner}/{repo}"))
}

fn basename_from_source(source: &str) -> String {
    if source.starts_with("http://") || source.starts_with("https://") {
        source
            .rsplit('/')
            .next()
            .unwrap_or("artifact.bin")
            .to_string()
    } else {
        Path::new(source)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("artifact.bin")
            .to_string()
    }
}

fn infer_version_from_source(source: &str) -> Option<String> {
    if !(source.starts_with("http://") || source.starts_with("https://")) {
        return None;
    }

    let parsed = url::Url::parse(source).ok()?;
    let segments: Vec<_> = parsed.path_segments()?.collect();
    let download_idx = segments.iter().position(|segment| *segment == "download")?;
    segments.get(download_idx + 1).map(|s| s.to_string())
}

fn infer_platform_from_filename(filename: &str) -> Option<&'static str> {
    let lower = filename.to_ascii_lowercase();
    if lower.ends_with(".exe")
        || lower.ends_with(".dll")
        || lower.ends_with(".ps1")
        || lower.ends_with(".bat")
        || lower.ends_with(".cmd")
        || lower.ends_with(".vbs")
    {
        Some("windows")
    } else if lower.ends_with(".sh")
        || lower.ends_with(".elf")
        || lower.ends_with(".bin")
        || lower.ends_with(".run")
        || !lower.contains('.')
    {
        Some("linux")
    } else {
        None
    }
}

fn infer_category_from_filename(filename: &str) -> &'static str {
    let lower = filename.to_ascii_lowercase();
    if lower.ends_with(".ps1")
        || lower.ends_with(".py")
        || lower.ends_with(".sh")
        || lower.ends_with(".pl")
        || lower.ends_with(".rb")
        || lower.ends_with(".js")
    {
        "scripts"
    } else if lower.ends_with(".php")
        || lower.ends_with(".jsp")
        || lower.ends_with(".jspx")
        || lower.ends_with(".asp")
        || lower.ends_with(".aspx")
        || lower.ends_with(".war")
    {
        "webshells"
    } else {
        "bin"
    }
}

fn sha256_hex(path: &Path) -> Result<String> {
    let bytes = fs::read(path)?;
    let digest = Sha256::digest(&bytes);
    Ok(format!("{digest:x}"))
}

fn rebuild_checksums(repo: &RepoPaths, manifest: &Manifest) -> Result<()> {
    repo.ensure_layout()?;
    let mut lines = Vec::new();
    for artifact in &manifest.artifacts {
        let staged = artifact.staged_path(repo);
        if staged.is_file() {
            lines.push(format!("{}  {}", artifact.sha256, artifact.object_name));
        }
    }
    lines.sort();
    fs::write(
        &repo.checksums,
        lines.join("\n") + if lines.is_empty() { "" } else { "\n" },
    )?;
    Ok(())
}

fn github_sync_with_progress<F>(
    payloads: &PayloadPaths,
    args: &SyncArgs,
    mut progress: F,
) -> Result<usize>
where
    F: FnMut(String),
{
    payloads.ensure_dirs()?;
    let client = Client::builder().build()?;
    let manifest: Manifest = client
        .get(github_manifest_url())
        .send()?
        .error_for_status()?
        .json()?;
    ensure_valid_manifest(&manifest)?;
    let checksums = parse_checksums(
        &client
            .get(github_checksums_url())
            .send()?
            .error_for_status()?
            .text()?,
    )?;
    validate_manifest_checksums(&manifest, &checksums)?;

    progress(format!("payload root: {}", payloads.root.display()));
    progress(format!("artifact base URL: {}", github_base_url()));

    let artifacts: Vec<Artifact> = manifest
        .artifacts
        .into_iter()
        .filter(|a| a.active)
        .filter(|artifact| {
            args.platform
                .as_ref()
                .is_none_or(|platform| &artifact.platform == platform)
        })
        .filter(|artifact| {
            args.category
                .as_ref()
                .is_none_or(|category| &artifact.category == category)
        })
        .filter(|artifact| {
            args.only_filename
                .as_ref()
                .is_none_or(|filename| &artifact.filename == filename)
        })
        .collect();

    let total = artifacts.len();
    if total == 0 {
        if let Some(filename) = &args.only_filename {
            bail!(
                "no published active artifact matched --only {}. Publish the current catalog release first, then sync.",
                filename
            );
        }
        if let Some(platform) = &args.platform {
            progress(format!(
                "no active artifacts matched platform filter: {platform}"
            ));
        } else if let Some(category) = &args.category {
            progress(format!(
                "no active artifacts matched category filter: {category}"
            ));
        } else {
            progress("no active artifacts matched the current sync filters".into());
        }
        return Ok(0);
    }

    let mut metadata = load_local_metadata(payloads)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    for (idx, artifact) in artifacts.into_iter().enumerate() {
        let step = idx + 1;
        let dest_path = payloads.destination_for(&artifact)?;
        let url = format!("{}/{}", github_base_url(), artifact.object_name());
        progress(format!(
            "syncing {step}/{total}: {} -> {}",
            artifact.filename,
            dest_path.display()
        ));

        if args.dry_run {
            continue;
        }

        let bytes = client.get(&url).send()?.error_for_status()?.bytes()?;
        let digest = {
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            format!("{:x}", hasher.finalize())
        };
        let expected_sha = checksums
            .get(artifact.object_name())
            .ok_or_else(|| anyhow!("missing checksum entry for {}", artifact.object_name()))?;
        if digest != *expected_sha {
            bail!(
                "SHA256 mismatch for {}: expected {}, got {}",
                artifact.filename,
                expected_sha,
                digest
            );
        }

        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&dest_path, &bytes)?;
        if should_be_executable(&artifact.filename) {
            make_executable(&dest_path)?;
        }

        upsert_local_metadata(
            &mut metadata,
            LocalArtifactRecord {
                filename: artifact.filename.clone(),
                platform: artifact.platform.clone(),
                category: artifact.category.clone(),
                version: artifact.version.clone(),
                provenance: artifact.provenance.clone(),
                sha256: artifact.sha256.clone(),
                object_name: artifact.object_name.clone(),
                local_path: dest_path.display().to_string(),
                synced_at_epoch: now,
            },
        );
        progress(format!("synced {step}/{total}: {}", artifact.filename));
    }

    if !args.dry_run {
        save_local_metadata(payloads, &metadata)?;
    }

    Ok(total)
}

fn choose_from(field: &str, options: &[&str]) -> Result<String> {
    let idx = Select::new()
        .with_prompt(field)
        .items(options)
        .default(0)
        .interact()?;
    Ok(options[idx].to_string())
}

fn prompt_or_default(
    field: &str,
    suggested: Option<String>,
    fallback: &str,
    yes: bool,
) -> Result<String> {
    if yes {
        return Ok(suggested.unwrap_or_else(|| fallback.to_string()));
    }
    let default = suggested.unwrap_or_else(|| fallback.to_string());
    Ok(Input::new()
        .with_prompt(field)
        .default(default)
        .interact_text()?)
}

fn prompt_required(field: &str, suggested: Option<String>, yes: bool) -> Result<String> {
    if yes {
        return suggested
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow!("missing required {field}; pass --{field} explicitly"));
    }
    let mut prompt = Input::new().with_prompt(field);
    if let Some(default) = suggested.filter(|value| !value.trim().is_empty()) {
        prompt = prompt.default(default);
    }
    let value: String = prompt.interact_text()?;
    if value.trim().is_empty() {
        bail!("{field} is required");
    }
    Ok(value)
}

fn copy_or_download(source: &str) -> Result<(PathBuf, String)> {
    if source.starts_with("http://") || source.starts_with("https://") {
        let client = Client::builder().build()?;
        let response = client.get(source).send()?.error_for_status()?;
        let tmpdir = env::temp_dir().join(format!("locker-{}", std::process::id()));
        fs::create_dir_all(&tmpdir)?;
        let filename = basename_from_source(source);
        let path = tmpdir.join(filename);
        fs::write(&path, response.bytes()?)?;
        Ok((path, source.to_string()))
    } else {
        let path = PathBuf::from(source);
        if !path.is_file() {
            bail!("source file not found: {source}");
        }
        Ok((path.canonicalize().unwrap_or(path), source.to_string()))
    }
}

fn add_command(repo: &RepoPaths, args: AddArgs) -> Result<()> {
    repo.init_if_missing()?;
    let (source_path, source_identity) = copy_or_download(&args.source)?;
    let filename = args
        .filename
        .clone()
        .unwrap_or_else(|| basename_from_source(&args.source));
    let inferred_platform = infer_platform_from_filename(&filename).map(str::to_string);
    let inferred_category = Some(infer_category_from_filename(&filename).to_string());
    let inferred_version = infer_version_from_source(&args.source);
    let inferred_kind = infer_provenance_kind(&args.source);

    let platform = match args.platform {
        Some(v) => v,
        None => {
            if let Some(suggested) = inferred_platform {
                prompt_or_default("platform", Some(suggested), "windows", args.yes)?
            } else if args.yes {
                "windows".to_string()
            } else {
                choose_from("platform", &["windows", "linux"])?
            }
        }
    };
    let category = match args.category {
        Some(v) => v,
        None => prompt_or_default("category", inferred_category, "bin", args.yes)?,
    };
    let version = match args.version {
        Some(v) => v,
        None => prompt_required("version", inferred_version, args.yes)?,
    };
    let source_url = normalized_optional(args.source_url);
    let source_repo = normalized_optional(args.source_repo);
    let source_tag = normalized_optional(args.source_tag);
    let source_commit = normalized_optional(args.source_commit);
    let archive_path = normalized_optional(args.archive_path);
    let build_method = normalized_optional(args.build_method);
    let notes = normalized_optional(args.notes);
    let provenance_kind = match args.provenance_kind.as_deref() {
        Some(value) => parse_provenance_kind(value)?,
        None => inferred_kind,
    };
    let default_source_url =
        if source_identity.starts_with("http://") || source_identity.starts_with("https://") {
            Some(source_identity.clone())
        } else {
            None
        };
    let provenance = match provenance_kind {
        ProvenanceKind::Download => Provenance {
            kind: ProvenanceKind::Download,
            uri: Some(match source_url.clone() {
                Some(url) => url,
                None => prompt_required("source_url", default_source_url, args.yes)?,
            }),
            repo: source_repo.or_else(|| infer_github_repo(&source_identity)),
            tag: source_tag.or_else(|| infer_version_from_source(&source_identity)),
            commit: source_commit,
            asset_name: Some(filename.clone()),
            archive_path,
            build_method,
            notes,
        },
        ProvenanceKind::Built => Provenance {
            kind: ProvenanceKind::Built,
            uri: source_url.or(default_source_url),
            repo: Some(match source_repo.clone() {
                Some(repo) => repo,
                None => prompt_required("source_repo", None, args.yes)?,
            }),
            tag: source_tag,
            commit: Some(match source_commit.clone() {
                Some(commit) => commit,
                None => prompt_required("source_commit", None, args.yes)?,
            }),
            asset_name: Some(filename.clone()),
            archive_path,
            build_method: Some(match build_method.clone() {
                Some(build_method) => build_method,
                None => prompt_required("build_method", None, args.yes)?,
            }),
            notes,
        },
        ProvenanceKind::Local => Provenance {
            kind: ProvenanceKind::Local,
            uri: Some(source_path.display().to_string()),
            repo: source_repo,
            tag: source_tag,
            commit: source_commit,
            asset_name: Some(filename.clone()),
            archive_path,
            build_method,
            notes,
        },
    };
    let object_name = derived_object_name(&platform, &category, &filename);
    let dest_path = repo.release_dir.join(&object_name);
    fs::copy(&source_path, &dest_path)?;
    let sha256 = sha256_hex(&dest_path)?;

    let artifact = Artifact {
        platform,
        category,
        filename,
        version,
        provenance,
        sha256,
        object_name,
        active: !args.inactive,
    };
    let artifact_problems = artifact_metadata_problems(&artifact);
    if !artifact_problems.is_empty() {
        bail!(
            "refusing to add invalid artifact metadata: {}",
            artifact_problems.join("; ")
        );
    }

    let mut manifest = load_manifest(repo)?;
    ensure_valid_manifest(&manifest)?;
    let mut updated_existing = false;
    if let Some(existing) = manifest
        .artifacts
        .iter_mut()
        .find(|a| a.object_name == artifact.object_name)
    {
        *existing = artifact.clone();
        updated_existing = true;
    } else {
        manifest.artifacts.push(artifact.clone());
    }
    manifest.artifacts.sort_by(|a, b| {
        (&a.platform, &a.category, &a.filename).cmp(&(&b.platform, &b.category, &b.filename))
    });
    ensure_valid_manifest(&manifest)?;
    save_manifest(repo, &manifest)?;
    rebuild_checksums(repo, &manifest)?;

    println!(
        "[+] {} {} -> {}",
        if updated_existing { "Updated" } else { "Added" },
        args.source,
        artifact.object_name
    );
    println!("[+] Staged at {}", dest_path.display());
    println!("[+] Version: {}", artifact.version);
    println!("[+] Provenance kind: {}", artifact.provenance.kind.as_str());
    for warning in artifact_metadata_warnings(&artifact) {
        println!("[!] {warning}");
    }
    println!("[+] Next: locker verify");
    Ok(())
}

fn build_artifact_views(
    repo: &RepoPaths,
    manifest: Manifest,
    payloads: &PayloadPaths,
) -> Vec<ArtifactView> {
    let metadata = load_local_metadata(payloads).unwrap_or(LocalMetadata { artifacts: vec![] });
    manifest
        .artifacts
        .into_iter()
        .map(|artifact| {
            let staged_path = artifact.staged_path(repo);
            let local_state = local_payload_state(payloads, &metadata, &artifact);
            let provenance_kind = artifact.provenance_kind().to_string();
            ArtifactView {
                filename: artifact.filename,
                platform: artifact.platform,
                category: artifact.category,
                version: artifact.version,
                provenance_kind,
                active: artifact.active,
                staged: staged_path.is_file(),
                present: local_state.present,
                verified: local_state.verified,
                stale: local_state.stale,
                staged_path: staged_path.display().to_string(),
                expected_local_path: local_state.expected_local_path,
                recorded_local_path: local_state.recorded_local_path,
            }
        })
        .collect()
}

fn filter_artifact_views<'a>(views: Vec<ArtifactView>, args: &'a ListArgs) -> Vec<ArtifactView> {
    let mut views = views;
    views.retain(|artifact| {
        args.platform
            .as_ref()
            .is_none_or(|platform| &artifact.platform == platform)
            && args
                .category
                .as_ref()
                .is_none_or(|category| &artifact.category == category)
            && args.active.is_none_or(|active| artifact.active == active)
            && args.synced.is_none_or(|synced| artifact.verified == synced)
    });
    views
}

fn list_command(repo: &RepoPaths, args: &ListArgs) -> Result<()> {
    let manifest = load_manifest(repo)?;
    let payloads = PayloadPaths::discover()?;
    let views = filter_artifact_views(build_artifact_views(repo, manifest, &payloads), args);

    if args.json {
        println!("{}", serde_json::to_string_pretty(&views)?);
        return Ok(());
    }

    println!(
        "{:<32} {:<8} {:<10} {:<20} {:<15} {:<6} {:<6} {:<7} {:<8} {:<6}",
        "filename",
        "platform",
        "category",
        "version",
        "provenance",
        "active",
        "staged",
        "present",
        "verified",
        "stale"
    );
    for artifact in views {
        println!(
            "{:<32} {:<8} {:<10} {:<20} {:<15} {:<6} {:<6} {:<7} {:<8} {:<6}",
            artifact.filename,
            artifact.platform,
            artifact.category,
            truncate(&artifact.version, 20),
            artifact.provenance_kind,
            if artifact.active { "yes" } else { "no" },
            if artifact.staged { "yes" } else { "no" },
            if artifact.present { "yes" } else { "no" },
            if artifact.verified { "yes" } else { "no" },
            if artifact.stale { "yes" } else { "no" }
        );
    }
    Ok(())
}

fn truncate(s: &str, width: usize) -> String {
    if s.len() <= width {
        s.to_string()
    } else {
        format!("{}...", &s[..width.saturating_sub(3)])
    }
}

fn show_command(repo: &RepoPaths, args: &ShowArgs) -> Result<()> {
    let manifest = load_manifest(repo)?;
    let artifact = manifest
        .artifacts
        .into_iter()
        .find(|a| a.filename == args.filename)
        .ok_or_else(|| anyhow!("artifact not found: {}", args.filename))?;
    let payloads = PayloadPaths::discover()?;
    let metadata = load_local_metadata(&payloads).unwrap_or(LocalMetadata { artifacts: vec![] });
    let staged_path = artifact.staged_path(repo);
    let local_state = local_payload_state(&payloads, &metadata, &artifact);
    let local = local_record_for_artifact(&metadata, &artifact).cloned();

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "artifact": artifact,
                "staged_path": staged_path,
                "staged": staged_path.is_file(),
                "local_state": local_state,
                "local": local,
            }))?
        );
        return Ok(());
    }

    println!("{}", serde_json::to_string_pretty(&artifact)?);
    println!("staged_path: {}", staged_path.display());
    println!("staged: {}", staged_path.is_file());
    println!("provenance_kind: {}", artifact.provenance.kind.as_str());
    println!(
        "local_expected_path: {}",
        local_state
            .expected_local_path
            .clone()
            .unwrap_or_else(|| "-".into())
    );
    println!(
        "local_recorded_path: {}",
        local_state
            .recorded_local_path
            .clone()
            .unwrap_or_else(|| "-".into())
    );
    println!("local_present: {}", local_state.present);
    println!("local_verified: {}", local_state.verified);
    println!("local_stale: {}", local_state.stale);
    if let Some(local) = local {
        println!("synced_path: {}", local.local_path);
        println!("synced_at_epoch: {}", local.synced_at_epoch);
    }
    Ok(())
}

fn resolve_url_command(filename: &str) -> Result<()> {
    let url = backend_sync().resolve_url(filename, None)?;
    println!("{url}");
    Ok(())
}

fn init_command(repo: &RepoPaths) -> Result<()> {
    let manifest_exists = repo.manifest.exists();
    let checksums_exists = repo.checksums.exists();
    let release_dir_exists = repo.release_dir.exists();
    repo.init_if_missing()?;
    println!("[+] Catalog root: {}", repo.root.display());
    println!(
        "[+] manifests/: {}",
        if manifest_exists {
            "kept existing artifacts.yaml"
        } else {
            "created artifacts.yaml"
        }
    );
    println!(
        "[+] checksums/: {}",
        if checksums_exists {
            "kept existing sha256sums.txt"
        } else {
            "created sha256sums.txt"
        }
    );
    println!(
        "[+] staging/release-assets/: {}",
        if release_dir_exists {
            "kept existing directory"
        } else {
            "created directory"
        }
    );
    Ok(())
}

fn verify_command(repo: &RepoPaths) -> Result<()> {
    let count = verify_with_progress(repo, |_| {})?;
    println!("[+] Verified {count} artifact(s)");
    Ok(())
}

fn publish_command(repo: &RepoPaths, args: &PublishArgs) -> Result<()> {
    let manifest = load_manifest(repo)?;
    verify_command(repo)?;
    backend_publish().publish(repo, &manifest, args)
}

fn sync_command(args: SyncArgs) -> Result<()> {
    backend_sync().sync(&PayloadPaths::discover()?, &args)
}

fn verify_with_progress<F>(repo: &RepoPaths, mut progress: F) -> Result<usize>
where
    F: FnMut(String),
{
    let manifest = load_manifest(repo)?;
    ensure_valid_manifest(&manifest)?;
    let total = manifest.artifacts.len();
    let mut expected = Vec::new();
    for (idx, artifact) in manifest.artifacts.iter().enumerate() {
        progress(format!(
            "verifying {}/{}: {}",
            idx + 1,
            total,
            artifact.filename
        ));
        let path = artifact.staged_path(repo);
        if !path.is_file() {
            bail!("missing staged asset: {}", artifact.object_name);
        }
        let digest = sha256_hex(&path)?;
        if digest != artifact.sha256 {
            bail!("sha mismatch for {}", artifact.object_name);
        }
        expected.push(format!("{}  {}", artifact.sha256, artifact.object_name));
    }
    expected.sort();
    let mut actual: Vec<String> = fs::read_to_string(&repo.checksums)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    actual.sort();
    if expected != actual {
        bail!("checksum file does not match manifest/release-assets");
    }
    Ok(total)
}

fn doctor_command(repo: &RepoPaths) -> Result<()> {
    let mut problems = Vec::new();
    let mut notes = Vec::new();
    let mut warnings = Vec::new();

    if !repo.root.join(".git").exists() {
        notes.push("catalog root is not a git checkout".to_string());
    } else {
        if git_output(&repo.root, &["rev-parse", "--abbrev-ref", "HEAD"]).is_err() {
            problems.push("git repo present but current branch could not be resolved".to_string());
        }
        match git_output(&repo.root, &["remote", "get-url", "origin"]) {
            Ok(origin) => notes.push(format!("git origin: {origin}")),
            Err(_) => problems.push("git remote 'origin' is missing".to_string()),
        }
    }
    for path in [&repo.manifest, &repo.checksums] {
        if !path.exists() {
            problems.push(format!("missing {}", path.display()));
        }
    }
    if !repo.release_dir.exists() {
        problems.push(format!("missing {}", repo.release_dir.display()));
    }
    let payloads = PayloadPaths::discover()?;
    let manifest = load_manifest(repo)?;
    for problem in manifest_metadata_problems(&manifest) {
        problems.push(problem);
    }
    let payload_metadata =
        load_local_metadata(&payloads).unwrap_or(LocalMetadata { artifacts: vec![] });
    notes.push(format!("payload root: {}", payloads.root.display()));

    let staged_names: std::collections::BTreeSet<String> = fs::read_dir(&repo.release_dir)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(|entry| entry.ok()))
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter_map(|path| {
            path.file_name()
                .and_then(|s| s.to_str())
                .map(str::to_string)
        })
        .filter(|name| !name.starts_with('.'))
        .collect();
    let manifest_names: std::collections::BTreeSet<String> = manifest
        .artifacts
        .iter()
        .map(|artifact| artifact.object_name.clone())
        .collect();
    for missing in manifest_names.difference(&staged_names) {
        problems.push(format!(
            "manifest references missing staged asset: {missing}"
        ));
    }
    for orphan in staged_names.difference(&manifest_names) {
        problems.push(format!("staged asset not tracked in manifest: {orphan}"));
    }
    for artifact in &manifest.artifacts {
        warnings.extend(artifact_metadata_warnings(artifact));
        let local_state = local_payload_state(&payloads, &payload_metadata, artifact);
        if local_state.present && !local_state.has_local_record {
            problems.push(format!(
                "local payload exists without local metadata: {}",
                local_state
                    .expected_local_path
                    .clone()
                    .unwrap_or_else(|| artifact.filename.clone())
            ));
        } else if local_state.stale {
            problems.push(format!(
                "local payload state is stale for {} ({})",
                artifact.filename,
                local_state.status_label()
            ));
        }
    }

    match BackendKind::from_env() {
        BackendKind::GithubReleases => {
            if env::var("GITHUB_TOKEN").is_ok() {
                notes.push("GITHUB_TOKEN present".to_string());
            } else {
                notes.push("GITHUB_TOKEN missing (required only for publish)".to_string());
            }
            notes.push(format!("manifest url: {}", github_manifest_url()));
            let client = Client::builder().timeout(Duration::from_secs(10)).build()?;
            match client.get(github_manifest_url()).send() {
                Ok(resp) if resp.status().is_success() => {
                    notes.push("manifest URL reachable".to_string());
                }
                Ok(resp) => {
                    problems.push(format!(
                        "manifest URL returned HTTP {}",
                        resp.status().as_u16()
                    ));
                }
                Err(err) => {
                    problems.push(format!("manifest URL unreachable: {err}"));
                }
            }
        }
        BackendKind::OciRegistry => {
            notes.push("LOCKER_BACKEND=oci-registry".to_string());
            match oci_repository() {
                Ok(repository) => {
                    notes.push(format!("oci repository: {repository}"));
                    notes.push(format!(
                        "oci manifest ref: {}",
                        oci_resolved_url(&oci_manifest_tag())?
                    ));
                    notes.push(format!(
                        "oci checksums ref: {}",
                        oci_resolved_url(&oci_checksums_tag())?
                    ));
                }
                Err(err) => problems.push(err.to_string()),
            }
            match ensure_oras_available() {
                Ok(()) => notes.push("oras available".to_string()),
                Err(err) => problems.push(err.to_string()),
            }
            if problems.is_empty() {
                match load_remote_oci_manifest() {
                    Ok(remote_manifest) => {
                        notes.push(format!(
                            "remote OCI manifest reachable ({} artifact(s))",
                            remote_manifest.artifacts.len()
                        ));
                    }
                    Err(err) => {
                        problems.push(format!("remote OCI manifest unreachable: {err}"));
                    }
                }
            }
        }
    }
    if let Err(err) = verify_command(repo) {
        problems.push(format!("verify failed: {err}"));
    }

    if problems.is_empty() {
        println!("[+] doctor: ok");
    } else {
        println!("[!] doctor: found issues");
        for p in &problems {
            println!("  - {p}");
        }
    }
    for warning in warnings {
        println!("  ! {warning}");
    }
    for note in notes {
        println!("  * {note}");
    }
    Ok(())
}

fn tui_command(repo: &RepoPaths) -> Result<()> {
    let mut manifest = load_manifest(repo)?;
    if manifest.artifacts.is_empty() {
        println!("No artifacts in manifest.");
        return Ok(());
    }
    let payloads = PayloadPaths::discover()?;
    let mut metadata =
        load_local_metadata(&payloads).unwrap_or(LocalMetadata { artifacts: vec![] });
    let hints = "j/k move  / search  esc clear  s sync  S bulk-sync  v verify  Enter actions  R reload  a toggle  y/p/u/r copy  q quit";
    let mut status = String::from("ready");
    let mut progress_line = String::from("task: idle");
    let mut filter_query = String::new();
    let mut search_input: Option<String> = None;
    let mut task: Option<TuiTask> = None;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut state = ListState::default();
    state.select(Some(0));

    let result = loop {
        terminal.draw(|f| {
            let size = f.area();
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
                .split(size);
            let right = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(10), Constraint::Length(5)])
                .split(chunks[1]);
            let visible_indices = filtered_artifact_indices(&manifest, &filter_query);
            let selected_visible = selected_visible_index(state.selected(), visible_indices.len());

            let items: Vec<ListItem> = manifest
                .artifacts
                .iter()
                .enumerate()
                .filter(|(idx, _)| visible_indices.contains(idx))
                .map(|(_, a)| {
                    let state_tags = local_payload_state(&payloads, &metadata, a);
                    let active = if a.active { "+" } else { "-" };
                    let staged = if a.staged_path(repo).is_file() {
                        "stg"
                    } else {
                        "---"
                    };
                    let sync = if state_tags.stale {
                        "old"
                    } else if state_tags.verified {
                        "ver"
                    } else if state_tags.present {
                        "pre"
                    } else {
                        "---"
                    };
                    ListItem::new(Line::from(format!(
                        "[{active}][{staged}][{sync}] {}",
                        a.filename
                    )))
                })
                .collect();
            let list = List::new(items)
                .block(
                    Block::default()
                        .title(format!(
                            "Artifacts{}",
                            if filter_query.is_empty() {
                                String::new()
                            } else {
                                format!(" / {}", filter_query)
                            }
                        ))
                        .borders(Borders::ALL),
                )
                .highlight_style(Style::default().add_modifier(Modifier::BOLD))
                .highlight_symbol("> ");
            f.render_stateful_widget(list, chunks[0], &mut state);

            let text = if let Some(actual_idx) = visible_indices.get(selected_visible) {
                let artifact = &manifest.artifacts[*actual_idx];
                let artifact_state = local_payload_state(&payloads, &metadata, artifact);
                Text::from(vec![
                    Line::from(format!("filename: {}", artifact.filename)),
                    Line::from(format!("platform: {}", artifact.platform)),
                    Line::from(format!("category: {}", artifact.category)),
                    Line::from(format!("version: {}", artifact.version)),
                    Line::from(format!(
                        "provenance_kind: {}",
                        artifact.provenance.kind.as_str()
                    )),
                    Line::from(format!(
                        "provenance_uri: {}",
                        artifact
                            .provenance
                            .uri
                            .clone()
                            .unwrap_or_else(|| "-".into())
                    )),
                    Line::from(format!(
                        "provenance_repo: {}",
                        artifact
                            .provenance
                            .repo
                            .clone()
                            .unwrap_or_else(|| "-".into())
                    )),
                    Line::from(format!(
                        "provenance_commit: {}",
                        artifact
                            .provenance
                            .commit
                            .clone()
                            .unwrap_or_else(|| "-".into())
                    )),
                    Line::from(format!("sha256: {}", artifact.sha256)),
                    Line::from(format!("object_name: {}", artifact.object_name)),
                    Line::from(format!("active: {}", artifact.active)),
                    Line::from(format!("staged: {}", artifact.staged_path(repo).display())),
                    Line::from(format!(
                        "sync_path: {}",
                        artifact_state
                            .expected_local_path
                            .clone()
                            .unwrap_or_else(|| "-".into())
                    )),
                    Line::from(format!(
                        "recorded_local_path: {}",
                        artifact_state
                            .recorded_local_path
                            .clone()
                            .unwrap_or_else(|| "-".into())
                    )),
                    Line::from(format!("local_present: {}", artifact_state.present)),
                    Line::from(format!("local_verified: {}", artifact_state.verified)),
                    Line::from(format!("sync_state: {}", artifact_state.status_label())),
                    Line::from(format!("stale: {}", artifact_state.stale)),
                ])
            } else {
                Text::from(vec![
                    Line::from("No artifacts match the current filter."),
                    Line::from("Press / to search again or Esc to clear the filter."),
                ])
            };
            let details =
                Paragraph::new(text).block(Block::default().title("Details").borders(Borders::ALL));
            f.render_widget(details, right[0]);
            let footer = Paragraph::new(Text::from(vec![
                Line::from(format!("status: {status}")),
                Line::from(progress_line.as_str()),
                Line::from(match &search_input {
                    Some(input) => format!("search: {input}"),
                    None => format!(
                        "filter: {}",
                        if filter_query.is_empty() {
                            "(none)".to_string()
                        } else {
                            filter_query.clone()
                        }
                    ),
                }),
                Line::from(hints),
            ]))
            .block(
                Block::default()
                    .title("Status")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Yellow)),
            );
            f.render_widget(footer, right[1]);
        })?;

        if let Some(active_task) = &task {
            loop {
                match active_task.receiver.try_recv() {
                    Ok(TaskUpdate::Progress(message)) => {
                        progress_line = format!("task: {}", message);
                    }
                    Ok(TaskUpdate::Finished(result)) => {
                        task = None;
                        progress_line = String::from("task: idle");
                        match result {
                            Ok(message) => {
                                status = message;
                                manifest = load_manifest(repo)?;
                                metadata = load_local_metadata(&payloads)
                                    .unwrap_or(LocalMetadata { artifacts: vec![] });
                            }
                            Err(err) => {
                                status = err;
                            }
                        }
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        task = None;
                        progress_line = String::from("task: idle");
                        status = "task channel disconnected".into();
                        break;
                    }
                }
            }
        }

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if let Some(input) = &mut search_input {
                    match key.code {
                        KeyCode::Esc => {
                            search_input = None;
                            status = "search cancelled".into();
                        }
                        KeyCode::Enter => {
                            filter_query = input.trim().to_string();
                            search_input = None;
                            state.select(Some(0));
                            status = if filter_query.is_empty() {
                                "filter cleared".into()
                            } else {
                                format!("filter: {}", filter_query)
                            };
                        }
                        KeyCode::Backspace => {
                            input.pop();
                        }
                        KeyCode::Char(ch) => {
                            input.push(ch);
                        }
                        _ => {}
                    }
                    continue;
                }
                let visible_indices = filtered_artifact_indices(&manifest, &filter_query);
                let selected_visible =
                    selected_visible_index(state.selected(), visible_indices.len());
                match key.code {
                    KeyCode::Char('q') => break Ok(()),
                    KeyCode::Esc => {
                        filter_query.clear();
                        state.select(Some(0));
                        status = "filter cleared".into();
                    }
                    KeyCode::Char('/') => {
                        search_input = Some(filter_query.clone());
                        status =
                            "search mode: type to filter, Enter to apply, Esc to cancel".into();
                    }
                    KeyCode::Char('R') => {
                        if task.is_some() {
                            status = "task is running; wait for it to finish".into();
                            continue;
                        }
                        manifest = load_manifest(repo)?;
                        metadata = load_local_metadata(&payloads)
                            .unwrap_or(LocalMetadata { artifacts: vec![] });
                        state.select(Some(0));
                        status = "reloaded manifest and local metadata".into();
                    }
                    KeyCode::Char('s') => {
                        if task.is_some() {
                            status = "task is running; wait for it to finish".into();
                            continue;
                        }
                        if let Some(actual_idx) = visible_indices.get(selected_visible) {
                            let artifact = manifest.artifacts[*actual_idx].clone();
                            task = Some(spawn_sync_task(
                                payloads.clone(),
                                vec![artifact.clone()],
                                format!("synced {}", artifact.filename),
                            ));
                            progress_line = format!("task: queued sync for {}", artifact.filename);
                            status = "sync started".into();
                        } else {
                            status = "no selected artifact to sync".into();
                        }
                    }
                    KeyCode::Char('S') => {
                        if task.is_some() {
                            status = "task is running; wait for it to finish".into();
                            continue;
                        }
                        let artifacts: Vec<Artifact> = visible_indices
                            .iter()
                            .filter_map(|idx| manifest.artifacts.get(*idx).cloned())
                            .collect();
                        if artifacts.is_empty() {
                            status = "no filtered artifacts to sync".into();
                        } else {
                            let count = artifacts.len();
                            task = Some(spawn_sync_task(
                                payloads.clone(),
                                artifacts,
                                format!("synced {count} filtered artifact(s)"),
                            ));
                            progress_line =
                                format!("task: queued bulk sync for {count} artifact(s)");
                            status = "bulk sync started".into();
                        }
                    }
                    KeyCode::Char('v') => {
                        if task.is_some() {
                            status = "task is running; wait for it to finish".into();
                            continue;
                        }
                        task = Some(spawn_verify_task(repo.clone()));
                        progress_line = "task: queued verify".into();
                        status = "verify started".into();
                    }
                    KeyCode::Enter => {
                        if task.is_some() {
                            status = "task is running; wait for it to finish".into();
                            continue;
                        }
                        let choice = tui_prompt_select(
                            &mut terminal,
                            "artifact action",
                            &[
                                "sync selected",
                                "sync filtered view",
                                "verify catalog",
                                "toggle active",
                                "copy filename",
                                "copy path",
                                "copy provenance",
                                "copy resolved URL",
                                "cancel",
                            ],
                        )?;
                        match choice {
                            Some(0) => {
                                if let Some(actual_idx) = visible_indices.get(selected_visible) {
                                    let artifact = manifest.artifacts[*actual_idx].clone();
                                    task = Some(spawn_sync_task(
                                        payloads.clone(),
                                        vec![artifact.clone()],
                                        format!("synced {}", artifact.filename),
                                    ));
                                    progress_line =
                                        format!("task: queued sync for {}", artifact.filename);
                                    status = "sync started".into();
                                }
                            }
                            Some(1) => {
                                let artifacts: Vec<Artifact> = visible_indices
                                    .iter()
                                    .filter_map(|idx| manifest.artifacts.get(*idx).cloned())
                                    .collect();
                                if artifacts.is_empty() {
                                    status = "no filtered artifacts to sync".into();
                                } else {
                                    let count = artifacts.len();
                                    task = Some(spawn_sync_task(
                                        payloads.clone(),
                                        artifacts,
                                        format!("synced {count} filtered artifact(s)"),
                                    ));
                                    progress_line =
                                        format!("task: queued bulk sync for {count} artifact(s)");
                                    status = "bulk sync started".into();
                                }
                            }
                            Some(2) => {
                                task = Some(spawn_verify_task(repo.clone()));
                                progress_line = "task: queued verify".into();
                                status = "verify started".into();
                            }
                            Some(3) => {
                                if let Some(actual_idx) =
                                    visible_indices.get(selected_visible).copied()
                                    && let Some(artifact) = manifest.artifacts.get_mut(actual_idx)
                                {
                                    artifact.active = !artifact.active;
                                    let active = artifact.active;
                                    let filename = artifact.filename.clone();
                                    let _ = artifact;
                                    save_manifest(repo, &manifest)?;
                                    status = format!("active={} for {}", active, filename);
                                }
                            }
                            Some(4) => {
                                if let Some(actual_idx) = visible_indices.get(selected_visible) {
                                    let artifact = &manifest.artifacts[*actual_idx];
                                    status = copy_status("filename", &artifact.filename);
                                }
                            }
                            Some(5) => {
                                if let Some(actual_idx) = visible_indices.get(selected_visible) {
                                    let artifact = &manifest.artifacts[*actual_idx];
                                    let artifact_state =
                                        local_payload_state(&payloads, &metadata, artifact);
                                    let value = artifact_state
                                        .recorded_local_path
                                        .or(artifact_state.expected_local_path)
                                        .unwrap_or_else(|| "-".into());
                                    status = copy_status("path", &value);
                                }
                            }
                            Some(6) => {
                                if let Some(actual_idx) = visible_indices.get(selected_visible) {
                                    let artifact = &manifest.artifacts[*actual_idx];
                                    let value = artifact
                                        .provenance
                                        .uri
                                        .clone()
                                        .or_else(|| artifact.provenance.repo.clone())
                                        .unwrap_or_else(|| "-".into());
                                    status = copy_status("provenance", &value);
                                }
                            }
                            Some(7) => {
                                if let Some(actual_idx) = visible_indices.get(selected_visible) {
                                    let artifact = &manifest.artifacts[*actual_idx];
                                    let value = backend_sync()
                                        .resolve_url(&artifact.filename, Some(&artifact.platform))
                                        .unwrap_or_else(|err| format!("resolve-url failed: {err}"));
                                    status = if value.starts_with("resolve-url failed:") {
                                        value
                                    } else {
                                        copy_status("url", &value)
                                    };
                                }
                            }
                            _ => {
                                status = "action menu cancelled".into();
                            }
                        }
                    }
                    KeyCode::Char('a') => {
                        if task.is_some() {
                            status = "task is running; wait for it to finish".into();
                            continue;
                        }
                        if let Some(actual_idx) = visible_indices.get(selected_visible).copied()
                            && let Some(artifact) = manifest.artifacts.get_mut(actual_idx)
                        {
                            artifact.active = !artifact.active;
                            let active = artifact.active;
                            let filename = artifact.filename.clone();
                            let _ = artifact;
                            save_manifest(repo, &manifest)?;
                            status = format!("active={} for {}", active, filename);
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if visible_indices.is_empty() {
                            state.select(Some(0));
                            continue;
                        }
                        let next = match state.selected() {
                            Some(i) if i + 1 < visible_indices.len() => i + 1,
                            _ => visible_indices.len() - 1,
                        };
                        state.select(Some(next));
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if visible_indices.is_empty() {
                            state.select(Some(0));
                            continue;
                        }
                        let next = match state.selected() {
                            Some(i) if i > 0 => i - 1,
                            _ => 0,
                        };
                        state.select(Some(next));
                    }
                    KeyCode::Char('y') => {
                        if let Some(actual_idx) = visible_indices.get(selected_visible) {
                            let artifact = &manifest.artifacts[*actual_idx];
                            status = copy_status("filename", &artifact.filename);
                        }
                    }
                    KeyCode::Char('p') => {
                        if let Some(actual_idx) = visible_indices.get(selected_visible) {
                            let artifact = &manifest.artifacts[*actual_idx];
                            let artifact_state =
                                local_payload_state(&payloads, &metadata, artifact);
                            let value = artifact_state
                                .recorded_local_path
                                .or(artifact_state.expected_local_path)
                                .unwrap_or_else(|| "-".into());
                            status = copy_status("path", &value);
                        }
                    }
                    KeyCode::Char('u') => {
                        if let Some(actual_idx) = visible_indices.get(selected_visible) {
                            let artifact = &manifest.artifacts[*actual_idx];
                            let value = artifact
                                .provenance
                                .uri
                                .clone()
                                .or_else(|| artifact.provenance.repo.clone())
                                .unwrap_or_else(|| "-".into());
                            status = copy_status("provenance", &value);
                        }
                    }
                    KeyCode::Char('r') => {
                        if let Some(actual_idx) = visible_indices.get(selected_visible) {
                            let artifact = &manifest.artifacts[*actual_idx];
                            let value = backend_sync()
                                .resolve_url(&artifact.filename, Some(&artifact.platform))
                                .unwrap_or_else(|err| format!("resolve-url failed: {err}"));
                            status = if value.starts_with("resolve-url failed:") {
                                value
                            } else {
                                copy_status("url", &value)
                            };
                        }
                    }
                    _ => {}
                }
            }
        }
    };

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn home_dir() -> PathBuf {
    env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn xdg_data_home() -> PathBuf {
    env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".local/share"))
}

fn xdg_config_home() -> PathBuf {
    env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".config"))
}

fn config_file_path() -> PathBuf {
    xdg_config_home().join("artifact-catalog/config.yaml")
}

fn default_catalog_root() -> PathBuf {
    xdg_data_home().join("artifact-catalog")
}

fn expand_home_path(path: PathBuf) -> PathBuf {
    let raw = path.to_string_lossy();
    if raw == "~" {
        return home_dir();
    }
    if let Some(stripped) = raw.strip_prefix("~/") {
        return home_dir().join(stripped);
    }
    path
}

fn load_config() -> Result<AppConfig> {
    let path = config_file_path();
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("reading config file {}", path.display()))?;
    serde_json::from_str(&raw).context("parsing config file")
}

fn repo_checkout_root() -> Result<Option<PathBuf>> {
    let cwd = env::current_dir().context("could not determine current directory")?;
    if cwd.join("manifests/artifacts.yaml").exists() {
        return Ok(Some(cwd));
    }

    let root = env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .filter(|p| p.join("manifests/artifacts.yaml").exists());
    Ok(root)
}

fn should_be_executable(filename: &str) -> bool {
    matches!(
        filename,
        f if f.ends_with(".sh")
            || f.ends_with(".py")
            || f.ends_with(".pl")
            || f.ends_with(".rb")
            || f.ends_with(".elf")
            || f.ends_with(".bin")
            || f == "pspy64"
            || f == "socat"
            || f.ends_with(".exe")
    )
}

fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms)?;
    }
    Ok(())
}

fn load_local_metadata(payloads: &PayloadPaths) -> Result<LocalMetadata> {
    if !payloads.metadata_file.exists() {
        return Ok(LocalMetadata { artifacts: vec![] });
    }
    let raw = fs::read_to_string(&payloads.metadata_file)?;
    Ok(serde_json::from_str(&raw)?)
}

fn save_local_metadata(payloads: &PayloadPaths, metadata: &LocalMetadata) -> Result<()> {
    fs::create_dir_all(&payloads.metadata_dir)?;
    fs::write(
        &payloads.metadata_file,
        serde_json::to_string_pretty(metadata)? + "\n",
    )?;
    Ok(())
}

fn upsert_local_metadata(metadata: &mut LocalMetadata, record: LocalArtifactRecord) {
    if let Some(existing) = metadata.artifacts.iter_mut().find(|m| {
        m.filename == record.filename
            && m.platform == record.platform
            && m.category == record.category
    }) {
        *existing = record;
    } else {
        metadata.artifacts.push(record);
    }
    metadata.artifacts.sort_by(|a, b| {
        (&a.platform, &a.category, &a.filename).cmp(&(&b.platform, &b.category, &b.filename))
    });
}

fn git_output(repo_root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(["-C", repo_root.to_str().unwrap_or(".")])
        .args(args)
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[derive(Debug, Clone, Default, Serialize)]
struct LocalPayloadState {
    has_local_record: bool,
    present: bool,
    verified: bool,
    stale: bool,
    expected_local_path: Option<String>,
    recorded_local_path: Option<String>,
}

struct TuiTask {
    receiver: Receiver<TaskUpdate>,
}

enum TaskUpdate {
    Progress(String),
    Finished(Result<String, String>),
}

impl LocalPayloadState {
    fn status_label(&self) -> &'static str {
        if self.verified {
            "verified"
        } else if self.stale {
            "stale"
        } else if self.present {
            "present"
        } else {
            "missing"
        }
    }
}

fn local_record_for_artifact<'a>(
    metadata: &'a LocalMetadata,
    artifact: &Artifact,
) -> Option<&'a LocalArtifactRecord> {
    metadata.artifacts.iter().find(|m| {
        m.filename == artifact.filename
            && m.platform == artifact.platform
            && m.category == artifact.category
    })
}

fn local_payload_state(
    payloads: &PayloadPaths,
    metadata: &LocalMetadata,
    artifact: &Artifact,
) -> LocalPayloadState {
    let dest = payloads.destination_for(artifact).ok();
    let expected_dest = dest.as_ref().map(|p| p.display().to_string());
    let record = local_record_for_artifact(metadata, artifact);
    let has_local_record = record.is_some();
    let present = dest.as_ref().is_some_and(|path| path.is_file());
    let recorded_local_path = record.map(|record| record.local_path.clone());

    let verified = record.is_some_and(|record| {
        expected_dest
            .as_ref()
            .is_some_and(|expected| &record.local_path == expected)
            && present
            && dest
                .as_ref()
                .and_then(|path| sha256_hex(path).ok())
                .is_some_and(|digest| digest == artifact.sha256)
            && record.sha256 == artifact.sha256
            && record.version == artifact.version
    });
    let stale = !verified && (has_local_record || present);

    LocalPayloadState {
        has_local_record,
        present,
        verified,
        stale,
        expected_local_path: expected_dest,
        recorded_local_path,
    }
}

fn filtered_artifact_indices(manifest: &Manifest, filter_query: &str) -> Vec<usize> {
    if filter_query.trim().is_empty() {
        return (0..manifest.artifacts.len()).collect();
    }
    let needle = filter_query.to_ascii_lowercase();
    manifest
        .artifacts
        .iter()
        .enumerate()
        .filter(|(_, artifact)| artifact.filename.to_ascii_lowercase().contains(&needle))
        .map(|(idx, _)| idx)
        .collect()
}

fn selected_visible_index(current: Option<usize>, visible_len: usize) -> usize {
    if visible_len == 0 {
        0
    } else {
        current.unwrap_or(0).min(visible_len - 1)
    }
}

fn spawn_sync_task(
    payloads: PayloadPaths,
    artifacts: Vec<Artifact>,
    success_message: String,
) -> TuiTask {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let total = artifacts.len();
        if total == 0 {
            let _ = tx.send(TaskUpdate::Finished(Ok("no artifacts to sync".into())));
            return;
        }

        for (idx, artifact) in artifacts.into_iter().enumerate() {
            let step = idx + 1;
            let _ = tx.send(TaskUpdate::Progress(format!(
                "syncing {step}/{total}: {}",
                artifact.filename
            )));
            let args = SyncArgs {
                platform: Some(artifact.platform.clone()),
                category: Some(artifact.category.clone()),
                only_filename: Some(artifact.filename.clone()),
                dry_run: false,
            };

            let result = match BackendKind::from_env() {
                BackendKind::GithubReleases => {
                    github_sync_with_progress(&payloads, &args, |message| {
                        let _ = tx.send(TaskUpdate::Progress(message));
                    })
                    .map(|_| ())
                }
                BackendKind::OciRegistry => OciRegistryBackend.sync(&payloads, &args),
            };

            if let Err(err) = result {
                let _ = tx.send(TaskUpdate::Finished(Err(format!(
                    "sync failed for {}: {}",
                    artifact.filename, err
                ))));
                return;
            }
        }

        let _ = tx.send(TaskUpdate::Finished(Ok(success_message)));
    });
    TuiTask { receiver: rx }
}

fn spawn_verify_task(repo: RepoPaths) -> TuiTask {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = verify_with_progress(&repo, |message| {
            let _ = tx.send(TaskUpdate::Progress(message));
        })
        .map(|count| format!("verified {count} artifact(s)"))
        .map_err(|err| format!("verify failed: {err}"));
        let _ = tx.send(TaskUpdate::Finished(result));
    });
    TuiTask { receiver: rx }
}

fn copy_status(label: &str, value: &str) -> String {
    match copy_to_clipboard(value) {
        Ok(()) => format!("{label} copied: {value}"),
        Err(err) => format!("{label}: {value} (clipboard unavailable: {err})"),
    }
}

fn copy_to_clipboard(value: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        return copy_with_command("pbcopy", &[], value);
    }

    #[cfg(target_os = "windows")]
    {
        return copy_with_command("clip", &[], value);
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let session = env::var("XDG_SESSION_TYPE").unwrap_or_default();
        if session.eq_ignore_ascii_case("wayland")
            && let Ok(()) = copy_with_command("wl-copy", &[], value)
        {
            return Ok(());
        }
        if copy_with_command("xclip", &["-selection", "clipboard"], value).is_ok() {
            return Ok(());
        }
        if copy_with_command("xsel", &["--clipboard", "--input"], value).is_ok() {
            return Ok(());
        }
        bail!("need wl-copy, xclip, or xsel");
    }
}

fn copy_with_command(command: &str, args: &[&str], value: &str) -> Result<()> {
    use std::io::Write as _;
    use std::process::Stdio;

    let mut child = Command::new(command)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("spawn {command}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("failed to open stdin for {command}"))?;
    stdin.write_all(value.as_bytes())?;
    drop(stdin);

    let status = child.wait()?;
    if !status.success() {
        bail!("{command} exited with status {}", status);
    }
    Ok(())
}

fn tui_prompt_select(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    prompt: &str,
    items: &[&str],
) -> Result<Option<usize>> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    let result = Select::new()
        .with_prompt(prompt)
        .items(items)
        .default(0)
        .interact_opt();

    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
    enable_raw_mode()?;
    terminal.hide_cursor()?;

    Ok(result?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    fn env_lock() -> MutexGuard<'static, ()> {
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("lock env mutex")
    }

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(prefix: &str) -> Self {
            let path = env::temp_dir().join(format!(
                "{prefix}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn init_if_missing_creates_catalog_layout() {
        let temp = TestDir::new("artifact-catalog-init");
        let repo = RepoPaths::from_root(temp.path.clone());

        repo.init_if_missing().expect("initialize repo");

        assert!(repo.manifest.exists());
        assert!(repo.checksums.exists());
        assert!(repo.release_dir.exists());

        let manifest = load_manifest(&repo).expect("load initialized manifest");
        assert!(manifest.artifacts.is_empty());
        assert_eq!(
            fs::read_to_string(&repo.checksums).expect("read checksums"),
            ""
        );
    }

    #[test]
    fn ensure_initialized_errors_for_missing_layout() {
        let temp = TestDir::new("artifact-catalog-missing");
        let repo = RepoPaths::from_root(temp.path.clone());

        let err = repo
            .ensure_initialized()
            .expect_err("missing layout should fail");
        assert!(
            err.to_string()
                .contains("locker catalog is not initialized")
        );
    }

    #[test]
    fn discover_uses_explicit_root_override() {
        let temp = TestDir::new("artifact-catalog-root");
        let chosen = temp.path.join("chosen-root");
        fs::create_dir_all(&chosen).expect("create chosen root");

        let repo = RepoPaths::discover(Some(chosen.as_path())).expect("discover root");

        assert_eq!(repo.root, chosen);
    }

    #[test]
    fn default_catalog_root_respects_xdg_data_home() {
        let _env_guard = env_lock();
        let temp = TestDir::new("artifact-catalog-xdg");
        let xdg_home = temp.path.join("xdg-home");
        fs::create_dir_all(&xdg_home).expect("create xdg home");

        unsafe {
            env::set_var("XDG_DATA_HOME", &xdg_home);
        }
        let result = default_catalog_root();
        unsafe {
            env::remove_var("XDG_DATA_HOME");
        }

        assert_eq!(result, xdg_home.join("artifact-catalog"));
    }

    #[test]
    fn config_file_path_respects_xdg_config_home() {
        let _env_guard = env_lock();
        let temp = TestDir::new("artifact-catalog-config-home");
        let xdg_home = temp.path.join("xdg-config");
        fs::create_dir_all(&xdg_home).expect("create xdg config home");

        unsafe {
            env::set_var("XDG_CONFIG_HOME", &xdg_home);
        }
        let result = config_file_path();
        unsafe {
            env::remove_var("XDG_CONFIG_HOME");
        }

        assert_eq!(result, xdg_home.join("artifact-catalog/config.yaml"));
    }

    #[test]
    fn discover_uses_catalog_root_from_config() {
        let _env_guard = env_lock();
        let temp = TestDir::new("artifact-catalog-config");
        let xdg_config_home = temp.path.join("xdg-config");
        let configured_root = temp.path.join("configured-root");
        fs::create_dir_all(xdg_config_home.join("artifact-catalog")).expect("create config dir");
        fs::create_dir_all(&configured_root).expect("create configured root");
        fs::write(
            xdg_config_home.join("artifact-catalog/config.yaml"),
            format!("{{\"catalog_root\":\"{}\"}}\n", configured_root.display()),
        )
        .expect("write config file");

        unsafe {
            env::set_var("XDG_CONFIG_HOME", &xdg_config_home);
            env::remove_var("LOCKER_ROOT");
        }
        let repo = RepoPaths::discover(None).expect("discover root from config");
        unsafe {
            env::remove_var("XDG_CONFIG_HOME");
        }

        assert_eq!(repo.root, configured_root);
    }

    #[test]
    fn discover_expands_tilde_catalog_root_from_config() {
        let _env_guard = env_lock();
        let temp = TestDir::new("artifact-catalog-config-tilde");
        let xdg_config_home = temp.path.join("xdg-config");
        fs::create_dir_all(xdg_config_home.join("artifact-catalog")).expect("create config dir");
        fs::write(
            xdg_config_home.join("artifact-catalog/config.yaml"),
            "{\"catalog_root\":\"~/catalog-root\"}\n",
        )
        .expect("write config file");

        unsafe {
            env::set_var("XDG_CONFIG_HOME", &xdg_config_home);
            env::remove_var("LOCKER_ROOT");
        }
        let repo = RepoPaths::discover(None).expect("discover root from config");
        unsafe {
            env::remove_var("XDG_CONFIG_HOME");
        }

        assert_eq!(repo.root, home_dir().join("catalog-root"));
    }

    #[test]
    fn backend_kind_uses_config_default() {
        let _env_guard = env_lock();
        let temp = TestDir::new("artifact-catalog-backend-config");
        let xdg_config_home = temp.path.join("xdg-config");
        fs::create_dir_all(xdg_config_home.join("artifact-catalog")).expect("create config dir");
        fs::write(
            xdg_config_home.join("artifact-catalog/config.yaml"),
            "{\"default_backend\":\"oci-registry\"}\n",
        )
        .expect("write config file");

        unsafe {
            env::set_var("XDG_CONFIG_HOME", &xdg_config_home);
            env::remove_var("LOCKER_BACKEND");
        }
        let backend = BackendKind::from_env();
        unsafe {
            env::remove_var("XDG_CONFIG_HOME");
        }

        assert!(matches!(backend, BackendKind::OciRegistry));
    }

    #[test]
    fn backend_kind_defaults_to_oci_registry() {
        let _env_guard = env_lock();
        let temp = TestDir::new("artifact-catalog-backend-default");
        let xdg_config_home = temp.path.join("xdg-config");
        fs::create_dir_all(&xdg_config_home).expect("create empty xdg config home");

        unsafe {
            env::set_var("XDG_CONFIG_HOME", &xdg_config_home);
            env::remove_var("LOCKER_BACKEND");
        }
        let backend = BackendKind::from_env();
        unsafe {
            env::remove_var("XDG_CONFIG_HOME");
        }

        assert!(matches!(backend, BackendKind::OciRegistry));
    }

    fn test_payload_paths(root: &Path) -> PayloadPaths {
        let windows_dir = root.join("windows");
        let metadata_dir = root.join(".locker");
        PayloadPaths {
            root: root.to_path_buf(),
            linux_dir: root.join("linux"),
            windows_dir: windows_dir.clone(),
            windows_bin_dir: windows_dir.join("bin"),
            windows_scripts_dir: windows_dir.join("scripts"),
            windows_webshells_dir: windows_dir.join("webshells"),
            metadata_dir: metadata_dir.clone(),
            metadata_file: metadata_dir.join("artifacts.json"),
        }
    }

    fn sample_artifact(sha256: &str) -> Artifact {
        Artifact {
            platform: "linux".into(),
            category: "bin".into(),
            filename: "pspy64".into(),
            version: "v1.2.1".into(),
            provenance: Provenance {
                kind: ProvenanceKind::Download,
                uri: Some("https://github.com/example/pspy/releases/download/v1.2.1/pspy64".into()),
                repo: Some("https://github.com/example/pspy".into()),
                tag: Some("v1.2.1".into()),
                commit: None,
                asset_name: Some("pspy64".into()),
                archive_path: None,
                build_method: None,
                notes: None,
            },
            sha256: sha256.into(),
            object_name: "linux--bin--pspy64".into(),
            active: true,
        }
    }

    #[test]
    fn example_manifest_uses_new_provenance_schema() {
        let example_manifest =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/artifacts.example.yaml");
        let raw = fs::read_to_string(&example_manifest).expect("read example manifest");
        let manifest: Manifest = serde_json::from_str(&raw).expect("parse example manifest");

        assert!(!manifest.artifacts.is_empty());
        assert!(
            manifest
                .artifacts
                .iter()
                .all(|artifact| !artifact.object_name.is_empty())
        );
        assert!(
            manifest
                .artifacts
                .iter()
                .all(|artifact| matches!(artifact.provenance.kind, ProvenanceKind::Download))
        );
    }

    #[test]
    fn built_artifact_without_notes_warns() {
        let artifact = Artifact {
            platform: "windows".into(),
            category: "bin".into(),
            filename: "tool.exe".into(),
            version: "git-a1b2c3d-x64".into(),
            provenance: Provenance {
                kind: ProvenanceKind::Built,
                uri: None,
                repo: Some("https://github.com/example/tool".into()),
                tag: None,
                commit: None,
                asset_name: Some("tool.exe".into()),
                archive_path: None,
                build_method: Some("cargo build --release".into()),
                notes: None,
            },
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            object_name: "windows--bin--tool.exe".into(),
            active: true,
        };

        let warnings = artifact_metadata_warnings(&artifact);

        assert_eq!(warnings.len(), 2);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("provenance.uri is missing"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("notes are missing"))
        );
    }

    #[test]
    fn built_artifact_without_pinned_commit_is_invalid() {
        let artifact = Artifact {
            platform: "windows".into(),
            category: "bin".into(),
            filename: "tool.exe".into(),
            version: "git-a1b2c3d-x64".into(),
            provenance: Provenance {
                kind: ProvenanceKind::Built,
                uri: Some("/tmp/tool.exe".into()),
                repo: Some("https://github.com/example/tool".into()),
                tag: None,
                commit: None,
                asset_name: Some("tool.exe".into()),
                archive_path: None,
                build_method: Some("cargo build --release".into()),
                notes: Some("built locally".into()),
            },
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            object_name: "windows--bin--tool.exe".into(),
            active: true,
        };

        let problems = artifact_metadata_problems(&artifact);

        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("provenance.commit"));
    }

    #[test]
    fn local_artifact_with_upstream_fields_warns() {
        let artifact = Artifact {
            platform: "windows".into(),
            category: "bin".into(),
            filename: "tool.exe".into(),
            version: "2026.04.28".into(),
            provenance: Provenance {
                kind: ProvenanceKind::Local,
                uri: Some("/tmp/tool.exe".into()),
                repo: Some("https://github.com/example/tool".into()),
                tag: Some("v1.0.0".into()),
                commit: Some("a1b2c3d".into()),
                asset_name: Some("tool.exe".into()),
                archive_path: None,
                build_method: None,
                notes: None,
            },
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            object_name: "windows--bin--tool.exe".into(),
            active: true,
        };

        let warnings = artifact_metadata_warnings(&artifact);

        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("upstream provenance fields"));
    }

    #[test]
    fn manifest_with_duplicate_object_name_is_invalid() {
        let artifact =
            sample_artifact("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");
        let manifest = Manifest {
            artifacts: vec![artifact.clone(), artifact],
        };

        let err = ensure_valid_manifest(&manifest).expect_err("duplicate object_name should fail");
        assert!(err.to_string().contains("duplicate object_name"));
    }

    #[test]
    fn manifest_with_placeholder_download_url_is_invalid() {
        let mut artifact =
            sample_artifact("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");
        artifact.provenance.uri = Some("https://example.invalid/download".into());

        let problems = artifact_metadata_problems(&artifact);

        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("placeholder-like"));
    }

    #[test]
    fn parse_checksums_and_validate_manifest_round_trip() {
        let artifact =
            sample_artifact("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");
        let manifest = Manifest {
            artifacts: vec![artifact.clone()],
        };
        let raw = format!("{}  {}\n", artifact.sha256, artifact.object_name);

        let checksums = parse_checksums(&raw).expect("parse checksums");
        validate_manifest_checksums(&manifest, &checksums).expect("validate manifest checksums");
    }

    #[test]
    fn validate_manifest_checksums_rejects_mismatch() {
        let artifact =
            sample_artifact("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");
        let manifest = Manifest {
            artifacts: vec![artifact],
        };
        let checksums = parse_checksums(
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff  linux--bin--pspy64\n",
        )
        .expect("parse mismatched checksums");

        let err =
            validate_manifest_checksums(&manifest, &checksums).expect_err("mismatch should fail");
        assert!(
            err.to_string()
                .contains("checksum file does not match manifest")
        );
    }

    #[test]
    fn pulled_oci_path_uses_object_name() {
        let artifact =
            sample_artifact("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");
        let output_dir = PathBuf::from("/tmp/pull-output");

        assert_eq!(
            artifact.pulled_oci_path(&output_dir),
            output_dir.join("linux--bin--pspy64")
        );
    }

    #[test]
    fn oci_metadata_tags_include_stable_and_versioned_refs() {
        let args = PublishArgs {
            tag: "v2026-04-28".into(),
            title: None,
            notes_file: None,
        };

        let (manifest_tags, checksum_tags) = oci_metadata_tags(&args);

        assert_eq!(
            manifest_tags,
            vec![
                "artifacts-manifest".to_string(),
                "v2026-04-28-manifest".to_string()
            ]
        );
        assert_eq!(
            checksum_tags,
            vec![
                "artifacts-sha256sums".to_string(),
                "v2026-04-28-sha256sums".to_string()
            ]
        );
    }

    #[test]
    fn prompt_required_requires_explicit_value_in_yes_mode() {
        let err = prompt_required("version", None, true).expect_err("version should be required");
        assert!(err.to_string().contains("missing required version"));
    }

    #[test]
    fn local_payload_state_marks_verified_file() {
        let temp = TestDir::new("artifact-catalog-local-verified");
        let payloads = test_payload_paths(&temp.path);
        payloads.ensure_dirs().expect("create payload dirs");
        let expected_bytes = b"expected-bytes";
        let expected_sha = format!("{:x}", Sha256::digest(expected_bytes));
        let artifact = sample_artifact(&expected_sha);
        let dest = payloads
            .destination_for(&artifact)
            .expect("resolve destination");
        fs::write(&dest, expected_bytes).expect("write expected payload");
        let metadata = LocalMetadata {
            artifacts: vec![LocalArtifactRecord {
                filename: artifact.filename.clone(),
                platform: artifact.platform.clone(),
                category: artifact.category.clone(),
                version: artifact.version.clone(),
                provenance: artifact.provenance.clone(),
                sha256: artifact.sha256.clone(),
                object_name: artifact.object_name.clone(),
                local_path: dest.display().to_string(),
                synced_at_epoch: 1,
            }],
        };

        let state = local_payload_state(&payloads, &metadata, &artifact);

        assert!(state.present);
        assert!(state.verified);
        assert!(!state.stale);
    }

    #[test]
    fn local_payload_state_marks_tampered_file_stale() {
        let temp = TestDir::new("artifact-catalog-local-stale");
        let payloads = test_payload_paths(&temp.path);
        payloads.ensure_dirs().expect("create payload dirs");
        let expected_sha = format!("{:x}", Sha256::digest(b"expected-bytes"));
        let artifact = sample_artifact(&expected_sha);
        let dest = payloads
            .destination_for(&artifact)
            .expect("resolve destination");
        fs::write(&dest, b"tampered-bytes").expect("write tampered payload");
        let metadata = LocalMetadata {
            artifacts: vec![LocalArtifactRecord {
                filename: artifact.filename.clone(),
                platform: artifact.platform.clone(),
                category: artifact.category.clone(),
                version: artifact.version.clone(),
                provenance: artifact.provenance.clone(),
                sha256: artifact.sha256.clone(),
                object_name: artifact.object_name.clone(),
                local_path: dest.display().to_string(),
                synced_at_epoch: 1,
            }],
        };

        let state = local_payload_state(&payloads, &metadata, &artifact);

        assert!(state.present);
        assert!(!state.verified);
        assert!(state.stale);
    }

    #[test]
    fn local_payload_state_marks_present_file_without_metadata_stale() {
        let temp = TestDir::new("artifact-catalog-local-untracked");
        let payloads = test_payload_paths(&temp.path);
        payloads.ensure_dirs().expect("create payload dirs");
        let expected_bytes = b"expected-bytes";
        let expected_sha = format!("{:x}", Sha256::digest(expected_bytes));
        let artifact = sample_artifact(&expected_sha);
        let dest = payloads
            .destination_for(&artifact)
            .expect("resolve destination");
        fs::write(&dest, expected_bytes).expect("write payload");

        let state = local_payload_state(&payloads, &LocalMetadata { artifacts: vec![] }, &artifact);

        assert!(state.present);
        assert!(!state.verified);
        assert!(state.stale);
    }

    #[test]
    fn local_payload_state_marks_metadata_path_mismatch_stale() {
        let temp = TestDir::new("artifact-catalog-local-path-mismatch");
        let payloads = test_payload_paths(&temp.path);
        payloads.ensure_dirs().expect("create payload dirs");
        let expected_bytes = b"expected-bytes";
        let expected_sha = format!("{:x}", Sha256::digest(expected_bytes));
        let artifact = sample_artifact(&expected_sha);
        let dest = payloads
            .destination_for(&artifact)
            .expect("resolve destination");
        fs::write(&dest, expected_bytes).expect("write payload");
        let metadata = LocalMetadata {
            artifacts: vec![LocalArtifactRecord {
                filename: artifact.filename.clone(),
                platform: artifact.platform.clone(),
                category: artifact.category.clone(),
                version: artifact.version.clone(),
                provenance: artifact.provenance.clone(),
                sha256: artifact.sha256.clone(),
                object_name: artifact.object_name.clone(),
                local_path: temp.path.join("somewhere-else").display().to_string(),
                synced_at_epoch: 1,
            }],
        };

        let state = local_payload_state(&payloads, &metadata, &artifact);

        assert!(state.present);
        assert!(!state.verified);
        assert!(state.stale);
    }

    #[test]
    fn payload_paths_use_config_default() {
        let _env_guard = env_lock();
        let temp = TestDir::new("artifact-catalog-payloads-config");
        let xdg_config_home = temp.path.join("xdg-config");
        let payload_root = temp.path.join("payload-root");
        fs::create_dir_all(xdg_config_home.join("artifact-catalog")).expect("create config dir");
        fs::write(
            xdg_config_home.join("artifact-catalog/config.yaml"),
            format!("{{\"payloads_dir\":\"{}\"}}\n", payload_root.display()),
        )
        .expect("write config file");

        unsafe {
            env::set_var("XDG_CONFIG_HOME", &xdg_config_home);
            env::remove_var("PAYLOADS_DIR");
        }
        let payloads = PayloadPaths::discover().expect("discover payload paths");
        unsafe {
            env::remove_var("XDG_CONFIG_HOME");
        }

        assert_eq!(payloads.root, payload_root);
    }

    #[test]
    fn payload_paths_expand_tilde_from_config() {
        let _env_guard = env_lock();
        let temp = TestDir::new("artifact-catalog-payloads-tilde");
        let xdg_config_home = temp.path.join("xdg-config");
        fs::create_dir_all(xdg_config_home.join("artifact-catalog")).expect("create config dir");
        fs::write(
            xdg_config_home.join("artifact-catalog/config.yaml"),
            "{\"payloads_dir\":\"~/payload-root\"}\n",
        )
        .expect("write config file");

        unsafe {
            env::set_var("XDG_CONFIG_HOME", &xdg_config_home);
            env::remove_var("PAYLOADS_DIR");
        }
        let payloads = PayloadPaths::discover().expect("discover payload paths");
        unsafe {
            env::remove_var("XDG_CONFIG_HOME");
        }

        assert_eq!(payloads.root, home_dir().join("payload-root"));
    }
}
