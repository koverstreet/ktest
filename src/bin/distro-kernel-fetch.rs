//! distro-kernel-fetch — Poll configured distro repositories for new kernel
//! packages and convert them to ktest's canonical layout.
//!
//! Designed to run periodically (nightly or hourly via cron / ci-loop).
//! Memoized: if a package version is already present in the output dir, it
//! is skipped without redownloading.
//!
//! Output layout per package:
//!     <output>/<distro>/<release>/<arch>/<version>/
//!         vmlinuz
//!         build -> {lib/modules/<v>/build, headers/...}   # uniform path
//!         lib/modules/<version>/  # mirrors `make modules_install` output
//!         headers/                # contents of /usr/src/ from the package
//!         manifest.json           # distro, release, version, arch, urls, shas
//!
//! The `lib/modules/<v>/` nesting matches the layout `make modules_install`
//! produces, so the existing ktest testrunner symlink-into-9p approach
//! (`ln -sf /host/<kernel_binary>/lib/modules/* /lib/modules`) works
//! without modification.
//!
//! Retention: one version per (distro, release, arch). After a successful
//! fetch, older version directories under the same source are deleted.
//!
//! Supported distros:
//!   debian, ubuntu      — APT (Packages.gz, RFC822)
//!   fedora, centos      — DNF (repomd.xml + primary.xml.{gz,zst}), kernel-core
//!   opensuse            — DNF, kernel-default
//!   arch                — Pacman (core.db tarball)
//!
//! NixOS deliberately not supported here — its kernel-as-derivation model
//! doesn't fit "fetch a binary package". Needs a nix-eval driven companion.

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use clap::Parser;
use flate2::read::GzDecoder;
use quick_xml::events::Event;
use quick_xml::Reader;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

// ============================================================================
// CLI / config
// ============================================================================

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to sources config (JSON5).
    /// Defaults to $HOME/.ktest/distro-sources.json5.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Output directory for kernel packages.
    /// Defaults to $HOME/.ktest/kernels — alongside the ktest VM root
    /// images at $HOME/.ktest/root.<arch>.
    #[arg(long)]
    output: Option<PathBuf>,

    /// List what would be fetched, don't download
    #[arg(long)]
    dry_run: bool,

    /// Re-fetch even if already present
    #[arg(long)]
    force: bool,

    /// Verbose logging
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Deserialize)]
struct Config {
    sources: Vec<Source>,
}

#[derive(Deserialize, Clone)]
struct Source {
    distro: String,
    release: String,
    arch: String,
    /// One or more repo URLs to consult. RHEL-likes split kernel-core
    /// (BaseOS) from kernel-devel (AppStream), so multi-URL is required;
    /// other distros typically need only one. Accepts a bare string or
    /// an array in the config.
    #[serde(deserialize_with = "deserialize_repo_url")]
    repo_url: Vec<String>,
}

fn deserialize_repo_url<'de, D>(d: D) -> Result<Vec<String>, D::Error>
where D: serde::Deserializer<'de>
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrList {
        Single(String),
        Multiple(Vec<String>),
    }
    match StringOrList::deserialize(d)? {
        StringOrList::Single(s) => Ok(vec![s]),
        StringOrList::Multiple(v) if v.is_empty() =>
            Err(serde::de::Error::custom("repo_url must have at least one entry")),
        StringOrList::Multiple(v) => Ok(v),
    }
}

/// One downloadable file, paired with its SHA256 from the repo metadata
/// when available. `sha256` is None when the distro's metadata didn't
/// expose it (rare — all currently-supported formats include checksums).
#[derive(Debug, Clone, Serialize)]
struct PkgFile {
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
}

#[derive(Debug, Serialize)]
struct Manifest {
    /// Schema version of this manifest. Bump on field changes.
    manifest_version: u32,
    distro: String,
    release: String,
    /// uname-r of the kernel — the value `uname -r` prints inside the
    /// running kernel, also the directory name under /lib/modules/.
    version: String,
    arch: String,
    image: PkgFile,
    headers: Vec<PkgFile>,
    fetched_at: String,
}

/// One kernel package available for download from a distro repo.
#[derive(Debug, Clone)]
struct KernelPkg {
    distro: String,
    release: String,
    arch: String,
    /// uname-r of the kernel — used as the directory name and printed by
    /// `uname -r` inside the booted kernel.
    version: String,
    /// The package containing /boot/vmlinuz (and on most distros the
    /// modules tree).
    image: PkgFile,
    /// Additional packages to extract alongside the image: headers,
    /// modules-split, devel. Distro-specific.
    headers: Vec<PkgFile>,
}

trait DistroFetcher {
    fn latest_kernels(&self, src: &Source) -> Result<Vec<KernelPkg>>;
}

// ============================================================================
// HTTP + decompression helpers
// ============================================================================

fn http_get_bytes(url: &str) -> Result<Vec<u8>> {
    let resp = reqwest::blocking::get(url)
        .with_context(|| format!("GET {}", url))?;
    let status = resp.status();
    if !status.is_success() {
        bail!("GET {}: HTTP {}", url, status);
    }
    let bytes = resp.bytes()
        .with_context(|| format!("reading body of {}", url))?;
    Ok(bytes.to_vec())
}

fn gunzip(data: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = GzDecoder::new(data);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).context("gunzip")?;
    Ok(out)
}

fn zstd_decompress(data: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = zstd::Decoder::new(data).context("zstd init")?;
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).context("zstd decode")?;
    Ok(out)
}

/// Decompress based on URL suffix.
fn decompress_for_url(url: &str, data: &[u8]) -> Result<Vec<u8>> {
    if url.ends_with(".gz") {
        gunzip(data)
    } else if url.ends_with(".zst") {
        zstd_decompress(data)
    } else if url.ends_with(".xml") {
        Ok(data.to_vec())
    } else {
        bail!("unknown compression for URL: {}", url)
    }
}

// ============================================================================
// RFC822 stanza parser (Debian Packages format)
// ============================================================================

fn parse_packages(text: &str) -> Vec<HashMap<String, String>> {
    let mut stanzas = Vec::new();
    let mut cur: HashMap<String, String> = HashMap::new();
    let mut last_key: Option<String> = None;

    for line in text.lines() {
        if line.is_empty() {
            if !cur.is_empty() {
                stanzas.push(std::mem::take(&mut cur));
                last_key = None;
            }
        } else if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(k) = &last_key {
                if let Some(v) = cur.get_mut(k) {
                    v.push('\n');
                    v.push_str(line.trim_start());
                }
            }
        } else if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_string();
            let v = v.trim().to_string();
            cur.insert(k.clone(), v);
            last_key = Some(k);
        }
    }
    if !cur.is_empty() {
        stanzas.push(cur);
    }
    stanzas
}

// ============================================================================
// Natural-sort version compare
//
// Not a full dpkg/rpm version compare (no epoch, no ~ handling), but adequate
// for picking the highest among kernel versions in a single release/repo.
// Upgrade if a real edge case bites.
// ============================================================================

fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    loop {
        match (a.first(), b.first()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(&ca), Some(&cb)) => {
                if ca.is_ascii_digit() && cb.is_ascii_digit() {
                    let (na, ra) = take_digits(a);
                    let (nb, rb) = take_digits(b);
                    match na.cmp(&nb) {
                        Ordering::Equal => { a = ra; b = rb; }
                        o => return o,
                    }
                } else {
                    match ca.cmp(&cb) {
                        Ordering::Equal => { a = &a[1..]; b = &b[1..]; }
                        o => return o,
                    }
                }
            }
        }
    }
}

fn take_digits(s: &[u8]) -> (u64, &[u8]) {
    let end = s.iter().take_while(|c| c.is_ascii_digit()).count();
    let n: u64 = std::str::from_utf8(&s[..end]).unwrap_or("0").parse().unwrap_or(0);
    (n, &s[end..])
}

// ============================================================================
// APT family: Debian + Ubuntu
//
// Shared: Packages.gz fetch + RFC822 stanza parse.
// Differ: package naming pattern (`-amd64` vs `-generic`) and headers
//         pairing (Debian has arch + common, Ubuntu has flavor + meta).
// ============================================================================

/// One Debian-style Packages.gz stanza paired with the repo URL it came
/// from — so we can build per-package download URLs even when one Source
/// pulls from multiple repos.
struct AptStanza {
    repo: String,
    fields: HashMap<String, String>,
}

