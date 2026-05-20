//! distro-kernel-fetch — Poll configured distro repositories for new kernel
//! packages and convert them to ktest's canonical layout.
//!
//! Designed to run periodically (nightly or hourly via cron).
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
//!   nixos               — Hydra JSON + cache.nixos.org NAR (xz-decoded, nix-nar)

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
/// expose it (rare — all currently-supported formats include checksums;
/// nixos is the standing exception, see NixosFetcher).
///
/// `role` is an opaque distro-specific layout hint. The standard pipeline
/// ignores it (extracts every file into one merged tree); distros whose
/// kernels span multiple archives with non-overlapping placement (nixos)
/// use it to pick where each one lands. None for everyone else.
#[derive(Debug, Clone, Serialize)]
struct PkgFile {
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
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
    Some(PkgFile { url, sha256, role: None })
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

/// Debian's headers tree assumes its `/usr/{src,lib}/` install layout — both
/// in the Makefile's `include /usr/src/linux-headers-<v>-common/Makefile`
/// directive and in the `scripts -> ../../lib/linux-kbuild-<abi>/scripts`
/// symlinks. Outside that layout the build fails immediately. This runs
/// after the standard canonicalize and rewires the tree to be self-contained:
///
///   1. Move the linux-kbuild-<abi> package's content (`scripts/`, `tools/`)
///      from `<extracted>/usr/lib/linux-kbuild-<abi>/` into
///      `<stage>/lib/linux-kbuild-<abi>/`, where the headers' relative
///      `../../lib/linux-kbuild-<abi>/...` symlinks land.
///   2. Add a `<stage>/src -> headers` symlink so the
///      `lib/modules/<v>/build -> ../../../src/linux-headers-<v>-<arch>`
///      symlink (Debian's usr-merge expects /lib resolves through /usr/lib
///      to /usr/src; we collapse that to a one-hop alias) resolves.
///   3. Rewrite the linux-headers-<v>-<arch>/Makefile to use self-relative
///      paths (`$(THIS_DIR)`) instead of absolute `/usr/src/<...>` paths,
///      so the tree builds regardless of where it gets mounted.
fn debian_fixup(extracted: &Path, stage: &Path, version: &str) -> Result<()> {
    // The version is `<abi>-<arch>`. The kbuild package is named
    // `linux-kbuild-<abi>` (no arch suffix). Walk usr/lib/ to find it
    // rather than re-derive the ABI — cheaper and more robust.
    let usrlib = extracted.join("usr").join("lib");
    let mut kbuild_dir: Option<PathBuf> = None;
    if usrlib.is_dir() {
        for entry in fs::read_dir(&usrlib).context("read_dir usr/lib")? {
            let entry = entry?;
            let name = entry.file_name();
            if let Some(s) = name.to_str() {
                if s.starts_with("linux-kbuild-") && entry.file_type()?.is_dir() {
                    kbuild_dir = Some(entry.path());
                    break;
                }
            }
        }
    }
    if let Some(src) = kbuild_dir {
        let stage_lib = stage.join("lib");
        fs::create_dir_all(&stage_lib)
            .with_context(|| format!("mkdir {}", stage_lib.display()))?;
        let dst = stage_lib.join(src.file_name().unwrap());
        rename_or_copy(&src, &dst)
            .with_context(|| format!("placing linux-kbuild {} -> {}",
                                     src.display(), dst.display()))?;
    } else {
        // Not fatal — older Debian releases (where headers shipped their
        // own scripts) didn't have this split. Pre-forky bookworm/trixie
        // we'd land here too. Continue without it; if the tree actually
        // needs the kbuild content the Makefile rewrite below or a later
        // build failure will surface it.
    }

    // Add stage/src symlink → stage/headers so lib/modules/<v>/build,
    // which points at ../../../src/linux-headers-<v>-<arch>, resolves to
    // the placed headers tree. On a real Debian system that symlink
    // resolves through usr-merge (/lib -> /usr/lib, then ../../../src/X
    // = /usr/src/X); our layout's flat, so the one-hop src→headers alias
    // is the minimum to make those package symlinks resolve.
    let src_link = stage.join("src");
    if !src_link.exists() {
        std::os::unix::fs::symlink("headers", &src_link)
            .with_context(|| format!("creating {} -> headers", src_link.display()))?;
    }

    // Rewrite the headers Makefile. Debian's version is two lines:
    //   KBUILD_OUTPUT=/usr/src/linux-headers-<v>-<arch>
    //   include /usr/src/linux-headers-<v>-common/Makefile
    // Substitute `/usr/src/` → `$(THIS_DIR)/../` (since the Makefile lives
    // inside one of those /usr/src/linux-headers-<...> dirs, .. takes you
    // up to the equivalent of /usr/src, where the sibling dirs sit).
    let hdrs_amd64 = stage.join("headers")
        .join(format!("linux-headers-{}", version));
    let makefile = hdrs_amd64.join("Makefile");
    if makefile.is_file() {
        let text = fs::read_to_string(&makefile)
            .with_context(|| format!("reading {}", makefile.display()))?;
        if text.contains("/usr/src/") {
            let rewritten = format!(
                "# Rewritten by distro-kernel-fetch — was /usr/src-anchored.\n\
                 THIS_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))\n\
                 {}",
                text.replace("/usr/src/", "$(THIS_DIR)/../"));
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&makefile)?.permissions();
            perms.set_mode(perms.mode() | 0o200);
            fs::set_permissions(&makefile, perms)
                .with_context(|| format!("chmod +w {}", makefile.display()))?;
            fs::write(&makefile, rewritten)
                .with_context(|| format!("rewriting {}", makefile.display()))?;
        }
    }
    Ok(())
}

