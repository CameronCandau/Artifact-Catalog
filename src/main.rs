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
    env, fs,
    io,
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
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
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
    #[arg(long = "source-type")]
    source_type: Option<String>,
    #[arg(long = "source-ref")]
    source_ref: Option<String>,
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
    source_type: String,
    active: bool,
    staged: bool,
    synced: bool,
    staged_path: String,
    synced_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    artifacts: Vec<Artifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Artifact {
    platform: String,
    category: String,
    filename: String,
    version: String,
    source_type: String,
    source_ref: String,
    sha256: String,
    release_asset_name: String,
    active: bool,
}

impl Artifact {
    fn staged_path(&self, repo: &RepoPaths) -> PathBuf {
        repo.release_dir.join(&self.release_asset_name)
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
    source_type: String,
    source_ref: String,
    sha256: String,
    release_asset_name: String,
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

impl RepoPaths {
    fn discover() -> Result<Self> {
        let cwd = env::current_dir().context("could not determine current directory")?;
        let root = if cwd.join("manifests/artifacts.yaml").exists() {
            cwd
        } else {
            env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(Path::to_path_buf))
                .and_then(|p| p.parent().map(Path::to_path_buf))
                .filter(|p| p.join("manifests/artifacts.yaml").exists())
                .ok_or_else(|| anyhow!("run inside artifact-catalog repo or from installed binary"))?
        };
        Ok(Self {
            manifest: root.join("manifests/artifacts.yaml"),
            checksums: root.join("checksums/sha256sums.txt"),
            release_dir: root.join("staging/release-assets"),
            root,
        })
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
        let root = env::var("PAYLOADS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home_dir().join("tools/payloads"));
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
        match env::var("LOCKER_BACKEND")
            .unwrap_or_else(|_| "github-releases".into())
            .as_str()
        {
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

fn github_owner() -> String {
    env::var("ARTIFACT_CATALOG_OWNER")
        .or_else(|_| env::var("GITHUB_OWNER"))
        .unwrap_or_else(|_| "CameronCandau".into())
}

fn github_repo() -> String {
    env::var("ARTIFACT_CATALOG_REPO")
        .or_else(|_| env::var("GITHUB_REPO"))
        .unwrap_or_else(|_| "Artifact-Catalog".into())
}

fn github_base_url() -> String {
    env::var("ARTIFACT_CATALOG_BASE_URL").unwrap_or_else(|_| {
        format!(
            "https://github.com/{}/{}/releases/latest/download",
            github_owner(),
            github_repo()
        )
    })
}

fn github_manifest_url() -> String {
    env::var("ARTIFACT_CATALOG_MANIFEST_URL")
        .unwrap_or_else(|_| format!("{}/artifacts.yaml", github_base_url()))
}

fn github_checksums_url() -> String {
    env::var("ARTIFACT_CATALOG_CHECKSUMS_URL")
        .unwrap_or_else(|_| format!("{}/sha256sums.txt", github_base_url()))
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
            if let Some(existing) = assets.iter().find(|a| a["name"].as_str() == Some(&asset_name))
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
        let repo = RepoPaths::discover()?;
        let manifest = load_manifest(&repo)?;
        let artifact = manifest
            .artifacts
            .into_iter()
            .find(|a| a.filename == filename && a.active && platform.is_none_or(|p| a.platform == p))
            .ok_or_else(|| anyhow!("artifact not found: {filename}"))?;
        Ok(format!(
            "{}/{}",
            github_base_url().trim_end_matches('/'),
            artifact.release_asset_name
        ))
    }
}

impl PublishBackend for OciRegistryBackend {
    fn publish(&self, _repo: &RepoPaths, _manifest: &Manifest, _args: &PublishArgs) -> Result<()> {
        bail!(
            "OCI backend is not implemented yet. Keep using GitHub Releases now; Harbor/ORAS can slot in later."
        )
    }
}

impl SyncBackend for OciRegistryBackend {
    fn sync(&self, _payloads: &PayloadPaths, _args: &SyncArgs) -> Result<()> {
        bail!("OCI sync backend is not implemented yet.")
    }

    fn resolve_url(&self, _filename: &str, _platform: Option<&str>) -> Result<String> {
        bail!("resolve-url for oci backend is not implemented yet")
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let repo = RepoPaths::discover()?;
    match cli.command {
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
    let raw = fs::read_to_string(&repo.manifest).context("reading manifest")?;
    Ok(serde_json::from_str(&raw).context("parsing manifest")?)
}

fn save_manifest(repo: &RepoPaths, manifest: &Manifest) -> Result<()> {
    fs::write(&repo.manifest, serde_json::to_string_pretty(manifest)? + "\n")?;
    Ok(())
}

fn infer_source_type(source: &str) -> &'static str {
    if source.starts_with("https://github.com/") && source.contains("/releases/download/") {
        "github-release"
    } else if source.starts_with("http://") || source.starts_with("https://") {
        "url"
    } else {
        "local"
    }
}

fn basename_from_source(source: &str) -> String {
    if source.starts_with("http://") || source.starts_with("https://") {
        source.rsplit('/').next().unwrap_or("artifact.bin").to_string()
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
    let mut lines = Vec::new();
    for artifact in &manifest.artifacts {
        let staged = artifact.staged_path(repo);
        if staged.is_file() {
            lines.push(format!(
                "{}  {}",
                artifact.sha256, artifact.release_asset_name
            ));
        }
    }
    lines.sort();
    fs::write(
        &repo.checksums,
        lines.join("\n") + if lines.is_empty() { "" } else { "\n" },
    )?;
    Ok(())
}

fn github_sync_with_progress<F>(payloads: &PayloadPaths, args: &SyncArgs, mut progress: F) -> Result<usize>
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
    let _checksums = client
        .get(github_checksums_url())
        .send()?
        .error_for_status()?
        .text()?;

    progress(format!("payload root: {}", payloads.root.display()));
    progress(format!("artifact base URL: {}", github_base_url()));

    let artifacts: Vec<Artifact> = manifest
        .artifacts
        .into_iter()
        .filter(|a| a.active)
        .filter(|artifact| args.platform.as_ref().is_none_or(|platform| &artifact.platform == platform))
        .filter(|artifact| args.category.as_ref().is_none_or(|category| &artifact.category == category))
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
            progress(format!("no active artifacts matched platform filter: {platform}"));
        } else if let Some(category) = &args.category {
            progress(format!("no active artifacts matched category filter: {category}"));
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
        let url = format!("{}/{}", github_base_url(), artifact.release_asset_name);
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
        if digest != artifact.sha256 {
            bail!(
                "SHA256 mismatch for {}: expected {}, got {}",
                artifact.filename,
                artifact.sha256,
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
                source_type: artifact.source_type.clone(),
                source_ref: artifact.source_ref.clone(),
                sha256: artifact.sha256.clone(),
                release_asset_name: artifact.release_asset_name.clone(),
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

fn copy_or_download(source: &str) -> Result<(PathBuf, String, String)> {
    if source.starts_with("http://") || source.starts_with("https://") {
        let client = Client::builder().build()?;
        let response = client.get(source).send()?.error_for_status()?;
        let tmpdir = env::temp_dir().join(format!("locker-{}", std::process::id()));
        fs::create_dir_all(&tmpdir)?;
        let filename = basename_from_source(source);
        let path = tmpdir.join(filename);
        fs::write(&path, response.bytes()?)?;
        Ok((path, source.to_string(), infer_source_type(source).to_string()))
    } else {
        let path = PathBuf::from(source);
        if !path.is_file() {
            bail!("source file not found: {source}");
        }
        Ok((
            path.canonicalize().unwrap_or(path),
            source.to_string(),
            "local".to_string(),
        ))
    }
}

fn add_command(repo: &RepoPaths, args: AddArgs) -> Result<()> {
    fs::create_dir_all(&repo.release_dir)?;
    let (source_path, source_identity, inferred_source_type) = copy_or_download(&args.source)?;
    let filename = args
        .filename
        .clone()
        .unwrap_or_else(|| basename_from_source(&args.source));
    let inferred_platform = infer_platform_from_filename(&filename).map(str::to_string);
    let inferred_category = Some(infer_category_from_filename(&filename).to_string());
    let inferred_version = infer_version_from_source(&args.source);

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
        None => prompt_or_default("version", inferred_version, "manual", args.yes)?,
    };
    let source_type = args.source_type.unwrap_or(inferred_source_type);
    let source_ref = args.source_ref.unwrap_or_else(|| {
        if source_identity.starts_with("http://") || source_identity.starts_with("https://") {
            source_identity.clone()
        } else {
            source_path.display().to_string()
        }
    });
    let release_asset_name = format!("{platform}--{category}--{filename}");
    let dest_path = repo.release_dir.join(&release_asset_name);
    fs::copy(&source_path, &dest_path)?;
    let sha256 = sha256_hex(&dest_path)?;

    let artifact = Artifact {
        platform,
        category,
        filename,
        version,
        source_type,
        source_ref,
        sha256,
        release_asset_name,
        active: !args.inactive,
    };

    let mut manifest = load_manifest(repo)?;
    let mut updated_existing = false;
    if let Some(existing) = manifest
        .artifacts
        .iter_mut()
        .find(|a| a.release_asset_name == artifact.release_asset_name)
    {
        *existing = artifact.clone();
        updated_existing = true;
    } else {
        manifest.artifacts.push(artifact.clone());
    }
    manifest.artifacts.sort_by(|a, b| {
        (&a.platform, &a.category, &a.filename).cmp(&(&b.platform, &b.category, &b.filename))
    });
    save_manifest(repo, &manifest)?;
    rebuild_checksums(repo, &manifest)?;

    println!(
        "[+] {} {} -> {}",
        if updated_existing { "Updated" } else { "Added" },
        args.source,
        artifact.release_asset_name
    );
    println!("[+] Staged at {}", dest_path.display());
    println!("[+] Version: {}", artifact.version);
    println!("[+] Source ref: {}", artifact.source_ref);
    println!("[+] Next: locker verify");
    Ok(())
}

fn build_artifact_views(repo: &RepoPaths, manifest: Manifest, payloads: &PayloadPaths) -> Vec<ArtifactView> {
    let metadata = load_local_metadata(payloads).unwrap_or(LocalMetadata { artifacts: vec![] });
    manifest
        .artifacts
        .into_iter()
        .map(|artifact| {
            let staged_path = artifact.staged_path(repo);
            let synced_path = metadata
                .artifacts
                .iter()
                .find(|m| {
                    m.filename == artifact.filename
                        && m.platform == artifact.platform
                        && m.category == artifact.category
                })
                .map(|m| m.local_path.clone());
            let synced = synced_path
                .as_ref()
                .is_some_and(|path| Path::new(path).is_file());
            ArtifactView {
                filename: artifact.filename,
                platform: artifact.platform,
                category: artifact.category,
                version: artifact.version,
                source_type: artifact.source_type,
                active: artifact.active,
                staged: staged_path.is_file(),
                synced,
                staged_path: staged_path.display().to_string(),
                synced_path,
            }
        })
        .collect()
}

fn list_command(repo: &RepoPaths, args: &ListArgs) -> Result<()> {
    let manifest = load_manifest(repo)?;
    let payloads = PayloadPaths::discover()?;
    let mut views = build_artifact_views(repo, manifest, &payloads);
    views.retain(|artifact| {
        args.platform
            .as_ref()
            .is_none_or(|platform| &artifact.platform == platform)
            && args
                .category
                .as_ref()
                .is_none_or(|category| &artifact.category == category)
            && args.active.is_none_or(|active| artifact.active == active)
            && args.synced.is_none_or(|synced| artifact.synced == synced)
    });

    if args.json {
        println!("{}", serde_json::to_string_pretty(&views)?);
        return Ok(());
    }

    println!(
        "{:<32} {:<8} {:<10} {:<20} {:<15} {:<6} {:<6} {:<6}",
        "filename", "platform", "category", "version", "source_type", "active", "staged", "synced"
    );
    for artifact in views {
        println!(
            "{:<32} {:<8} {:<10} {:<20} {:<15} {:<6} {:<6} {:<6}",
            artifact.filename,
            artifact.platform,
            artifact.category,
            truncate(&artifact.version, 20),
            artifact.source_type,
            if artifact.active { "yes" } else { "no" },
            if artifact.staged { "yes" } else { "no" },
            if artifact.synced { "yes" } else { "no" }
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
    let local = metadata
        .artifacts
        .into_iter()
        .find(|m| m.filename == args.filename && m.platform == artifact.platform && m.category == artifact.category);

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "artifact": artifact,
                "staged_path": staged_path,
                "staged": staged_path.is_file(),
                "local": local,
            }))?
        );
        return Ok(());
    }

    println!("{}", serde_json::to_string_pretty(&artifact)?);
    println!("staged_path: {}", staged_path.display());
    println!("staged: {}", staged_path.is_file());
    if let Some(local) = local {
        println!("synced_path: {}", local.local_path);
        println!("synced: {}", Path::new(&local.local_path).is_file());
        println!("synced_at_epoch: {}", local.synced_at_epoch);
    }
    Ok(())
}

fn resolve_url_command(filename: &str) -> Result<()> {
    let url = backend_sync().resolve_url(filename, None)?;
    println!("{url}");
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
            bail!("missing staged asset: {}", artifact.release_asset_name);
        }
        let digest = sha256_hex(&path)?;
        if digest != artifact.sha256 {
            bail!("sha mismatch for {}", artifact.release_asset_name);
        }
        expected.push(format!(
            "{}  {}",
            artifact.sha256, artifact.release_asset_name
        ));
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

    if !repo.root.join(".git").exists() {
        problems.push("not a git repo".to_string());
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
    let payload_metadata = load_local_metadata(&payloads).unwrap_or(LocalMetadata { artifacts: vec![] });
    notes.push(format!("payload root: {}", payloads.root.display()));

    let staged_names: std::collections::BTreeSet<String> = fs::read_dir(&repo.release_dir)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(|entry| entry.ok()))
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter_map(|path| path.file_name().and_then(|s| s.to_str()).map(str::to_string))
        .filter(|name| !name.starts_with('.'))
        .collect();
    let manifest_names: std::collections::BTreeSet<String> = manifest
        .artifacts
        .iter()
        .map(|artifact| artifact.release_asset_name.clone())
        .collect();
    for missing in manifest_names.difference(&staged_names) {
        problems.push(format!("manifest references missing staged asset: {missing}"));
    }
    for orphan in staged_names.difference(&manifest_names) {
        problems.push(format!("staged asset not tracked in manifest: {orphan}"));
    }
    for artifact in &manifest.artifacts {
        let dest = payloads.destination_for(artifact)?;
        let has_local_record = payload_metadata.artifacts.iter().any(|item| {
            item.filename == artifact.filename
                && item.platform == artifact.platform
                && item.category == artifact.category
        });
        if dest.exists() && !has_local_record {
            problems.push(format!(
                "synced file exists without local metadata: {}",
                dest.display()
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
                    problems.push(format!("manifest URL returned HTTP {}", resp.status().as_u16()));
                }
                Err(err) => {
                    problems.push(format!("manifest URL unreachable: {err}"));
                }
            }
        }
        BackendKind::OciRegistry => {
            notes.push("LOCKER_BACKEND=oci-registry set; backend not implemented yet".to_string());
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
    let mut metadata = load_local_metadata(&payloads).unwrap_or(LocalMetadata { artifacts: vec![] });
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
                    let state_tags = tui_artifact_state(repo, &payloads, &metadata, a);
                    let active = if a.active { "+" } else { "-" };
                    let staged = if state_tags.staged { "stg" } else { "---" };
                    let sync = if state_tags.stale {
                        "old"
                    } else if state_tags.synced {
                        "syn"
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
                let artifact_state = tui_artifact_state(repo, &payloads, &metadata, artifact);
                Text::from(vec![
                    Line::from(format!("filename: {}", artifact.filename)),
                    Line::from(format!("platform: {}", artifact.platform)),
                    Line::from(format!("category: {}", artifact.category)),
                    Line::from(format!("version: {}", artifact.version)),
                    Line::from(format!("source_type: {}", artifact.source_type)),
                    Line::from(format!("source_ref: {}", artifact.source_ref)),
                    Line::from(format!("sha256: {}", artifact.sha256)),
                    Line::from(format!("release_asset: {}", artifact.release_asset_name)),
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
                        "synced: {}",
                        artifact_state.local_path.unwrap_or_else(|| "-".into())
                    )),
                    Line::from(format!(
                        "sync_state: {}",
                        if artifact_state.synced {
                            "synced"
                        } else if artifact_state.stale {
                            "stale"
                        } else {
                            "not-synced"
                        }
                    )),
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
                let selected_visible = selected_visible_index(state.selected(), visible_indices.len());
                match key.code {
                    KeyCode::Char('q') => break Ok(()),
                    KeyCode::Esc => {
                        filter_query.clear();
                        state.select(Some(0));
                        status = "filter cleared".into();
                    }
                    KeyCode::Char('/') => {
                        search_input = Some(filter_query.clone());
                        status = "search mode: type to filter, Enter to apply, Esc to cancel".into();
                    }
                    KeyCode::Char('R') => {
                        if task.is_some() {
                            status = "task is running; wait for it to finish".into();
                            continue;
                        }
                        manifest = load_manifest(repo)?;
                        metadata = load_local_metadata(&payloads).unwrap_or(LocalMetadata { artifacts: vec![] });
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
                            progress_line = format!("task: queued bulk sync for {count} artifact(s)");
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
                                "copy source ref",
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
                                    progress_line = format!("task: queued sync for {}", artifact.filename);
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
                                    progress_line = format!("task: queued bulk sync for {count} artifact(s)");
                                    status = "bulk sync started".into();
                                }
                            }
                            Some(2) => {
                                task = Some(spawn_verify_task(repo.clone()));
                                progress_line = "task: queued verify".into();
                                status = "verify started".into();
                            }
                            Some(3) => {
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
                            Some(4) => {
                                if let Some(actual_idx) = visible_indices.get(selected_visible) {
                                    let artifact = &manifest.artifacts[*actual_idx];
                                    status = copy_status("filename", &artifact.filename);
                                }
                            }
                            Some(5) => {
                                if let Some(actual_idx) = visible_indices.get(selected_visible) {
                                    let artifact = &manifest.artifacts[*actual_idx];
                                    let artifact_state = tui_artifact_state(repo, &payloads, &metadata, artifact);
                                    let value = artifact_state
                                        .local_path
                                        .or(artifact_state.expected_local_path)
                                        .unwrap_or_else(|| "-".into());
                                    status = copy_status("path", &value);
                                }
                            }
                            Some(6) => {
                                if let Some(actual_idx) = visible_indices.get(selected_visible) {
                                    let artifact = &manifest.artifacts[*actual_idx];
                                    status = copy_status("source_ref", &artifact.source_ref);
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
                            let artifact_state = tui_artifact_state(repo, &payloads, &metadata, artifact);
                            let value = artifact_state
                                .local_path
                                .or(artifact_state.expected_local_path)
                                .unwrap_or_else(|| "-".into());
                            status = copy_status("path", &value);
                        }
                    }
                    KeyCode::Char('u') => {
                        if let Some(actual_idx) = visible_indices.get(selected_visible) {
                            let artifact = &manifest.artifacts[*actual_idx];
                            status = copy_status("source_ref", &artifact.source_ref);
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
    if let Some(existing) = metadata
        .artifacts
        .iter_mut()
        .find(|m| m.filename == record.filename && m.platform == record.platform && m.category == record.category)
    {
        *existing = record;
    } else {
        metadata.artifacts.push(record);
    }
    metadata
        .artifacts
        .sort_by(|a, b| (&a.platform, &a.category, &a.filename).cmp(&(&b.platform, &b.category, &b.filename)));
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

#[derive(Default)]
struct TuiArtifactState {
    staged: bool,
    synced: bool,
    stale: bool,
    expected_local_path: Option<String>,
    local_path: Option<String>,
}

struct TuiTask {
    receiver: Receiver<TaskUpdate>,
}

enum TaskUpdate {
    Progress(String),
    Finished(Result<String, String>),
}

fn tui_artifact_state(
    repo: &RepoPaths,
    payloads: &PayloadPaths,
    metadata: &LocalMetadata,
    artifact: &Artifact,
) -> TuiArtifactState {
    let staged = artifact.staged_path(repo).is_file();
    let dest = payloads.destination_for(artifact).ok();
    let expected_dest = dest.as_ref().map(|p| p.display().to_string());
    let record = metadata.artifacts.iter().find(|m| {
        m.filename == artifact.filename
            && m.platform == artifact.platform
            && m.category == artifact.category
    });
    let local_path = record
        .map(|record| record.local_path.clone())
        .or_else(|| expected_dest.clone().filter(|p| Path::new(p).is_file()));
    let synced = record.is_some_and(|record| {
        Path::new(&record.local_path).is_file()
            && record.sha256 == artifact.sha256
            && record.version == artifact.version
            && expected_dest
                .as_ref()
                .is_none_or(|expected| &record.local_path == expected)
    });
    let stale = !synced
        && (record.is_some() || dest.as_ref().is_some_and(|path| path.is_file()));

    TuiArtifactState {
        staged,
        synced,
        stale,
        expected_local_path: expected_dest,
        local_path,
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

fn spawn_sync_task(payloads: PayloadPaths, artifacts: Vec<Artifact>, success_message: String) -> TuiTask {
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
                BackendKind::OciRegistry => Err(anyhow!("OCI sync backend is not implemented yet.")),
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