fn apt_fetch_stanzas(src: &Source) -> Result<Vec<AptStanza>> {
    let mut all = Vec::new();
    for repo in &src.repo_url {
        let url = format!(
            "{}/dists/{}/main/binary-{}/Packages.gz",
            repo.trim_end_matches('/'),
            src.release,
            src.arch,
        );
        let body = http_get_bytes(&url)?;
        let text = String::from_utf8(gunzip(&body)?)
            .with_context(|| format!("Packages.gz from {} not UTF-8", url))?;
        for fields in parse_packages(&text) {
            all.push(AptStanza { repo: repo.clone(), fields });
        }
    }
    Ok(all)
}

fn apt_url(repo: &str, filename: &str) -> String {
    format!("{}/{}", repo.trim_end_matches('/'), filename)
}

/// Build a PkgFile from a Debian stanza. None if no Filename: field.
/// Picks SHA256: when available; falls back to None silently when not
/// (a few derivative repos omit it).
fn apt_stanza_to_pkgfile(s: &AptStanza) -> Option<PkgFile> {
    let filename = s.fields.get("Filename")?;
    let url = apt_url(&s.repo, filename);
    let sha256 = s.fields.get("SHA256").cloned();
    Some(PkgFile { url, sha256 })
}

// ----------- Debian -----------

/// Strip `linux-image-` prefix and `-<arch>` suffix; returns the ABI.
/// None for the metapackage (suffix would overlap prefix).
fn debian_abi_from_image<'a>(name: &'a str, arch: &str) -> Option<&'a str> {
    let suffix = format!("-{}", arch);
    name.strip_prefix("linux-image-")
        .and_then(|s| s.strip_suffix(&suffix))
}

fn debian_is_vanilla_image(name: &str, arch: &str) -> bool {
    let Some(abi) = debian_abi_from_image(name, arch) else { return false };
    abi.chars().next().is_some_and(|c| c.is_ascii_digit())
        && !abi.contains("cloud")
        && !abi.ends_with("-rt")
        && !abi.contains("-rt-")
}

struct DebianFetcher;
impl DistroFetcher for DebianFetcher {
    fn latest_kernels(&self, src: &Source) -> Result<Vec<KernelPkg>> {
        let stanzas = apt_fetch_stanzas(src)?;
        let by_name: HashMap<&str, &AptStanza> = stanzas.iter()
            .filter_map(|s| s.fields.get("Package").map(|n| (n.as_str(), s)))
            .collect();

        let mut candidates: Vec<(&str, &str, &str)> = Vec::new();
        for s in &stanzas {
            let Some(name) = s.fields.get("Package") else { continue };
            if !debian_is_vanilla_image(name, &src.arch) { continue }
            let Some(version) = s.fields.get("Version") else { continue };
            let Some(abi) = debian_abi_from_image(name, &src.arch) else { continue };
            candidates.push((name.as_str(), version.as_str(), abi));
        }
        if candidates.is_empty() {
            bail!("no kernel-image packages found for {}/{}", src.distro, src.release);
        }
        candidates.sort_by(|a, b| natural_cmp(a.1, b.1));
        let (img_name, _img_ver, abi) = *candidates.last().unwrap();

        let img_stanza = by_name.get(img_name)
            .ok_or_else(|| anyhow!("{}: stanza lost", img_name))?;
        let image = apt_stanza_to_pkgfile(img_stanza)
            .ok_or_else(|| anyhow!("{}: no Filename", img_name))?;

        // Debian: arch-specific headers + common headers.
        let arch_hdr = format!("linux-headers-{}-{}", abi, src.arch);
        let common_hdr = format!("linux-headers-{}-common", abi);
        let mut headers = Vec::new();
        for hdr in [&arch_hdr, &common_hdr] {
            if let Some(stanza) = by_name.get(hdr.as_str()) {
                if let Some(p) = apt_stanza_to_pkgfile(stanza) {
                    headers.push(p);
                }
            }
        }
        if headers.is_empty() {
            bail!("no headers packages found for ABI {}", abi);
        }

        // Debian uname-r = ABI + arch.
        let version = format!("{}-{}", abi, src.arch);

        Ok(vec![KernelPkg {
            distro: src.distro.clone(),
            release: src.release.clone(),
            arch: src.arch.clone(),
            version,
            image,
            headers,
        }])
    }
}

// ----------- Ubuntu -----------

/// Ubuntu kernel images: `linux-image-<abi>-<flavor>` where flavor is e.g.
/// `generic`, `lowlatency`, `aws`. We track only `generic`. The arch does
/// not appear in the package name — it's in the Architecture: field.
fn ubuntu_abi_from_image(name: &str) -> Option<&str> {
    name.strip_prefix("linux-image-")
        .and_then(|s| s.strip_suffix("-generic"))
}

fn ubuntu_is_vanilla_image(name: &str) -> bool {
    let Some(abi) = ubuntu_abi_from_image(name) else { return false };
    abi.chars().next().is_some_and(|c| c.is_ascii_digit())
        && !abi.contains("-unsigned")
        && !abi.contains("-dbgsym")
}

struct UbuntuFetcher;
impl DistroFetcher for UbuntuFetcher {
    fn latest_kernels(&self, src: &Source) -> Result<Vec<KernelPkg>> {
        let stanzas = apt_fetch_stanzas(src)?;
        let by_name: HashMap<&str, &AptStanza> = stanzas.iter()
            .filter_map(|s| s.fields.get("Package").map(|n| (n.as_str(), s)))
            .collect();

        let mut candidates: Vec<(&str, &str, &str)> = Vec::new();
        for s in &stanzas {
            let Some(name) = s.fields.get("Package") else { continue };
            if !ubuntu_is_vanilla_image(name) { continue }
            // Filter to Architecture matching src.arch (Ubuntu has -generic on all arches).
            if s.fields.get("Architecture").map(String::as_str) != Some(src.arch.as_str()) { continue }
            let Some(version) = s.fields.get("Version") else { continue };
            let Some(abi) = ubuntu_abi_from_image(name) else { continue };
            candidates.push((name.as_str(), version.as_str(), abi));
        }
        if candidates.is_empty() {
            bail!("no kernel-image packages found for ubuntu/{}", src.release);
        }
        candidates.sort_by(|a, b| natural_cmp(a.1, b.1));
        let (img_name, _img_ver, abi) = *candidates.last().unwrap();

        let img_stanza = by_name.get(img_name)
            .ok_or_else(|| anyhow!("{}: stanza lost", img_name))?;
        let image = apt_stanza_to_pkgfile(img_stanza)
            .ok_or_else(|| anyhow!("{}: no Filename", img_name))?;

        // Ubuntu headers: `linux-headers-<abi>-generic` (flavor-specific build
        // dir) + `linux-headers-<abi>` (common). The modules tree is in the
        // image package for older releases; newer ones split it out into
        // `linux-modules-<abi>-generic`.
        let flavor_hdr = format!("linux-headers-{}-generic", abi);
        let common_hdr = format!("linux-headers-{}", abi);
        let modules_pkg = format!("linux-modules-{}-generic", abi);
        let modules_extra = format!("linux-modules-extra-{}-generic", abi);
        let mut headers = Vec::new();
        for hdr in [&flavor_hdr, &common_hdr, &modules_pkg, &modules_extra] {
            if let Some(stanza) = by_name.get(hdr.as_str()) {
                if let Some(p) = apt_stanza_to_pkgfile(stanza) {
                    headers.push(p);
                }
            }
        }
        if headers.is_empty() {
            bail!("no headers packages found for ABI {}", abi);
        }

        let version = format!("{}-generic", abi);
        Ok(vec![KernelPkg {
            distro: src.distro.clone(),
            release: src.release.clone(),
            arch: src.arch.clone(),
            version,
            image,
            headers,
        }])
    }
}

// ============================================================================
// DNF family: Fedora, openSUSE, CentOS Stream
//
// Shared: repomd.xml -> data type="primary" -> primary.xml.{gz,zst} -> parse.
// Differ: package naming. Fedora/CentOS use kernel-core + kernel-modules*
//         + kernel-devel; openSUSE uses kernel-default + kernel-default-devel.
// ============================================================================

#[derive(Debug, Clone)]
struct DnfPkg {
    name: String,
    arch: String,
    version: String,   // <ver>
    release: String,   // <rel>
    location: String,  // href, relative to repo_base
    sha256: Option<String>,
    /// The repo_url this package came from. RHEL-likes have kernel-core
    /// in BaseOS and kernel-devel in AppStream; storing repo_base on each
    /// package lets us build the right download URL regardless.
    repo_base: String,
}