/// openSUSE's kernel build dir (under `linux-<v>-obj/<arch>/<flavor>/`) is
/// pointed at by our `build` symlink. Its Makefile is two lines:
///
///   export KBUILD_OUTPUT = /usr/src/linux-<v>-obj/<arch>/<flavor>
///   include ../../../linux-<v>/Makefile
///
/// The include is already self-relative and works for us; the KBUILD_OUTPUT
/// path is absolute /usr/src/... and the source Makefile checks
/// `realpath $(KBUILD_OUTPUT)` exists, which fails outside a SUSE install.
/// Rewrite KBUILD_OUTPUT to `$(THIS_DIR)` — same pattern as the nixos and
/// debian fixups.
fn opensuse_fixup(stage: &Path) -> Result<()> {
    // Follow the build symlink to find the obj-dir Makefile.
    let build = stage.join("build");
    let obj_dir = match fs::read_link(&build) {
        Ok(target) => stage.join(target),
        // No symlink? Already-real-dir layout (unexpected for opensuse) —
        // nothing for us to rewrite.
        Err(_) => return Ok(()),
    };
    let makefile = obj_dir.join("Makefile");
    if !makefile.is_file() { return Ok(()) }
    let text = fs::read_to_string(&makefile)
        .with_context(|| format!("reading {}", makefile.display()))?;
    if !text.contains("/usr/src/") { return Ok(()) }

    // The KBUILD_OUTPUT path always names this very directory, so map any
    // `/usr/src/linux-<...>-obj/<arch>/<flavor>` reference to $(THIS_DIR).
    // Substituting the full prefix is more brittle than just replacing the
    // common /usr/src/ root — for openSUSE the rest of the path is exactly
    // the relative location of this Makefile, so /usr/src/ → $(THIS_DIR)/../
    // doesn't quite work (would give $(THIS_DIR)/../linux-X-obj/<arch>/<f>
    // which is wrong by 3 levels). Instead, replace the specific
    // KBUILD_OUTPUT line with one that uses $(THIS_DIR) directly.
    let mut out = String::with_capacity(text.len() + 200);
    out.push_str(
        "# Rewritten by distro-kernel-fetch — was /usr/src-anchored.\n\
         THIS_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))\n",
    );
    for line in text.lines() {
        if line.starts_with("export KBUILD_OUTPUT") || line.starts_with("KBUILD_OUTPUT") {
            out.push_str("export KBUILD_OUTPUT := $(THIS_DIR)\n");
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&makefile)?.permissions();
    perms.set_mode(perms.mode() | 0o200);
    fs::set_permissions(&makefile, perms)
        .with_context(|| format!("chmod +w {}", makefile.display()))?;
    fs::write(&makefile, out)
        .with_context(|| format!("rewriting {}", makefile.display()))?;
    Ok(())
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

        // Debian splits the kernel into up to FIVE packages we need to pull:
        //   - linux-headers-<abi>-<arch>   arch-specific headers
        //   - linux-headers-<abi>-common   shared header tree (full source tree
        //                                  minus arch-specific bits)
        //   - linux-kbuild-<abi>           kbuild infra: scripts/* + tools/*.
        //                                  The headers packages' top-level
        //                                  `scripts -> ../../lib/linux-kbuild-<abi>/scripts`
        //                                  symlink expects this package at
        //                                  /usr/lib/linux-kbuild-<abi>/ in the
        //                                  extracted tree.
        //   - linux-binary-<abi>-<arch>    vmlinuz (forky/sid only — split
        //                                  out of linux-image into a separate
        //                                  package; pre-forky linux-image
        //                                  carries the binary itself, and
        //                                  this package doesn't exist)
        //   - linux-modules-<abi>-<arch>   /lib/modules/<v>/ (same split as
        //                                  linux-binary above)
        //
        // Silently skip any of the binary/modules pair that doesn't exist —
        // pre-forky releases ship them inside linux-image.
        let arch_hdr   = format!("linux-headers-{}-{}", abi, src.arch);
        let common_hdr = format!("linux-headers-{}-common", abi);
        let kbuild_pkg = format!("linux-kbuild-{}", abi);
        let binary_pkg = format!("linux-binary-{}-{}", abi, src.arch);
        let modules_pkg = format!("linux-modules-{}-{}", abi, src.arch);
        let mut headers = Vec::new();
        for hdr in [&arch_hdr, &common_hdr, &kbuild_pkg, &binary_pkg, &modules_pkg] {
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

        // Ubuntu headers split into a flavor-specific arch package + a
        // common (all-arch) header tree. The flavor pkg is always
        // `linux-headers-<abi>-generic`; the common pkg's name varies:
        //   - main archive kernel:  `linux-headers-<abi>`
        //   - HWE kernel:           `linux-hwe-X.Y-headers-<abi>`
        //                            (X.Y = major.minor of the abi)
        //   - cloud variants:       `linux-aws-X.Y-headers-<abi>` etc.
        // The arch package's stanza always Depends: on the common one as
        // its first entry, but rather than parse Depends we scan by_name
        // for any package whose name suffix is `-headers-<abi>` and isn't
        // the flavor-specific one. Picks up the common variant regardless
        // of HWE/cloud prefix.
        let flavor_hdr = format!("linux-headers-{}-generic", abi);
        let modules_pkg = format!("linux-modules-{}-generic", abi);
        let modules_extra = format!("linux-modules-extra-{}-generic", abi);
        // Find the common-headers package by matching the arch-headers
        // stanza's `Source:` field — the flavor + common pkgs ship from
        // the same source. Filtering by Source weeds out other arches'
        // `<flavor>-headers-<abi>` packages that happen to land in this
        // Packages list as Architecture: all (linux-riscv-X.Y on amd64,
        // etc.).
        let hdr_suffix = format!("-headers-{}", abi);
        let flavor_stanza = by_name.get(flavor_hdr.as_str())
            .ok_or_else(|| anyhow!("{}: flavor headers stanza missing", flavor_hdr))?;
        let flavor_source = flavor_stanza.fields.get("Source")
            .map(String::as_str);
        let common_hdrs: Vec<&str> = by_name.iter()
            .filter(|(name, stanza)| {
                **name != flavor_hdr
                && name.ends_with(&hdr_suffix)
                && stanza.fields.get("Source").map(String::as_str) == flavor_source
            })
            .map(|(name, _)| *name)
            .collect();
        let mut headers = Vec::new();
        if let Some(stanza) = by_name.get(flavor_hdr.as_str()) {
            if let Some(p) = apt_stanza_to_pkgfile(stanza) {
                headers.push(p);
            }
        }
        for name in &common_hdrs {
            if let Some(stanza) = by_name.get(name) {
                if let Some(p) = apt_stanza_to_pkgfile(stanza) {
                    headers.push(p);
                }
            }
        }
        for name in [&modules_pkg, &modules_extra] {
            if let Some(stanza) = by_name.get(name.as_str()) {
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
            role: None,
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
            role: None,
        };
        let mut headers = Vec::new();
        if let Some((base, h)) = headers_pkg {
            headers.push(PkgFile {
                url: format!("{}/{}", base, h.filename),
                sha256: h.sha256.clone(),
                role: None,
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
// NixOS: Hydra job lookup + cache.nixos.org NAR fetch
//
// NixOS doesn't publish per-version binary packages from a repo index. Instead,
// nixpkgs builds are tracked by Hydra (the nixpkgs CI), and the resulting store
// paths are served as NAR archives by cache.nixos.org. To map "give me the
// newest kernel on channel X" to a downloadable URL set:
//
//   1. Ask Hydra: GET /job/nixos/<jobset>/<attr>.<arch>/latest-finished →
//      JSON with `buildoutputs.{out,modules,dev}.path` (store paths).
//   2. For each store path, GET /<hash-part>.narinfo from cache.nixos.org →
//      tells us the .nar.xz URL, compression, and integrity hash.
//   3. Fetch+xz-decode each NAR, unpack with nix-nar.
//
// Three outputs per kernel:
//   out      — bzImage + System.map + .config
//   modules  — lib/modules/<v>/{kernel, modules.{dep,order,builtin,…}}
//              plus dangling symlinks `build` and `source` pointing into
//              /nix/store; we delete those and replace from the dev tree.
//   dev      — the kernel build+source trees (Makefile, include/, scripts/,
//              Module.symvers, plus pre-built objtool/fixdep).
//
// The dev tree comes from nix's binary cache, which means every script
// inside it has a `#!/nix/store/<hash>-bash-X.Y/bin/sh` shebang and every
// pre-built binary is linked against `/nix/store/<hash>-glibc/lib/ld-…`.
// None of those paths exist on a non-nix host, so a bare `make modules` blows
// up the moment it invokes pahole-version.sh or objtool. We post-process:
//   - rewrite `#!/nix/store/<hash>-bash-X.Y/bin/sh` → `#!/bin/sh` in scripts
//   - patchelf --set-interpreter on ELF binaries with /nix/store interps
// Requires the `patchelf` host tool. With both fixups in place, OOT module
// builds against these kernels work the same as on debian/fedora/etc.
//
// `release` mapping:
//   "unstable"      → jobset `unstable`,            attr `linuxPackages_latest.kernel`
//   "X.Y"           → jobset `release-X.Y`,         attr `linuxPackages.kernel`
//                     (stable-on-stable; below DKMS floor for most releases)
//   "X.Y-latest"    → jobset `release-X.Y`,         attr `linuxPackages_latest.kernel`
// ============================================================================

const NIXOS_HYDRA_BASE: &str = "https://hydra.nixos.org";
const NIXOS_CACHE_BASE: &str = "https://cache.nixos.org";

/// Resolve a `release` config value to (hydra jobset, hydra job attr prefix).
/// The attr is the part before `.<arch>` — caller appends e.g. `.x86_64-linux`.
fn nixos_jobset_attr(release: &str) -> (String, &'static str) {
    if release == "unstable" {
        ("unstable".to_string(), "nixpkgs.linuxPackages_latest.kernel")
    } else if let Some(rel) = release.strip_suffix("-latest") {
        (format!("release-{}", rel), "nixpkgs.linuxPackages_latest.kernel")
    } else {
        (format!("release-{}", release), "nixpkgs.linuxPackages.kernel")
    }
}

/// Hydra system identifier: ktest config uses "x86_64" / "aarch64"; Hydra
/// uses "x86_64-linux" / "aarch64-linux". Map.
fn nixos_hydra_arch(arch: &str) -> Result<&'static str> {
    match arch {
        "x86_64"  => Ok("x86_64-linux"),
        "aarch64" => Ok("aarch64-linux"),
        a => bail!("nixos: unsupported arch `{}` (expected x86_64 or aarch64)", a),
    }
}

#[derive(Debug)]
struct HydraOutputs {
    nixname: String,    // e.g. "linux-7.0.5"
    out_path: String,   // /nix/store/<hash>-linux-X.Y.Z
    modules_path: String,
    dev_path: String,
}

/// `reqwest::blocking::get` follows redirects by default; Hydra historically
/// redirects /job/nixos/trunk-combined/* → /job/nixos/unstable/*. We rely on
/// the default redirect policy and add an explicit Accept header.
fn nixos_fetch_hydra(jobset: &str, attr: &str, hydra_arch: &str) -> Result<HydraOutputs> {
    let url = format!("{}/job/nixos/{}/{}.{}/latest-finished",
                      NIXOS_HYDRA_BASE, jobset, attr, hydra_arch);
    let client = reqwest::blocking::Client::builder()
        .build()
        .context("building reqwest client")?;
    let resp = client.get(&url)
        .header("Accept", "application/json")
        .send()
        .with_context(|| format!("GET {}", url))?;
    if !resp.status().is_success() {
        bail!("Hydra GET {}: HTTP {}", url, resp.status());
    }
    let body = resp.bytes()
        .with_context(|| format!("reading Hydra body from {}", url))?;
    let v: serde_json::Value = serde_json::from_slice(&body)
        .with_context(|| format!("parsing Hydra JSON from {}", url))?;
    let nixname = v["nixname"].as_str()
        .ok_or_else(|| anyhow!("{}: missing nixname", url))?.to_string();
    let outputs = &v["buildoutputs"];
    let read = |key: &str| -> Result<String> {
        outputs[key]["path"].as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("{}: missing buildoutputs.{}.path", url, key))
    };
    Ok(HydraOutputs {
        nixname,
        out_path:     read("out")?,
        modules_path: read("modules")?,
        dev_path:     read("dev")?,
    })
}

/// One entry from a cache.nixos.org narinfo.
#[derive(Debug)]
struct NarInfo {
    /// Full URL to the NAR (compressed). Decompression is inferred from the
    /// suffix the same way `decompress_for_url` does it.
    url: String,
}

/// Fetch and parse a narinfo for a /nix/store path. The hash-part is the
/// 32-char base32 segment before the first `-`.
///
/// We deliberately don't return the FileHash for SHA256 verification: it's
/// encoded in Nix's custom base32 alphabet (not the standard one), and we
/// trust HTTPS-to-cache.nixos.org for integrity. The existing PkgFile.sha256
/// is therefore None for nixos.
fn nixos_fetch_narinfo(store_path: &str) -> Result<NarInfo> {
    let hash = store_path.strip_prefix("/nix/store/")
        .and_then(|s| s.split('-').next())
        .ok_or_else(|| anyhow!("malformed store path: {}", store_path))?;
    let url = format!("{}/{}.narinfo", NIXOS_CACHE_BASE, hash);
    let bytes = http_get_bytes(&url)?;
    let text = std::str::from_utf8(&bytes)
        .with_context(|| format!("narinfo {}: not utf8", url))?;
    let mut nar_url: Option<String> = None;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("URL: ") {
            nar_url = Some(v.trim().to_string());
        }
    }
    let nar_url = nar_url.ok_or_else(|| anyhow!("narinfo {}: no URL line", url))?;
    Ok(NarInfo {
        url: format!("{}/{}", NIXOS_CACHE_BASE, nar_url),
    })
}

/// Unpack each NAR into a role-named subdir of `extracted/`, then assemble
/// the canonical layout in `stage/`. NixOS-specific because the three NARs
/// (out / modules / dev) place their content at different roots, with
/// dangling symlinks from the modules tree to the dev store path that we
/// need to rewrite.
fn nixos_extract_and_layout(
    downloaded: &[(&PkgFile, PathBuf)],
    extracted: &Path,
    stage: &Path,
    version: &str,
) -> Result<()> {
    use std::collections::HashMap;
    // 1. Index by role and extract.
    let mut role_dir: HashMap<String, PathBuf> = HashMap::new();
    for (file, path) in downloaded {
        let role = file.role.as_deref()
            .ok_or_else(|| anyhow!("nixos: PkgFile {} missing role", file.url))?;
        let dst = extracted.join(role);
        // nix-nar's Decoder::unpack refuses to write into an existing
        // directory — it wants to mkdir it. Don't pre-create.
        extract_nar_xz(path, &dst)
            .with_context(|| format!("extracting nixos {} NAR", role))?;
        role_dir.insert(role.to_string(), dst);
    }
    let get = |r: &str| -> Result<&Path> {
        role_dir.get(r)
            .map(PathBuf::as_path)
            .ok_or_else(|| anyhow!("nixos: missing `{}` role in downloads", r))
    };
    let (out, modules, dev) = (get("out")?, get("modules")?, get("dev")?);

    // 2. Place vmlinuz. nixpkgs's x86_64 kernel ships `bzImage` (not
    //    `vmlinuz`); other arches differ (Image for arm64). find_vmlinuz
    //    handles both name forms.
    let vmlinuz = find_vmlinuz(out)
        .with_context(|| format!("locating vmlinuz in nixos `out` ({})", out.display()))?;
    rename_or_copy(&vmlinuz, &stage.join("vmlinuz"))
        .context("placing nixos vmlinuz")?;

    // 3. Place modules tree. The dangling symlinks `build` and `source`
    //    inside lib/modules/<v>/ point into /nix/store; remove them before
    //    moving so we can drop the real headers tree at build/ in step 4.
    let mods_src = modules.join("lib").join("modules").join(version);
    if !mods_src.is_dir() {
        bail!("nixos modules NAR missing lib/modules/{} at {}",
              version, modules.display());
    }
    for stale in ["build", "source"] {
        let p = mods_src.join(stale);
        if fs::symlink_metadata(&p).is_ok() {
            // Could be a dangling symlink or (defensively) a real entry —
            // remove_file handles symlinks regardless of target validity.
            fs::remove_file(&p)
                .or_else(|_| fs::remove_dir_all(&p))
                .with_context(|| format!("removing nixos modules/{}", stale))?;
        }
    }
    let mods_dst = stage.join("lib").join("modules").join(version);
    fs::create_dir_all(mods_dst.parent().unwrap())?;
    rename_or_copy(&mods_src, &mods_dst)
        .context("placing nixos modules")?;

    // 4. Place the build (and source) trees from the dev NAR. nixpkgs's
    //    dev output lays its content out at
    //      <root>/lib/modules/<v>/{build,source}/...
    //    i.e. at the same path the modules tree's dangling symlinks pointed
    //    to. We pull both trees up into stage/lib/modules/<v>/, replacing
    //    the just-deleted dangling symlinks.
    //
    //    (The dev NAR also contains /vmlinux + /nix-support at NAR root —
    //    those we drop. We're not building modules out of vmlinux, and
    //    nix-support is just a propagated-build-inputs marker.)
    let dev_inner = dev.join("lib").join("modules").join(version);
    if !dev_inner.is_dir() {
        bail!("nixos dev NAR missing lib/modules/{} at {}",
              version, dev.display());
    }
    for kind in ["build", "source"] {
        let src = dev_inner.join(kind);
        if !src.exists() { continue }
        let dst = mods_dst.join(kind);
        rename_or_copy(&src, &dst)
            .with_context(|| format!("placing nixos kernel.dev `{}` tree", kind))?;
    }
    // Sanity: the build tree must have at least a Makefile + Module.symvers
    // — out-of-tree module builds depend on both.
    let bd = mods_dst.join("build");
    for required in ["Makefile", "Module.symvers"] {
        if !bd.join(required).exists() {
            bail!("nixos kernel.dev didn't yield {}/{} — layout changed?",
                  bd.display(), required);
        }
    }

    // The build/ Makefile is a stub that nix generates with absolute
    // /nix/store/<hash>-linux-<v>-dev paths to KBUILD_OUTPUT and the real
    // top-level source/Makefile. Those paths don't exist on our system
    // (and even if they did, baking a host-side absolute path into a
    // relocatable tree is fragile). Rewrite to be self-relative — the
    // stub now resolves both KBUILD_OUTPUT and the source-Makefile-include
    // off the stub's own directory.
    let stub = bd.join("Makefile");
    let stub_text = fs::read_to_string(&stub)
        .with_context(|| format!("reading nixos build stub {}", stub.display()))?;
    if stub_text.contains("/nix/store/") {
        let rewritten = "\
# Rewritten by distro-kernel-fetch (was a /nix/store-anchored stub).
# Resolves KBUILD_OUTPUT + the source-Makefile-include off this stub's own
# directory, so the tree works wherever it's been placed.
THIS_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))
export KBUILD_OUTPUT := $(THIS_DIR)
include $(THIS_DIR)/../source/Makefile
";
        // Files unpacked from a NAR retain their /nix/store mode (0444 for
        // regular files). Bump +w before writing.
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&stub)
            .with_context(|| format!("stat {}", stub.display()))?.permissions();
        perms.set_mode(perms.mode() | 0o200);
        fs::set_permissions(&stub, perms)
            .with_context(|| format!("chmod +w {}", stub.display()))?;
        fs::write(&stub, rewritten)
            .with_context(|| format!("rewriting nixos build stub {}", stub.display()))?;
    }

    // 5. De-nixify the build+source trees: rewrite /nix/store shebangs and
    //    patchelf any ELF interpreters. Without this, `make modules` fails
    //    on the first script (e.g. scripts/pahole-version.sh) or pre-built
    //    binary (e.g. tools/objtool/objtool).
    nixos_denixify_tree(&mods_dst)
        .context("de-nixifying kernel.dev tree")?;

    // 6. Standard uniform build symlink — relative path into our own tree.
    create_build_symlink(stage, version)?;
    Ok(())
}

/// Walk a tree and rewrite anything that depends on /nix/store paths:
///   - File shebangs `#!/nix/store/<hash>-bash-X/bin/sh` → `#!/bin/sh`
///   - ELF binaries with /nix/store interpreters → patchelf to /lib64/ld-…
///
/// We only touch files we'd otherwise leave intact: regular files (not
/// symlinks), with a /nix/store-anchored first line (scripts) or PT_INTERP
/// (binaries). Files unchanged in either dimension are skipped, including
/// the kernel's actual .ko modules.
fn nixos_denixify_tree(root: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    // Verify patchelf is on PATH up front — otherwise we'd half-fix the
    // tree and fail in the middle.
    let pe_check = Command::new("patchelf").arg("--version").output();
    if pe_check.is_err() || !pe_check.unwrap().status.success() {
        bail!("nixos kernel.dev fixup requires `patchelf` on PATH \
               (install via `apt install patchelf` / `dnf install patchelf`)");
    }

    // Canonical paths of the host's glibc dynamic linker, keyed by
    // current arch. /lib64 is x86_64 SysV ABI; ld-musl is musl. We pick
    // whatever ld.so the host has — that's what dkms-built userspace
    // helpers (fixdep, modpost, ...) would use anyway.
    let ldso_candidates: &[&str] = match std::env::consts::ARCH {
        "x86_64"  => &["/lib64/ld-linux-x86-64.so.2", "/lib/ld-linux-x86-64.so.2"],
        "aarch64" => &["/lib/ld-linux-aarch64.so.1"],
        "riscv64" => &["/lib/ld-linux-riscv64-lp64d.so.1"],
        other     => bail!("no host glibc ld.so candidates known for arch {}", other),
    };
    let host_ldso = ldso_candidates.iter().map(Path::new).find(|p| p.exists())
        .ok_or_else(|| anyhow!("no glibc dynamic linker on this host (tried {:?})",
                               ldso_candidates))?;

    let mut shebangs_patched = 0usize;
    let mut elfs_patched = 0usize;

    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry.context("walking tree for de-nixify")?;
        if !entry.file_type().is_file() { continue }
        let path = entry.path();

        // Peek the first few bytes. ELF magic = "\x7fELF"; shebang = "#!".
        let mut hdr = [0u8; 4];
        let n = match fs::File::open(path).and_then(|mut f| std::io::Read::read(&mut f, &mut hdr)) {
            Ok(n) => n,
            Err(_) => continue,  // unreadable file — leave it alone
        };
        if n < 2 { continue }

        let ensure_writable = |p: &Path| -> Result<()> {
            let mut perms = fs::metadata(p)?.permissions();
            if perms.mode() & 0o200 == 0 {
                perms.set_mode(perms.mode() | 0o200);
                fs::set_permissions(p, perms)?;
            }
            Ok(())
        };

        if &hdr[..2] == b"#!" {
            // Shebang: read first line, check for /nix/store prefix, rewrite.
            let text = match fs::read_to_string(path) {
                Ok(t) => t,
                Err(_) => continue,  // binary with #! prefix coincidence? leave.
            };
            let (first, rest) = match text.split_once('\n') {
                Some(pair) => pair,
                None       => (text.as_str(), ""),
            };
            if !first.starts_with("#!/nix/store/") { continue }
            // The path after #! is `/nix/store/<hash>-<pkg>/bin/<binary>` plus
            // optional args. Pick the binary name and route through /usr/bin/env
            // — bash, perl, python3 all live in different places across distros;
            // env handles the lookup and side-steps "is bash at /bin/bash or
            // /usr/bin/bash?".
            let cmd_line = first.trim_start_matches("#!").trim();
            let (interp_path, args) = match cmd_line.split_once(char::is_whitespace) {
                Some((p, a)) => (p, a.trim()),
                None         => (cmd_line, ""),
            };
            let binary = Path::new(interp_path).file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("sh");
            let new_first = if args.is_empty() {
                format!("#!/usr/bin/env {}", binary)
            } else {
                // /usr/bin/env supports `-S` for arg splitting since coreutils 8.30;
                // we conservatively pass through for shells that ignore extras.
                format!("#!/usr/bin/env -S {} {}", binary, args)
            };
            ensure_writable(path)
                .with_context(|| format!("chmod +w {}", path.display()))?;
            let new_text = format!("{}\n{}", new_first, rest);
            fs::write(path, new_text)
                .with_context(|| format!("rewriting shebang in {}", path.display()))?;
            shebangs_patched += 1;
            continue;
        }

        if &hdr[..4] == b"\x7fELF" {
            // Ask patchelf what the current interpreter is. If it's not a
            // /nix/store path, no work to do (some .o files have no INTERP).
            let out = Command::new("patchelf")
                .args(["--print-interpreter"]).arg(path)
                .output()
                .with_context(|| format!("patchelf --print-interpreter {}",
                                         path.display()))?;
            if !out.status.success() { continue }
            let interp = String::from_utf8_lossy(&out.stdout);
            let interp = interp.trim();
            if !interp.starts_with("/nix/store/") { continue }

            ensure_writable(path)
                .with_context(|| format!("chmod +w {}", path.display()))?;
            let status = Command::new("patchelf")
                .args(["--set-interpreter"])
                .arg(host_ldso)
                .arg(path)
                .status()
                .with_context(|| format!("patchelf --set-interpreter on {}",
                                         path.display()))?;
            if !status.success() {
                bail!("patchelf failed on {}: exit {}", path.display(), status);
            }
            elfs_patched += 1;
        }
    }

    if shebangs_patched + elfs_patched > 0 {
        eprintln!("    de-nixify: rewrote {} shebangs, patched {} ELF interpreters",
                  shebangs_patched, elfs_patched);
    }
    Ok(())
}