impl DnfPkg {
    /// RPM-style "version-release.arch" — matches uname -r for kernels.
    fn uname_r(&self) -> String {
        format!("{}-{}.{}", self.version, self.release, self.arch)
    }
    fn full_version(&self) -> String {
        format!("{}-{}", self.version, self.release)
    }
    fn to_pkgfile(&self) -> PkgFile {
        PkgFile {
            url: format!("{}/{}", self.repo_base.trim_end_matches('/'), self.location),
            sha256: self.sha256.clone(),
        }
    }
}

fn dnf_fetch_primary_url(repo: &str) -> Result<String> {
    let repomd_url = format!("{}/repodata/repomd.xml", repo.trim_end_matches('/'));
    let body = http_get_bytes(&repomd_url)?;
    let xml = std::str::from_utf8(&body)
        .with_context(|| format!("repomd at {} not UTF-8", repomd_url))?;

    // Find <data type="primary"><location href="..."/></data>.
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut in_primary = false;
    let mut href: Option<String> = None;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.name().as_ref() == b"data" => {
                for attr in e.attributes().flatten() {
                    if attr.key.as_ref() == b"type"
                        && attr.value.as_ref() == b"primary"
                    {
                        in_primary = true;
                    }
                }
            }
            Ok(Event::End(e)) if e.name().as_ref() == b"data" => {
                in_primary = false;
            }
            Ok(Event::Empty(e)) | Ok(Event::Start(e))
                if in_primary && e.name().as_ref() == b"location" =>
            {
                for attr in e.attributes().flatten() {
                    if attr.key.as_ref() == b"href" {
                        href = Some(String::from_utf8_lossy(&attr.value).to_string());
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => bail!("parsing repomd.xml: {}", e),
            _ => {}
        }
        buf.clear();
    }
    let href = href.ok_or_else(|| anyhow!("repomd.xml: no <data type='primary'> location"))?;
    Ok(format!("{}/{}", repo.trim_end_matches('/'), href))
}

fn dnf_fetch_packages(repo: &str) -> Result<Vec<DnfPkg>> {
    let primary_url = dnf_fetch_primary_url(repo)?;
    let body = http_get_bytes(&primary_url)?;
    let xml_bytes = decompress_for_url(&primary_url, &body)?;
    let xml = std::str::from_utf8(&xml_bytes)
        .with_context(|| format!("primary.xml from {} not UTF-8", primary_url))?;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut packages = Vec::new();
    let mut cur: Option<DnfPkg> = None;
    let mut in_package = false;
    let mut text_target: Option<&'static str> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                match e.name().as_ref() {
                    b"package" => {
                        // Confirm type="rpm".
                        let is_rpm = e.attributes().flatten().any(|a|
                            a.key.as_ref() == b"type" && a.value.as_ref() == b"rpm");
                        if is_rpm {
                            in_package = true;
                            cur = Some(DnfPkg {
                                name: String::new(),
                                arch: String::new(),
                                version: String::new(),
                                release: String::new(),
                                location: String::new(),
                                sha256: None,
                                repo_base: repo.to_string(),
                            });
                        }
                    }
                    b"name" if in_package => text_target = Some("name"),
                    b"arch" if in_package => text_target = Some("arch"),
                    b"checksum" if in_package => {
                        // Only capture sha256 — the only one we verify.
                        let is_sha256 = e.attributes().flatten().any(|a|
                            a.key.as_ref() == b"type" && a.value.as_ref() == b"sha256");
                        if is_sha256 { text_target = Some("sha256"); }
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) if in_package => {
                match e.name().as_ref() {
                    b"version" => {
                        if let Some(c) = cur.as_mut() {
                            for attr in e.attributes().flatten() {
                                let v = String::from_utf8_lossy(&attr.value).to_string();
                                match attr.key.as_ref() {
                                    b"ver" => c.version = v,
                                    b"rel" => c.release = v,
                                    _ => {}
                                }
                            }
                        }
                    }
                    b"location" => {
                        if let Some(c) = cur.as_mut() {
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == b"href" {
                                    c.location = String::from_utf8_lossy(&attr.value).to_string();
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                if let Some(target) = text_target {
                    if let Some(c) = cur.as_mut() {
                        let txt = t.unescape().unwrap_or_default().to_string();
                        match target {
                            "name" => c.name = txt,
                            "arch" => c.arch = txt,
                            "sha256" => c.sha256 = Some(txt),
                            _ => {}
                        }
                    }
                }
                text_target = None;
            }
            Ok(Event::End(e)) => {
                match e.name().as_ref() {
                    b"package" if in_package => {
                        if let Some(c) = cur.take() {
                            if !c.name.is_empty() {
                                packages.push(c);
                            }
                        }
                        in_package = false;
                    }
                    b"name" | b"arch" | b"checksum" => text_target = None,
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => bail!("parsing primary.xml: {}", e),
            _ => {}
        }
        buf.clear();
    }
    Ok(packages)
}

/// Pick the latest version of a named package matching `arch` from a DNF
/// package list. None if not found.
fn dnf_pick_latest<'a>(pkgs: &'a [DnfPkg], name: &str, arch: &str) -> Option<&'a DnfPkg> {
    pkgs.iter()
        .filter(|p| p.name == name && p.arch == arch)
        .max_by(|a, b| natural_cmp(&a.full_version(), &b.full_version()))
}

/// Fetch + merge package lists from every repo URL on this Source. Each
/// resulting DnfPkg carries the repo_url it came from so download URLs
/// can be built per-package.
fn dnf_fetch_all_repos(src: &Source) -> Result<Vec<DnfPkg>> {
    let mut all = Vec::new();
    for repo in &src.repo_url {
        let pkgs = dnf_fetch_packages(repo)
            .with_context(|| format!("fetching DNF metadata from {}", repo))?;
        all.extend(pkgs);
    }
    Ok(all)
}

// ----------- Fedora / CentOS Stream (share the kernel-core pattern) -----------

fn fetch_dnf_kernel_core_family(src: &Source) -> Result<Vec<KernelPkg>> {
    let pkgs = dnf_fetch_all_repos(src)?;

    let core = dnf_pick_latest(&pkgs, "kernel-core", &src.arch)
        .ok_or_else(|| anyhow!("no kernel-core in {:?}", src.repo_url))?;
    let core_ver = core.full_version();
    let image = core.to_pkgfile();

    // Collect aux packages at the same version-release.
    let aux_names = ["kernel-modules-core", "kernel-modules",
                     "kernel-modules-extra", "kernel-devel"];
    let mut headers = Vec::new();
    for name in aux_names {
        if let Some(p) = pkgs.iter().find(|p|
            p.name == name && p.arch == src.arch && p.full_version() == core_ver)
        {
            headers.push(p.to_pkgfile());
        }
    }
    if headers.is_empty() {
        bail!("no kernel aux packages found at v {} for {}", core_ver, src.distro);
    }

    Ok(vec![KernelPkg {
        distro: src.distro.clone(),
        release: src.release.clone(),
        arch: src.arch.clone(),
        version: core.uname_r(),
        image,
        headers,
    }])
}

struct FedoraFetcher;
impl DistroFetcher for FedoraFetcher {
    fn latest_kernels(&self, src: &Source) -> Result<Vec<KernelPkg>> {
        fetch_dnf_kernel_core_family(src)
    }
}

struct CentosFetcher;
impl DistroFetcher for CentosFetcher {
    fn latest_kernels(&self, src: &Source) -> Result<Vec<KernelPkg>> {
        fetch_dnf_kernel_core_family(src)
    }
}

// ----------- openSUSE -----------

struct SuseFetcher;
impl DistroFetcher for SuseFetcher {
    fn latest_kernels(&self, src: &Source) -> Result<Vec<KernelPkg>> {
        let pkgs = dnf_fetch_all_repos(src)?;

        let kdefault = dnf_pick_latest(&pkgs, "kernel-default", &src.arch)
            .ok_or_else(|| anyhow!("no kernel-default in {:?}", src.repo_url))?;
        let kver = kdefault.full_version();
        let image = kdefault.to_pkgfile();

        // openSUSE splits the kernel-build inputs three ways:
        //   - kernel-default-devel: arch-specific entry point (`.../obj/<arch>/default/`)
        //     with a stub Makefile that includes `../../../linux-<rel>/Makefile`
        //   - kernel-devel (noarch): the cross-arch build machinery — top-level
        //     Makefile, Kbuild, Kconfig, include/, scripts/ — installs to
        //     `/usr/src/linux-<rel>/`. Without this the kernel-default-devel
        //     Makefile's include cannot resolve.
        //   - kernel-source (noarch): the .c source files. Not strictly needed
        //     for DKMS-style module builds (which only need headers + scripts),
        //     but we fetch it too so a full `make` works if downstream wants it.
        //
        // Version-locking differs by package:
        //   - kernel-default-{devel,base,extra}: track kernel-default exactly
        //   - kernel-devel, kernel-source, kernel-syms: float at slightly
        //     different patchlevels (e.g. .21.2 vs kernel-default's .21.3) but
        //     extract to a dir whose name aligns at the SP-release level, so
        //     loose match (latest matching name+arch) is right
        let tight = ["kernel-default-devel", "kernel-default-base",
                     "kernel-default-extra"];
        let loose = ["kernel-devel", "kernel-source", "kernel-syms"];
        let mut headers = Vec::new();
        for name in tight {
            if let Some(p) = pkgs.iter().find(|p|
                p.name == name
                && (p.arch == src.arch || p.arch == "noarch")
                && p.full_version() == kver)
            {
                headers.push(p.to_pkgfile());
            }
        }
        // Pick the latest matching name+arch (drop the strict version-lock).
        for name in loose {
            if let Some(p) = pkgs.iter()
                .filter(|p| p.name == name
                            && (p.arch == src.arch || p.arch == "noarch"))
                .max_by(|a, b| natural_cmp(&a.full_version(), &b.full_version()))
            {
                headers.push(p.to_pkgfile());
            }
        }
        if headers.is_empty() {
            bail!("no kernel-default aux packages found at v {}", kver);
        }

        Ok(vec![KernelPkg {
            distro: src.distro.clone(),
            release: src.release.clone(),
            arch: src.arch.clone(),
            version: kdefault.uname_r(),
            image,
            headers,
        }])
    }
}

// ============================================================================
// Pacman family: Arch Linux
//
// Repo metadata at `<repo>/core/os/<arch>/core.db` — gzip-compressed tarball
// of `<name>-<version>/desc` text files. Each `desc` is a sequence of
// %FIELD% headers followed by values.
// ============================================================================

#[derive(Debug, Clone)]
struct PacmanPkg {
    name: String,
    version: String,
    filename: String,
    sha256: Option<String>,
}

fn pacman_fetch_packages(db_url: &str) -> Result<Vec<PacmanPkg>> {
    let body = http_get_bytes(db_url)?;
    let tar_bytes = gunzip(&body)?;
    let mut archive = tar::Archive::new(std::io::Cursor::new(tar_bytes));

    let mut packages = Vec::new();
    for entry in archive.entries().context("pacman db: read entries")? {
        let mut entry = entry.context("pacman db: bad entry")?;
        let path = entry.path().context("pacman db: bad entry path")?;
        if !path.ends_with("desc") { continue }

        let mut text = String::new();
        entry.read_to_string(&mut text).context("pacman db: read desc")?;
        packages.push(parse_pacman_desc(&text));
    }
    Ok(packages.into_iter().flatten().collect())
}

fn parse_pacman_desc(text: &str) -> Option<PacmanPkg> {
    let mut fields: HashMap<&str, String> = HashMap::new();
    let mut cur_key: Option<&str> = None;
    let mut cur_val = String::new();
    for line in text.lines() {
        if line.starts_with('%') && line.ends_with('%') {
            if let Some(k) = cur_key.take() {
                fields.insert(k, std::mem::take(&mut cur_val));
            }
            cur_key = Some(match line {
                "%FILENAME%" => "FILENAME",
                "%NAME%" => "NAME",
                "%VERSION%" => "VERSION",
                "%SHA256SUM%" => "SHA256SUM",
                _ => "_",
            });
        } else if cur_key.is_some() && !line.is_empty() {
            if !cur_val.is_empty() { cur_val.push('\n'); }
            cur_val.push_str(line);
        }
    }
    if let Some(k) = cur_key {
        fields.insert(k, cur_val);
    }
    Some(PacmanPkg {
        name: fields.remove("NAME")?,
        version: fields.remove("VERSION")?,
        filename: fields.remove("FILENAME")?,
        sha256: fields.remove("SHA256SUM"),
    })
}

/// Best-effort uname-r for an Arch kernel package version.
/// `linux 6.13.4.arch1-1` → uname-r `6.13.4-arch1-1`. The kernel's modules
/// directory uses uname-r with `.archN` joined by `-` rather than `.`.
fn arch_uname_r(version: &str) -> String {
    if let Some((upstream, suffix)) = version.rsplit_once(".arch") {
        if let Some((arch_n, pkgrel)) = suffix.split_once('-') {
            return format!("{}-arch{}-{}", upstream, arch_n, pkgrel);
        }
    }
    version.replace('.', "-")
}

struct ArchFetcher;
impl DistroFetcher for ArchFetcher {
    fn latest_kernels(&self, src: &Source) -> Result<Vec<KernelPkg>> {
        // Fetch the core db from each configured mirror; merge.
        // Each PacmanPkg carries the base URL it came from so we can
        // build the right download URL.
        let mut all: Vec<(String, PacmanPkg)> = Vec::new();
        for repo in &src.repo_url {
            let base = format!("{}/core/os/{}", repo.trim_end_matches('/'), src.arch);
            let db_url = format!("{}/core.db", base);
            let pkgs = pacman_fetch_packages(&db_url)
                .with_context(|| format!("fetching {}", db_url))?;
            for pkg in pkgs {
                all.push((base.clone(), pkg));
            }
        }

        // The plain `linux` package; also pair with `linux-headers`.
        let (kernel_base, kernel) = all.iter()
            .filter(|(_, p)| p.name == "linux")
            .max_by(|a, b| natural_cmp(&a.1.version, &b.1.version))
            .ok_or_else(|| anyhow!("no `linux` package in any of {:?}", src.repo_url))?;
        let headers_pkg = all.iter().find(|(_, p)|
            p.name == "linux-headers" && p.version == kernel.version);

        let image = PkgFile {
            url: format!("{}/{}", kernel_base, kernel.filename),
            sha256: kernel.sha256.clone(),
        };
        let mut headers = Vec::new();
        if let Some((base, h)) = headers_pkg {
            headers.push(PkgFile {
                url: format!("{}/{}", base, h.filename),
                sha256: h.sha256.clone(),
            });
        }
        if headers.is_empty() {
            bail!("no linux-headers matching version {}", kernel.version);
        }

        Ok(vec![KernelPkg {
            distro: src.distro.clone(),
            release: src.release.clone(),
            arch: src.arch.clone(),
            version: arch_uname_r(&kernel.version),
            image,
            headers,
        }])
    }
}

// ============================================================================
// Dispatch
// ============================================================================

fn fetcher_for(distro: &str) -> Result<Box<dyn DistroFetcher>> {
    match distro {
        "debian"    => Ok(Box::new(DebianFetcher)),
        "ubuntu"    => Ok(Box::new(UbuntuFetcher)),
        "fedora"    => Ok(Box::new(FedoraFetcher)),
        "centos" | "rhel" | "rocky" | "alma"
                    => Ok(Box::new(CentosFetcher)),
        "opensuse" | "suse"
                    => Ok(Box::new(SuseFetcher)),
        "arch"      => Ok(Box::new(ArchFetcher)),
        "nixos"     => Err(anyhow!(
            "nixos not supported by distro-kernel-fetch: kernels are nixpkgs \
             derivations, not binary packages. Use a nix-eval based path."
        )),
        d => Err(anyhow!("unsupported distro: {}", d)),
    }
}

// ============================================================================
// Memoization + fetch driver
// ============================================================================

/// The directory holding one source's version subdirectories.
/// `<output>/<distro>/<release>/<arch>/`
fn source_dir(output: &Path, pkg: &KernelPkg) -> PathBuf {
    output.join(&pkg.distro).join(&pkg.release).join(&pkg.arch)
}

fn already_have(output: &Path, pkg: &KernelPkg) -> bool {
    source_dir(output, pkg).join(&pkg.version).join("manifest.json").exists()
}

/// Delete every version subdirectory under the source dir except `keep`.
/// Called after a successful publish to enforce "one version per source".
fn prune_older_versions(src_dir: &Path, keep: &str) -> Result<()> {
    let entries = match fs::read_dir(src_dir) {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).context("scanning source dir for prune"),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if name == keep { continue }
        let path = entry.path();
        // Skip files (e.g. `_work` doesn't live here, but be defensive).
        if !path.is_dir() { continue }
        fs::remove_dir_all(&path)
            .with_context(|| format!("pruning {}", path.display()))?;
    }
    Ok(())
}

// ============================================================================
// Download + extract + canonical-layout discovery
//
// External tool deps: dpkg-deb (for .deb), rpm2cpio + cpio (for .rpm),
// tar with --zstd / -xJf / -xzf (for Arch .pkg.tar.zst and miscellaneous).
//
// Discovery is heuristic and distro-agnostic: vmlinuz by name+size,
// modules by `modules.order` marker under a "modules" parent, headers by
// the presence of a `usr/src` tree (Debian/Fedora/SUSE) or `build/`
// inside the modules dir (Arch).
// ============================================================================

fn download_to(url: &str, dst: &Path) -> Result<()> {
    let mut resp = reqwest::blocking::get(url)
        .with_context(|| format!("GET {}", url))?;
    if !resp.status().is_success() {
        bail!("GET {}: HTTP {}", url, resp.status());
    }
    let mut out = fs::File::create(dst)
        .with_context(|| format!("creating {}", dst.display()))?;
    std::io::copy(&mut resp, &mut out)
        .with_context(|| format!("downloading {}", url))?;
    Ok(())
}

fn run_extractor(mut cmd: Command, what: &str) -> Result<()> {
    let status = cmd.status()
        .with_context(|| format!("spawning extractor for {}", what))?;
    if !status.success() {
        bail!("extractor failed on {}: exit {}", what, status);
    }
    Ok(())
}

fn extract_archive(archive: &Path, dst: &Path) -> Result<()> {
    let fname = archive.file_name()
        .and_then(|f| f.to_str())
        .ok_or_else(|| anyhow!("bad archive path: {}", archive.display()))?;
    let a = archive.to_str().unwrap();
    let d = dst.to_str().unwrap();

    if fname.ends_with(".deb") {
        let mut c = Command::new("dpkg-deb");
        c.args(["-x", a, d]);
        run_extractor(c, fname)
    } else if fname.ends_with(".rpm") {
        // rpm2cpio <rpm> | (cd dst && cpio -idm --quiet)
        // Done via a pipe + working-directory rather than sh.
        let mut r2c = Command::new("rpm2cpio")
            .arg(a)
            .stdout(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawning rpm2cpio on {}", fname))?;
        let stdout = r2c.stdout.take()
            .ok_or_else(|| anyhow!("rpm2cpio: no stdout"))?;
        let cpio_status = Command::new("cpio")
            .args(["-idm", "--quiet"])
            .current_dir(dst)
            .stdin(Stdio::from(stdout))
            .status()
            .with_context(|| format!("spawning cpio on {}", fname))?;
        let r2c_status = r2c.wait().context("waiting on rpm2cpio")?;
        if !r2c_status.success() {
            bail!("rpm2cpio failed on {}: exit {}", fname, r2c_status);
        }
        if !cpio_status.success() {
            bail!("cpio failed on {}: exit {}", fname, cpio_status);
        }
        Ok(())
    } else if fname.ends_with(".tar.zst") || fname.ends_with(".pkg.tar.zst") {
        let mut c = Command::new("tar");
        c.args(["--zstd", "-xf", a, "-C", d]);
        run_extractor(c, fname)
    } else if fname.ends_with(".tar.xz") || fname.ends_with(".pkg.tar.xz") {
        let mut c = Command::new("tar");
        c.args(["-xJf", a, "-C", d]);
        run_extractor(c, fname)
    } else if fname.ends_with(".tar.gz") || fname.ends_with(".pkg.tar.gz") {
        let mut c = Command::new("tar");
        c.args(["-xzf", a, "-C", d]);
        run_extractor(c, fname)
    } else {
        bail!("unsupported archive extension: {}", fname)
    }
}

/// Find the largest "vmlinuz*" or "bzImage*" file under `root`. The
/// real kernel image is multi-MB; symlinks or stubs are smaller.
fn find_vmlinuz(root: &Path) -> Result<PathBuf> {
    let mut best: Option<(PathBuf, u64)> = None;
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry.context("walking for vmlinuz")?;
        if !entry.file_type().is_file() { continue }
        let name = entry.file_name().to_string_lossy();
        if !(name.starts_with("vmlinuz") || name.starts_with("bzImage")) { continue }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if size < 1024 * 1024 { continue }
        if best.as_ref().map(|(_, s)| size > *s).unwrap_or(true) {
            best = Some((entry.path().to_path_buf(), size));
        }
    }
    best.map(|(p, _)| p)
        .ok_or_else(|| anyhow!("no vmlinuz found under {}", root.display()))
}

/// Find a `<something>/modules/<uname-r>` directory containing modules.order
/// (or a kernel/ subdirectory). Covers /lib/modules/, /usr/lib/modules/.
fn find_modules_dir(root: &Path) -> Result<PathBuf> {
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry.context("walking for modules dir")?;
        if !entry.file_type().is_dir() { continue }
        let path = entry.path();
        let parent_name = path.parent()
            .and_then(|p| p.file_name())
            .and_then(|f| f.to_str());
        if parent_name != Some("modules") { continue }
        if path.join("modules.order").exists() || path.join("kernel").is_dir() {
            return Ok(path.to_path_buf());
        }
    }
    bail!("no modules dir found under {}", root.display())
}

/// Move src into dst. If they're on different filesystems, fall back to
/// recursive copy + remove.
fn rename_or_copy(src: &Path, dst: &Path) -> Result<()> {
    if fs::rename(src, dst).is_ok() { return Ok(()); }
    copy_dir_recursive(src, dst)?;
    fs::remove_dir_all(src).ok();
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)
        .with_context(|| format!("mkdir {}", dst.display()))?;
    for entry in fs::read_dir(src)
        .with_context(|| format!("read_dir {}", src.display()))?
    {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            let target = fs::read_link(&src_path)?;
            std::os::unix::fs::symlink(&target, &dst_path)
                .with_context(|| format!("symlink {} -> {}",
                                         dst_path.display(), target.display()))?;
        } else if ft.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)
                .with_context(|| format!("copy {} -> {}",
                                         src_path.display(), dst_path.display()))?;
        }
    }
    Ok(())
}

/// Collect headers/build directories into `stage/headers/`.
///
/// Strategies, tried in order:
///   1. extracted/usr/src/ exists (Debian, Fedora, openSUSE) — move the
///      whole `usr/src/` tree under headers/.
///   2. modules-tree already at `stage/lib/modules/<v>/build/` (Arch) —
///      symlink stage/headers -> that build dir.
fn collect_headers(extracted: &Path, stage: &Path, uname_r: &str) -> Result<()> {
    let usrsrc = extracted.join("usr").join("src");
    let dst_headers = stage.join("headers");
    if usrsrc.is_dir() {
        rename_or_copy(&usrsrc, &dst_headers)
            .with_context(|| format!("collecting headers from {}", usrsrc.display()))?;
        return Ok(());
    }
    let modules_build = stage.join("lib").join("modules").join(uname_r).join("build");
    if modules_build.is_dir() {
        let target = PathBuf::from("lib").join("modules").join(uname_r).join("build");
        std::os::unix::fs::symlink(&target, &dst_headers)
            .with_context(|| format!("symlinking headers -> {}", target.display()))?;
        return Ok(());
    }
    bail!("no headers found in {}", extracted.display())
}

fn write_manifest(stage: &Path, pkg: &KernelPkg) -> Result<()> {
    let manifest = Manifest {
        manifest_version: 1,
        distro: pkg.distro.clone(),
        release: pkg.release.clone(),
        version: pkg.version.clone(),
        arch: pkg.arch.clone(),
        image: pkg.image.clone(),
        headers: pkg.headers.clone(),
        fetched_at: Utc::now().to_rfc3339(),
    };
    let text = serde_json::to_string_pretty(&manifest)
        .context("serializing manifest")?;
    fs::write(stage.join("manifest.json"), text)
        .with_context(|| format!("writing {}/manifest.json", stage.display()))?;
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut f = fs::File::open(path)
        .with_context(|| format!("opening {} for hashing", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut f, &mut hasher)
        .with_context(|| format!("reading {} for hashing", path.display()))?;
    Ok(hex::encode(hasher.finalize()))
}

fn verify_sha256(path: &Path, expected: &str) -> Result<()> {
    let actual = sha256_file(path)?;
    if !actual.eq_ignore_ascii_case(expected) {
        bail!("SHA256 mismatch for {}: expected {}, got {}",
              path.display(), expected, actual);
    }
    Ok(())
}

/// Add `<stage>/build` as a relative symlink pointing at the kernel build
/// directory inside `stage`. Downstream consumers rely on this uniform
/// path instead of having to know each distro's `headers/` layout.
///
/// The authoritative source is the distro's own `lib/modules/<v>/build`
/// symlink: Debian sets it to `/usr/src/linux-headers-<v>-<arch>`, Fedora
/// to `/usr/src/kernels/<v>`. Since we extracted `/usr/src/` into
/// `<stage>/headers/`, we just rebase the target. For Arch, that path is
/// itself the real build directory.
fn create_build_symlink(stage: &Path, uname_r: &str) -> Result<()> {
    let modules_build = stage.join("lib").join("modules").join(uname_r).join("build");
    let meta = fs::symlink_metadata(&modules_build)
        .with_context(|| format!("no modules/build at {}", modules_build.display()))?;

    let rel = if meta.file_type().is_symlink() {
        let target = fs::read_link(&modules_build)
            .context("reading modules/build symlink")?;
        // Target may be absolute (`/usr/src/linux-headers-<v>-<arch>`) or
        // relative (`../../../src/linux-headers-...`, Debian trixie's
        // Usr-Merge style). Either way it lands under `/usr/src/` in the
        // package's view, so find the `src` path component and take
        // everything after it.
        let components: Vec<_> = target.components().collect();
        let src_pos = components.iter().position(|c| match c {
            std::path::Component::Normal(n) => n.to_str() == Some("src"),
            _ => false,
        });
        let src_pos = src_pos.ok_or_else(|| anyhow!(
            "modules/build target {} has no `src` component — can't rebase \
             into headers/", target.display()))?;
        let rest: PathBuf = components[src_pos + 1..].iter().collect();
        PathBuf::from("headers").join(rest)
    } else {
        // Arch / Arch-like: lib/modules/<v>/build is the real build directory.
        PathBuf::from("lib").join("modules").join(uname_r).join("build")
    };

    if !stage.join(&rel).exists() {
        bail!("build target {} doesn't exist in stage", rel.display());
    }

    std::os::unix::fs::symlink(&rel, stage.join("build"))
        .with_context(|| format!("creating build symlink -> {}", rel.display()))?;
    Ok(())
}

// ============================================================================
// Initramfs generation
//
// Distro kernels compile virtio/9p/ext4 as modules, not built-in. ktest's
// existing built-from-source flow has these as =y in the kernel config; for
// distro kernels we need an initramfs to insmod them before mounting root.
//
// Approach: walk modules.dep for the transitive closure of seed modules,
// decompress any .ko.xz/.ko.zst into staged plain .ko's, stage klibc-utils
// static binaries (sh, mount, insmod, run-init) + interp, write a tiny
// /init shell script, then cpio | gzip the staging tree into <stage>/initramfs.
//
// Host requirements: klibc-utils + libklibc packages installed (Debian) —
// these provide the static-ish minimal toolset for early-boot use.
// ============================================================================

/// Seed modules — the closure we resolve via modules.dep includes these
/// plus everything they transitively depend on.
const INITRAMFS_SEED_MODULES: &[&str] = &[
    // Disk: virtio_pci enumerates PCI bus, virtio_blk drives /dev/vda
    "virtio_pci",
    "virtio_blk",
    // hvc0 console — without this, distro kernels boot silently because
    // they have no built-in console driver registered before pivot_root.
    // With ktest's built-from-source kernel this is =y; for distro kernels
    // it's a module that we need preloaded for kernel output to be visible.
    "virtio_console",
    // virtio_net — eth0 needs to exist before networking.service runs.
    // After pivot_root, /lib/modules/<v>/ is empty (testrunner.service
    // symlinks the host tree, but that runs After=multi-user.target),
    // so udev's modprobe-on-coldplug can't auto-load it. Preload here.
    "virtio_net",
    // Rootfs filesystem (debootstrap ext4 image, per ktest convention).
    // crc32c is requested by ext4 via request_module() at mount time for
    // metadata-checksum support, which mkfs.ext4 has enabled by default
    // for many years — but that runtime request is NOT a build-time
    // dependency in modules.dep, so we list crc32c_generic explicitly.
    "crc32c_generic",
    "ext4",
    // configfs — the rootfs's fstab includes /sys/kernel/config, and
    // local-fs.target depends on it. Without configfs, that mount fails,
    // local-fs.target fails, systemd drops to emergency mode and we
    // never reach multi-user.target.
    "configfs",
    // 9p mount of host /  — needed by /etc/fstab in the guest rootfs which
    // mounts the host root at /host (libktest.sh's -virtfs mount_tag=host)
    "9pnet_virtio",
    "9p",
];

#[derive(Debug, Clone)]
struct ModuleInfo {
    /// Path relative to lib/modules/<uname-r>/ (e.g.
    /// "kernel/fs/ext4/ext4.ko.xz" — preserves the on-disk extension).
    rel_path: PathBuf,
    /// Dependency module names (already stripped of path + extension).
    deps: Vec<String>,
}

/// Strip ".ko" + any compression suffix, take basename, convert `-` to `_`
/// (the kernel module-name convention).
fn module_name_from_path(path: &str) -> String {
    let stem = Path::new(path).file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path);
    let stem = stem.strip_suffix(".ko.gz")
        .or_else(|| stem.strip_suffix(".ko.xz"))
        .or_else(|| stem.strip_suffix(".ko.zst"))
        .or_else(|| stem.strip_suffix(".ko"))
        .unwrap_or(stem);
    stem.replace('-', "_")
}