struct NixosFetcher;
impl DistroFetcher for NixosFetcher {
    fn latest_kernels(&self, src: &Source) -> Result<Vec<KernelPkg>> {
        let (jobset, attr) = nixos_jobset_attr(&src.release);
        let hydra_arch = nixos_hydra_arch(&src.arch)?;
        let outs = nixos_fetch_hydra(&jobset, attr, hydra_arch)
            .with_context(|| format!("Hydra lookup for nixos/{}", src.release))?;

        // nixname is "linux-X.Y.Z"; uname -r inside the booted kernel is "X.Y.Z".
        let version = outs.nixname.strip_prefix("linux-")
            .ok_or_else(|| anyhow!("unexpected nixname `{}` (expected linux-X.Y.Z)",
                                   outs.nixname))?
            .to_string();

        let out_nar = nixos_fetch_narinfo(&outs.out_path)?;
        let modules_nar = nixos_fetch_narinfo(&outs.modules_path)?;
        let dev_nar = nixos_fetch_narinfo(&outs.dev_path)?;

        let tag = |info: NarInfo, role: &str| PkgFile {
            url: info.url,
            sha256: None,
            role: Some(role.to_string()),
        };

        Ok(vec![KernelPkg {
            distro: src.distro.clone(),
            release: src.release.clone(),
            arch: src.arch.clone(),
            version,
            image:   tag(out_nar, "out"),
            headers: vec![
                tag(modules_nar, "modules"),
                tag(dev_nar, "dev"),
            ],
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
        "nixos"     => Ok(Box::new(NixosFetcher)),
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
    } else if fname.ends_with(".nar.xz") {
        extract_nar_xz(archive, dst)
    } else {
        bail!("unsupported archive extension: {}", fname)
    }
}

/// Decompress a `.nar.xz` and unpack the NAR into `dst`. The NAR's root
/// becomes the contents of `dst` (directories/files/symlinks land directly
/// under it). Compared to the deb/rpm/tar branches we don't shell out: xz
/// and NAR are both in-process via crates.
fn extract_nar_xz(archive: &Path, dst: &Path) -> Result<()> {
    let xz_bytes = fs::read(archive)
        .with_context(|| format!("reading {}", archive.display()))?;
    let mut nar_bytes = Vec::with_capacity(xz_bytes.len() * 4);
    lzma_rs::xz_decompress(&mut std::io::Cursor::new(&xz_bytes), &mut nar_bytes)
        .with_context(|| format!("xz-decompressing {}", archive.display()))?;
    let dec = nix_nar::Decoder::new(std::io::Cursor::new(&nar_bytes))
        .with_context(|| format!("opening NAR {}", archive.display()))?;
    dec.unpack(dst)
        .with_context(|| format!("unpacking NAR {} -> {}",
                                 archive.display(), dst.display()))?;
    Ok(())
}

/// Find the largest kernel-image file under `root`. The names that
/// appear in distro packages: `vmlinuz*` (most x86 distros), `bzImage*`
/// (occasionally on x86), `Image*` (arm64, and some nixos x86_64 too).
/// The real image is multi-MB; symlinks or stubs are smaller.
fn find_vmlinuz(root: &Path) -> Result<PathBuf> {
    let mut best: Option<(PathBuf, u64)> = None;
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry.context("walking for vmlinuz")?;
        if !entry.file_type().is_file() { continue }
        let name = entry.file_name().to_string_lossy();
        let is_image = name.starts_with("vmlinuz")
            || name.starts_with("bzImage")
            || name == "Image" || name.starts_with("Image.")
            || name == "zImage" || name.starts_with("zImage.");
        if !is_image { continue }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if size < 1024 * 1024 { continue }
        if best.as_ref().map(|(_, s)| size > *s).unwrap_or(true) {
            best = Some((entry.path().to_path_buf(), size));
        }
    }
    best.map(|(p, _)| p)
        .ok_or_else(|| anyhow!("no vmlinuz/bzImage/Image found under {}", root.display()))
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

/// Locate the lib dir containing klibc/bin/ and the klibc-<hash>.so interp.
/// On Debian/Ubuntu/Fedora this is `/usr/lib/`; on NixOS the klibc package
/// puts both under `$HOME/.nix-profile/lib/`. Probe a few candidates.
fn klibc_lib_dir() -> Result<PathBuf> {
    let mut candidates: Vec<PathBuf> = vec![PathBuf::from("/usr/lib")];
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(&home).join(".nix-profile/lib"));
    }
    candidates.push(PathBuf::from("/run/current-system/sw/lib"));
    for c in &candidates {
        if c.join("klibc/bin/sh").is_file() {
            return Ok(c.clone());
        }
    }
    bail!("klibc not found — tried {:?} (install klibc-utils + libklibc, \
           or `nix profile install nixpkgs#klibc`)", candidates)
}

/// Find klibc-<hash>.so — the runtime interpreter for klibc binaries.
fn klibc_interp_path(lib_dir: &Path) -> Result<PathBuf> {
    for entry in fs::read_dir(lib_dir)
        .with_context(|| format!("scanning {} for klibc interp", lib_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        let s = name.to_string_lossy();
        if s.starts_with("klibc-") && s.ends_with(".so") {
            return Ok(entry.path());
        }
    }
    bail!("no klibc-*.so under {}", lib_dir.display())
}

/// Run `depmod` against the staged modules tree to produce modules.dep
/// + friends. Distro .deb / .rpm packages ship `modules.order` and
/// `modules.builtin` but not the depmod-generated files — those are
/// generated by the package's post-install scripts on the target system.
/// We're not running post-installs, so we need to do this ourselves.
fn run_depmod(stage: &Path, uname_r: &str) -> Result<()> {
    // depmod isn't in $PATH for non-root users on standard distros, but on
    // NixOS it lives in /run/current-system/sw/bin/ (in $PATH). Check the
    // sbin locations first (a regular distro layout), then fall through to
    // walking $PATH.
    let mut candidates: Vec<PathBuf> = vec![
        PathBuf::from("/sbin/depmod"),
        PathBuf::from("/usr/sbin/depmod"),
    ];
    if let Ok(path) = std::env::var("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|p| p.join("depmod")));
    }
    let depmod_bin = candidates.iter()
        .find(|p| p.is_file())
        .ok_or_else(|| anyhow!("depmod not found in /sbin, /usr/sbin, or $PATH"))?;
    let status = Command::new(depmod_bin)
        .args(["-a", "-b"])
        .arg(stage)
        .arg(uname_r)
        .status()
        .with_context(|| format!("spawning {}", depmod_bin.display()))?;
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
    use std::os::unix::fs::PermissionsExt;
    let klibc_lib = klibc_lib_dir()?;
    let klibc_bin = klibc_lib.join("klibc/bin");
    let interp = klibc_interp_path(&klibc_lib)?;
    // Path inside the cpio where the interp will live; klibc tools' DT_INTERP
    // must match this exactly. On Debian the host already uses this path, so
    // patchelf is a no-op; on NixOS the original interp is /nix/store/... and
    // we have to rewrite.
    let cpio_interp = format!("/usr/lib/{}", interp.file_name().unwrap().to_string_lossy());
    for tool in ["sh", "mount", "insmod", "mkdir", "run-init"] {
        let src = klibc_bin.join(tool);
        let dst = irfs.join("bin").join(tool);
        fs::copy(&src, &dst)
            .with_context(|| format!("copying klibc {} from {}", tool, src.display()))?;
        fs::set_permissions(&dst, fs::Permissions::from_mode(0o755))?;
        let pe_status = Command::new("patchelf")
            .args(["--set-interpreter", &cpio_interp])
            .arg(&dst)
            .status()
            .with_context(|| format!("patchelf --set-interpreter on klibc {}", tool))?;
        if !pe_status.success() {
            bail!("patchelf --set-interpreter failed on klibc {}", tool);
        }
    }
    let interp_dst = irfs.join("usr/lib").join(interp.file_name().unwrap());
    fs::copy(&interp, &interp_dst)
        .with_context(|| format!("copying klibc interp {}", interp.display()))?;
    // Nix store files are 0444; ensure the cpio's copy is plain 0644.
    fs::set_permissions(&interp_dst, fs::Permissions::from_mode(0o644))?;

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
    //    Pair each downloaded path with the original PkgFile so role hints
    //    survive into the layout step.
    let files: Vec<&PkgFile> = std::iter::once(&pkg.image)
        .chain(pkg.headers.iter())
        .collect();
    let mut downloaded: Vec<(&PkgFile, PathBuf)> = Vec::with_capacity(files.len());
    for f in &files {
        let fname = f.url.rsplit('/').next().unwrap_or("download");
        let path = dl.join(fname);
        download_to(&f.url, &path)?;
        if let Some(expected) = &f.sha256 {
            verify_sha256(&path, expected)
                .with_context(|| format!("verifying {}", f.url))?;
        }
        downloaded.push((f, path));
    }

    // 2+3. Extract and lay out. Most distros: one merged tree, then heuristic
    //      find_vmlinuz/find_modules_dir/collect_headers. NixOS: per-role
    //      placement because its three NARs (out/modules/dev) have
    //      non-overlapping content and dangling symlinks to fix up.
    if pkg.distro == "nixos" {
        nixos_extract_and_layout(&downloaded, &extracted, &stage, &pkg.version)?;
    } else {
        for (_, archive) in &downloaded {
            extract_archive(archive, &extracted)
                .with_context(|| format!("extracting {}", archive.display()))?;
        }
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

        // Debian: linux-kbuild is a separate package (scripts/, tools/) the
        // headers symlinks point at; plus the headers Makefile hardcodes
        // /usr/src/<...> include paths that don't exist outside a Debian
        // install. See debian_fixup for the patchup.
        if pkg.distro == "debian" {
            debian_fixup(&extracted, &stage, &pkg.version)?;
        }
        // openSUSE: the obj/<arch>/<flavor>/Makefile hardcodes its
        // KBUILD_OUTPUT to the system install path. Rewrite to self-relative.
        if pkg.distro == "opensuse" {
            opensuse_fixup(&stage)?;
        }
    }
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
    let config: Config = json_five::from_str(&config_text)
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