fn parse_modules_dep(text: &str) -> HashMap<String, ModuleInfo> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let Some((mod_path, deps_str)) = line.split_once(':') else { continue };
        let mod_path = mod_path.trim();
        if mod_path.is_empty() { continue }
        let name = module_name_from_path(mod_path);
        let deps: Vec<String> = deps_str.split_whitespace()
            .map(module_name_from_path)
            .collect();
        out.insert(name, ModuleInfo {
            rel_path: PathBuf::from(mod_path),
            deps,
        });
    }
    out
}

/// Post-order DFS: produces a list where every module's dependencies appear
/// before it. Modules absent from modules.dep are silently skipped — they're
/// either built into the kernel (no insmod needed; symbols already resolved
/// against vmlinux) or genuinely missing (which insmod would have failed on
/// anyway). Seed skips get an eprintln so the user can spot a typo.
fn module_load_order(
    modules: &HashMap<String, ModuleInfo>,
    seeds: &[&str],
) -> Vec<String> {
    use std::collections::HashSet;
    let mut visited: HashSet<String> = HashSet::new();
    let mut ordered: Vec<String> = Vec::new();

    fn visit(
        name: &str,
        modules: &HashMap<String, ModuleInfo>,
        visited: &mut HashSet<String>,
        ordered: &mut Vec<String>,
    ) {
        if !visited.insert(name.to_string()) { return }
        let Some(info) = modules.get(name) else { return };
        for dep in &info.deps {
            visit(dep, modules, visited, ordered);
        }
        ordered.push(name.to_string());
    }

    for seed in seeds {
        if !modules.contains_key(*seed) {
            eprintln!("  note: {} built-in to this kernel, skipping", seed);
            continue;
        }
        visit(seed, modules, &mut visited, &mut ordered);
    }
    ordered
}

/// Decompress src into dst as a plain .ko. Dispatches on src extension.
/// klibc's insmod doesn't decompress on the fly, so all bundled modules
/// must be stored uncompressed.
fn decompress_module(src: &Path, dst: &Path) -> Result<()> {
    let src_name = src.to_string_lossy();
    if src_name.ends_with(".ko") {
        fs::copy(src, dst).map(|_| ()).map_err(Into::into)
    } else if src_name.ends_with(".ko.gz") {
        let data = fs::read(src)?;
        let plain = gunzip(&data)?;
        fs::write(dst, &plain).map_err(Into::into)
    } else if src_name.ends_with(".ko.xz") {
        let status = Command::new("xz")
            .args(["-dc", src.to_str().unwrap()])
            .stdout(fs::File::create(dst)?)
            .status()
            .context("spawning xz -dc")?;
        if !status.success() {
            bail!("xz -dc failed on {}", src.display());
        }
        Ok(())
    } else if src_name.ends_with(".ko.zst") {
        let data = fs::read(src)?;
        let plain = zstd_decompress(&data)?;
        fs::write(dst, &plain).map_err(Into::into)
    } else {
        bail!("unrecognized module extension: {}", src.display())
    }
}

/// Find /usr/lib/klibc-<hash>.so — the runtime interpreter for klibc binaries.
fn klibc_interp_path() -> Result<PathBuf> {
    for entry in fs::read_dir("/usr/lib")
        .context("scanning /usr/lib for klibc interp")?
    {
        let entry = entry?;
        let name = entry.file_name();
        let s = name.to_string_lossy();
        if s.starts_with("klibc-") && s.ends_with(".so") {
            return Ok(entry.path());
        }
    }
    bail!("no /usr/lib/klibc-*.so found — apt install klibc-utils libklibc")
}

/// Run `depmod` against the staged modules tree to produce modules.dep
/// + friends. Distro .deb / .rpm packages ship `modules.order` and
/// `modules.builtin` but not the depmod-generated files — those are
/// generated by the package's post-install scripts on the target system.
/// We're not running post-installs, so we need to do this ourselves.
fn run_depmod(stage: &Path, uname_r: &str) -> Result<()> {
    // depmod isn't in $PATH for non-root users; try /sbin and /usr/sbin.
    let depmod_bin = ["/sbin/depmod", "/usr/sbin/depmod"].iter()
        .find(|p| Path::new(p).is_file())
        .ok_or_else(|| anyhow!("depmod not found in /sbin or /usr/sbin"))?;
    let status = Command::new(depmod_bin)
        .args(["-a", "-b"])
        .arg(stage)
        .arg(uname_r)
        .status()
        .with_context(|| format!("spawning {}", depmod_bin))?;
    if !status.success() {
        bail!("depmod failed (exit {})", status);
    }
    Ok(())
}

/// Produce <stage>/initramfs (cpio.gz) so qemu can `-initrd` it.
fn generate_initramfs(stage: &Path, uname_r: &str) -> Result<()> {
    let modules_dir = stage.join("lib").join("modules").join(uname_r);
    run_depmod(stage, uname_r)?;
    let dep_text = fs::read_to_string(modules_dir.join("modules.dep"))
        .with_context(|| format!("reading {}/modules.dep", modules_dir.display()))?;
    let modules = parse_modules_dep(&dep_text);
    let ordered = module_load_order(&modules, INITRAMFS_SEED_MODULES);

    // Stage the initramfs filesystem tree.
    let irfs = stage.join("_irfs");
    if irfs.exists() { fs::remove_dir_all(&irfs)?; }
    for sub in ["bin", "usr/lib", "modules", "dev", "proc", "sys", "newroot"] {
        fs::create_dir_all(irfs.join(sub))?;
    }

    // klibc tools + their shared interp.
    let klibc_bin = Path::new("/usr/lib/klibc/bin");
    for tool in ["sh", "mount", "insmod", "mkdir", "run-init"] {
        let src = klibc_bin.join(tool);
        let dst = irfs.join("bin").join(tool);
        fs::copy(&src, &dst)
            .with_context(|| format!("copying klibc {} (install klibc-utils?)", tool))?;
        let mut perms = fs::metadata(&dst)?.permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        fs::set_permissions(&dst, perms)?;
    }
    let interp = klibc_interp_path()?;
    let interp_dst = irfs.join("usr/lib").join(interp.file_name().unwrap());
    fs::copy(&interp, &interp_dst)
        .with_context(|| format!("copying klibc interp {}", interp.display()))?;

    // Decompress + stage each module under /modules/ flat (no kernel/ prefix).
    for mod_name in &ordered {
        let info = &modules[mod_name];
        let src = modules_dir.join(&info.rel_path);
        let dst = irfs.join("modules").join(format!("{}.ko", mod_name));
        decompress_module(&src, &dst)
            .with_context(|| format!("decompressing {}", src.display()))?;
    }

    // Write /init.
    let mut init = String::from("#!/bin/sh\n\n");
    init.push_str("# Generated by distro-kernel-fetch — minimal initramfs init\n");
    init.push_str("set -e\n\n");
    init.push_str("mount -t proc proc /proc\n");
    init.push_str("mount -t sysfs sysfs /sys\n");
    init.push_str("mount -t devtmpfs devtmpfs /dev\n\n");
    init.push_str("# Load modules in dependency order\n");
    for mod_name in &ordered {
        init.push_str(&format!("insmod /modules/{}.ko\n", mod_name));
    }
    init.push_str("\n# Mount rootfs (ktest convention: ext4 on /dev/vda)\n");
    init.push_str("mount -t ext4 /dev/vda /newroot\n\n");
    init.push_str("# Pivot to the rootfs's own init\n");
    init.push_str("exec run-init /newroot /sbin/init\n");
    let init_path = irfs.join("init");
    fs::write(&init_path, init)?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&init_path, fs::Permissions::from_mode(0o755))?;

    // cpio (newc format) | gzip > <stage>/initramfs.
    // cpio -o reads filenames from stdin; use find piped in.
    let initramfs_path = stage.join("initramfs");
    let initramfs_file = fs::File::create(&initramfs_path)
        .with_context(|| format!("creating {}", initramfs_path.display()))?;
    let find = Command::new("find")
        .arg(".")
        .current_dir(&irfs)
        .stdout(Stdio::piped())
        .spawn()
        .context("spawning find for cpio")?;
    let find_stdout = find.stdout.unwrap();
    let cpio = Command::new("cpio")
        .args(["-o", "-H", "newc", "--quiet"])
        .current_dir(&irfs)
        .stdin(Stdio::from(find_stdout))
        .stdout(Stdio::piped())
        .spawn()
        .context("spawning cpio")?;
    let cpio_stdout = cpio.stdout.unwrap();
    let gzip_status = Command::new("gzip")
        .arg("-n")  // suppress timestamp for reproducibility
        .stdin(Stdio::from(cpio_stdout))
        .stdout(initramfs_file)
        .status()
        .context("spawning gzip")?;
    if !gzip_status.success() {
        bail!("gzip failed during initramfs build");
    }

    fs::remove_dir_all(&irfs)?;
    Ok(())
}

fn fetch_and_convert(output: &Path, pkg: &KernelPkg) -> Result<()> {
    let src_dir = source_dir(output, pkg);
    let dst = src_dir.join(&pkg.version);

    // Work area sits under output/ so rename(work, dst) is intra-filesystem.
    let work = output.join("_work").join(format!("{}-{}-{}-{}",
        pkg.distro, pkg.release, pkg.arch, pkg.version));
    if work.exists() {
        fs::remove_dir_all(&work)
            .with_context(|| format!("cleaning {}", work.display()))?;
    }
    let dl = work.join("dl");
    let extracted = work.join("extracted");
    let stage = work.join("stage");
    fs::create_dir_all(&dl)?;
    fs::create_dir_all(&extracted)?;
    fs::create_dir_all(&stage)?;

    // 1. Download all packages; verify SHA256 if metadata exposed it.
    let files: Vec<&PkgFile> = std::iter::once(&pkg.image)
        .chain(pkg.headers.iter())
        .collect();
    let mut downloaded = Vec::with_capacity(files.len());
    for f in &files {
        let fname = f.url.rsplit('/').next().unwrap_or("download");
        let path = dl.join(fname);
        download_to(&f.url, &path)?;
        if let Some(expected) = &f.sha256 {
            verify_sha256(&path, expected)
                .with_context(|| format!("verifying {}", f.url))?;
        }
        downloaded.push(path);
    }

    // 2. Extract all packages into one tree.
    for archive in &downloaded {
        extract_archive(archive, &extracted)
            .with_context(|| format!("extracting {}", archive.display()))?;
    }

    // 3. Build canonical layout in stage/.
    let vmlinuz = find_vmlinuz(&extracted)?;
    rename_or_copy(&vmlinuz, &stage.join("vmlinuz"))
        .context("placing vmlinuz")?;

    let modules = find_modules_dir(&extracted)?;
    let modules_dst = stage.join("lib").join("modules").join(&pkg.version);
    fs::create_dir_all(modules_dst.parent().unwrap())?;
    rename_or_copy(&modules, &modules_dst)
        .context("placing modules")?;

    collect_headers(&extracted, &stage, &pkg.version)?;
    create_build_symlink(&stage, &pkg.version)?;
    generate_initramfs(&stage, &pkg.version)
        .with_context(|| format!("generating initramfs for {}", pkg.version))?;

    write_manifest(&stage, pkg)?;

    // 4. Atomic publish.
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    if dst.exists() {
        fs::remove_dir_all(&dst)
            .with_context(|| format!("removing stale {}", dst.display()))?;
    }
    fs::rename(&stage, &dst)
        .with_context(|| format!("publishing to {}", dst.display()))?;

    // 5. Drop older versions of this source.
    prune_older_versions(&src_dir, &pkg.version)
        .with_context(|| format!("pruning older versions in {}", src_dir.display()))?;

    // 6. Cleanup.
    let _ = fs::remove_dir_all(&work);
    Ok(())
}

fn process_source(args: &Args, output: &Path, src: &Source) -> Result<()> {
    let fetcher = fetcher_for(&src.distro)?;
    let pkgs = fetcher.latest_kernels(src)
        .with_context(|| format!("listing kernels for {}/{}", src.distro, src.release))?;

    if args.verbose {
        eprintln!("{}/{}/{}: {} candidate(s)",
                  src.distro, src.release, src.arch, pkgs.len());
    }

    for pkg in pkgs {
        if !args.force && already_have(output, &pkg) {
            if args.verbose {
                eprintln!("  have {}: skip", pkg.version);
            }
            continue;
        }
        if args.dry_run {
            eprintln!("  would fetch {} -> {}", pkg.version, pkg.image.url);
            for h in &pkg.headers {
                eprintln!("            + {}", h.url);
            }
            continue;
        }
        eprintln!("  fetching {}", pkg.version);
        fetch_and_convert(output, &pkg)
            .with_context(|| format!("fetching {} for {}/{}",
                                     pkg.version, pkg.distro, pkg.release))?;
    }
    Ok(())
}

/// Resolve a user-or-system data path: prefer $HOME/.ktest, fall back to
/// /var/lib/ktest. Matches `lib/util.sh`'s convention for root_image
/// discovery so kernels live alongside the existing VM images.
fn ktest_data_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".ktest"))
        .unwrap_or_else(|| PathBuf::from("/var/lib/ktest"))
}

fn main() -> Result<()> {
    let args = Args::parse();
    let data_dir = ktest_data_dir();
    let config_path = args.config.clone()
        .unwrap_or_else(|| data_dir.join("distro-sources.json5"));
    let output = args.output.clone()
        .unwrap_or_else(|| data_dir.join("kernels"));

    let config_text = std::fs::read_to_string(&config_path)
        .with_context(|| format!("reading {}", config_path.display()))?;
    let config: Config = json5::from_str(&config_text)
        .with_context(|| format!("parsing {}", config_path.display()))?;

    let mut failed = 0;
    for src in &config.sources {
        if let Err(e) = process_source(&args, &output, src) {
            eprintln!("ERROR: {}/{}/{}: {:#}", src.distro, src.release, src.arch, e);
            failed += 1;
        }
    }

    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debian_vanilla_filter() {
        assert!(debian_is_vanilla_image("linux-image-6.1.0-39-amd64", "amd64"));
        assert!(!debian_is_vanilla_image("linux-image-amd64", "amd64"));
        assert!(!debian_is_vanilla_image("linux-image-6.1.0-39-amd64-dbg", "amd64"));
        assert!(!debian_is_vanilla_image("linux-image-6.1.0-39-amd64-unsigned", "amd64"));
        assert!(!debian_is_vanilla_image("linux-image-6.1.0-39-cloud-amd64", "amd64"));
        assert!(!debian_is_vanilla_image("linux-image-6.1.0-39-rt-amd64", "amd64"));
        assert!(!debian_is_vanilla_image("linux-headers-6.1.0-39-amd64", "amd64"));
    }

    #[test]
    fn debian_abi() {
        assert_eq!(debian_abi_from_image("linux-image-6.1.0-39-amd64", "amd64"),
                   Some("6.1.0-39"));
        assert_eq!(debian_abi_from_image("linux-image-amd64", "amd64"), None);
        assert_eq!(debian_abi_from_image("vim", "amd64"), None);
    }

    #[test]
    fn ubuntu_vanilla_filter() {
        assert!(ubuntu_is_vanilla_image("linux-image-6.8.0-31-generic"));
        assert!(!ubuntu_is_vanilla_image("linux-image-generic"));
        assert!(!ubuntu_is_vanilla_image("linux-image-unsigned-6.8.0-31-generic"));
        assert!(!ubuntu_is_vanilla_image("linux-headers-6.8.0-31-generic"));
    }

    #[test]
    fn arch_uname_r_conversion() {
        assert_eq!(arch_uname_r("6.13.4.arch1-1"), "6.13.4-arch1-1");
        assert_eq!(arch_uname_r("6.13.4.arch2-3"), "6.13.4-arch2-3");
        // Fallback path for unusual versions.
        assert_eq!(arch_uname_r("1.2.3-4"), "1-2-3-4");
    }

    #[test]
    fn natural_sort_orders_kernel_versions() {
        assert_eq!(natural_cmp("6.1.0-9", "6.1.0-26"), Ordering::Less);
        assert_eq!(natural_cmp("6.1.148-1", "6.1.155-1"), Ordering::Less);
        assert_eq!(natural_cmp("6.1.0-39", "6.1.0-39"), Ordering::Equal);
        assert_eq!(natural_cmp("6.1.0-42", "6.1.0-39"), Ordering::Greater);
        assert_eq!(natural_cmp("6.1.0-rc1", "6.1.0-rc2"), Ordering::Less);
    }

    #[test]
    fn rfc822_parser_basics() {
        let text = "Package: foo\nVersion: 1.0\nDescription: line one\n line two\n\nPackage: bar\nVersion: 2.0\n";
        let stanzas = parse_packages(text);
        assert_eq!(stanzas.len(), 2);
        assert_eq!(stanzas[0].get("Package").map(String::as_str), Some("foo"));
        assert_eq!(stanzas[0].get("Description").map(String::as_str),
                   Some("line one\nline two"));
        assert_eq!(stanzas[1].get("Package").map(String::as_str), Some("bar"));
    }

    #[test]
    fn modules_dep_parsing() {
        let text = "\
kernel/drivers/virtio/virtio_pci.ko: kernel/drivers/virtio/virtio.ko kernel/drivers/virtio/virtio_ring.ko
kernel/drivers/block/virtio_blk.ko: kernel/drivers/virtio/virtio.ko kernel/drivers/virtio/virtio_ring.ko
kernel/fs/ext4/ext4.ko.xz: kernel/fs/jbd2/jbd2.ko.xz kernel/fs/mbcache.ko.xz
kernel/fs/mbcache.ko.xz:
";
        let m = parse_modules_dep(text);
        assert_eq!(m.len(), 4);
        assert_eq!(m["virtio_pci"].deps, vec!["virtio", "virtio_ring"]);
        assert_eq!(m["ext4"].deps, vec!["jbd2", "mbcache"]);
        assert_eq!(m["mbcache"].deps.len(), 0);
    }

    #[test]
    fn module_load_order_is_post_order() {
        let mut modules = HashMap::new();
        modules.insert("ext4".to_string(), ModuleInfo {
            rel_path: PathBuf::from("ext4.ko"),
            deps: vec!["jbd2".to_string(), "mbcache".to_string()],
        });
        modules.insert("jbd2".to_string(), ModuleInfo {
            rel_path: PathBuf::from("jbd2.ko"),
            deps: vec![],
        });
        modules.insert("mbcache".to_string(), ModuleInfo {
            rel_path: PathBuf::from("mbcache.ko"),
            deps: vec![],
        });
        let order = module_load_order(&modules, &["ext4"]);
        // jbd2 + mbcache must precede ext4.
        let pos = |n: &str| order.iter().position(|x| x == n).unwrap();
        assert!(pos("jbd2") < pos("ext4"));
        assert!(pos("mbcache") < pos("ext4"));

        // Missing seed (e.g. built-in module) is skipped silently.
        let order2 = module_load_order(&modules, &["ext4", "nonexistent"]);
        assert!(order2.iter().any(|x| x == "ext4"));
        assert!(!order2.iter().any(|x| x == "nonexistent"));
    }

    #[test]
    fn module_name_normalization() {
        assert_eq!(module_name_from_path("kernel/fs/ext4/ext4.ko"), "ext4");
        assert_eq!(module_name_from_path("kernel/fs/ext4/ext4.ko.xz"), "ext4");
        assert_eq!(module_name_from_path("kernel/fs/ext4/ext4.ko.zst"), "ext4");
        // Dash → underscore (kernel module-name convention).
        assert_eq!(module_name_from_path("kernel/net/9p/9p-fs.ko"), "9p_fs");
    }

    #[test]
    fn pacman_desc_parse() {
        let text = "%FILENAME%\nlinux-6.13.4.arch1-1-x86_64.pkg.tar.zst\n\n%NAME%\nlinux\n\n%VERSION%\n6.13.4.arch1-1\n\n%BASE%\nlinux\n";
        let p = parse_pacman_desc(text).unwrap();
        assert_eq!(p.name, "linux");
        assert_eq!(p.version, "6.13.4.arch1-1");
        assert_eq!(p.filename, "linux-6.13.4.arch1-1-x86_64.pkg.tar.zst");
    }
}
