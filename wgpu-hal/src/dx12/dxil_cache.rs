//! A disk cache of DXC's output.
//!
//! DXC compiles every pipeline's HLSL on every run; for a large shader that is seconds. The cache stores the DXIL
//! container DXC returned, keyed by everything that decides it:
//!
//! - the HLSL source;
//! - the arguments `compile_dxc` passed (entry point, profile, HLSL version, strictness, debug and 16-bit flags);
//! - the compiler's identity: DXC's reported version and the contents of `dxcompiler.dll` and `dxil.dll`.
//!
//! It is opt-in: set `WGPU_DXIL_CACHE_DIR` to a directory. `WGPU_DXIL_CACHE_MAX_MB` caps its size (default 512);
//! past the cap the least recently used entries are deleted.
//!
//! An entry is written to a temporary file and renamed into place, so a reader sees a whole entry or none. Each
//! carries a header naming its key, its length and a checksum of its payload; an entry that fails any check is
//! deleted and the shader compiled as if it were missing. Nothing here can turn a compile into an error.

use alloc::{format, string::String, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const MAGIC: &[u8; 8] = b"WGPUDXIL";
/// Bump when the header or the key's composition changes, so old entries are misses rather than misreads.
const FORMAT_VERSION: u32 = 1;
const HEADER_LEN: usize = 8 + 4 + 16 + 8 + 8;
const DEFAULT_MAX_MB: u64 = 512;
/// A temporary file this old belongs to a writer that died; delete it.
const STALE_TMP_SECS: u64 = 3600;

/// A 128-bit hash of `parts`, each length-prefixed so `["ab", "c"]` and `["a", "bc"]` differ. SipHash-2-4 with two
/// fixed key pairs: a stable, documented algorithm, so keys mean the same thing from one run to the next.
#[allow(deprecated)]
pub(super) fn hash128(parts: &[&[u8]]) -> u128 {
    use core::hash::Hasher;
    let mut a = core::hash::SipHasher::new_with_keys(0x7767_7075_6478_696c, 0x6361_6368_6531_0001);
    let mut b = core::hash::SipHasher::new_with_keys(0x6478_696c_6361_6368, 0x7767_7075_0002_0002);
    for part in parts {
        a.write_u64(part.len() as u64);
        a.write(part);
        b.write_u64(part.len() as u64);
        b.write(part);
    }
    ((a.finish() as u128) << 64) | b.finish() as u128
}

pub(super) struct DxilCache {
    dir: PathBuf,
    max_bytes: u64,
    /// The compiler's identity, folded into every key.
    compiler: u128,
    /// The bytes of `*.dxil` entries in `dir`, as last counted plus what this process wrote since.
    total: AtomicU64,
    /// Distinguishes this process's temporary files from each other.
    counter: AtomicU64,
}

impl DxilCache {
    /// The cache `WGPU_DXIL_CACHE_DIR` names, for the compiler `compiler` identifies (asked only when the cache is
    /// on: it reads the compiler's libraries); `None` if it is unset or cannot be created.
    pub(super) fn from_env(compiler: impl FnOnce() -> u128) -> Option<Self> {
        let dir = std::env::var_os("WGPU_DXIL_CACHE_DIR").filter(|d| !d.is_empty())?;
        let compiler = compiler();
        let max_mb = std::env::var("WGPU_DXIL_CACHE_MAX_MB")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_MAX_MB);
        Self::new(
            PathBuf::from(dir),
            max_mb.saturating_mul(1024 * 1024),
            compiler,
        )
    }

    pub(super) fn new(dir: PathBuf, max_bytes: u64, compiler: u128) -> Option<Self> {
        if let Err(e) = fs::create_dir_all(&dir) {
            log::warn!("DXIL cache disabled: cannot create {}: {e}", dir.display());
            return None;
        }
        let cache = Self {
            dir,
            max_bytes,
            compiler,
            total: AtomicU64::new(0),
            counter: AtomicU64::new(0),
        };
        cache.trim();
        log::info!(
            "DXIL cache at {} ({} MB of {} MB)",
            cache.dir.display(),
            cache.total.load(Ordering::Relaxed) / (1024 * 1024),
            cache.max_bytes / (1024 * 1024)
        );
        Some(cache)
    }

    /// The key for `source` compiled with `args`.
    pub(super) fn key(&self, source: &str, args: &[String]) -> u128 {
        let mut parts: Vec<&[u8]> = Vec::with_capacity(args.len() + 3);
        let compiler = self.compiler.to_le_bytes();
        let version = FORMAT_VERSION.to_le_bytes();
        parts.push(&version);
        parts.push(&compiler);
        parts.extend(args.iter().map(|a| a.as_bytes()));
        parts.push(source.as_bytes());
        hash128(&parts)
    }

    fn path(&self, key: u128) -> PathBuf {
        self.dir.join(format!("{key:032x}.dxil"))
    }

    /// The DXIL stored under `key`, if a whole and valid entry is there.
    pub(super) fn load(&self, key: u128) -> Option<Vec<u8>> {
        let path = self.path(key);
        let bytes = fs::read(&path).ok()?;
        match decode(&bytes, key) {
            Ok(payload) => {
                // Best effort: the trim deletes the least recently USED, not the least recently written.
                if let Ok(file) = fs::File::options().write(true).open(&path) {
                    let _ = file.set_modified(std::time::SystemTime::now());
                }
                Some(payload.to_vec())
            }
            Err(why) => {
                log::warn!(
                    "DXIL cache: discarding {} ({why}); compiling",
                    path.display()
                );
                if fs::remove_file(&path).is_ok() {
                    self.total.fetch_sub(bytes.len() as u64, Ordering::Relaxed);
                }
                None
            }
        }
    }

    /// Store `dxil` under `key`. Failures are logged and otherwise ignored: the cache is an optimisation.
    pub(super) fn store(&self, key: u128, dxil: &[u8]) {
        let entry = encode(key, dxil);
        let tmp = self.dir.join(format!(
            "{key:032x}.{}.{}.tmp",
            std::process::id(),
            self.counter.fetch_add(1, Ordering::Relaxed)
        ));
        let written = fs::File::create(&tmp)
            .and_then(|mut file| file.write_all(&entry))
            .and_then(|()| fs::rename(&tmp, self.path(key)));
        if let Err(e) = written {
            log::warn!("DXIL cache: cannot write {}: {e}", tmp.display());
            let _ = fs::remove_file(&tmp);
            return;
        }
        let total =
            self.total.fetch_add(entry.len() as u64, Ordering::Relaxed) + entry.len() as u64;
        if total > self.max_bytes {
            self.trim();
        }
    }

    /// Recount the directory; delete stale temporary files, and the least recently used entries until the total is
    /// under three quarters of the cap.
    fn trim(&self) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        let now = std::time::SystemTime::now();
        let mut kept = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            let modified = meta.modified().unwrap_or(now);
            match path.extension().and_then(|e| e.to_str()) {
                Some("dxil") => kept.push((modified, meta.len(), path)),
                Some("tmp") => {
                    let age = now.duration_since(modified).map_or(0, |d| d.as_secs());
                    if age > STALE_TMP_SECS {
                        let _ = fs::remove_file(&path);
                    }
                }
                _ => {}
            }
        }
        let mut total: u64 = kept.iter().map(|(_, len, _)| len).sum();
        if total > self.max_bytes {
            let target = self.max_bytes / 4 * 3;
            kept.sort_by_key(|(modified, ..)| *modified);
            for (_, len, path) in &kept {
                if total <= target {
                    break;
                }
                if fs::remove_file(path).is_ok() {
                    total -= len;
                }
            }
        }
        self.total.store(total, Ordering::Relaxed);
    }

    #[cfg(test)]
    fn dir(&self) -> &Path {
        &self.dir
    }
}

fn encode(key: u128, dxil: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + dxil.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&key.to_le_bytes());
    out.extend_from_slice(&(dxil.len() as u64).to_le_bytes());
    out.extend_from_slice(&(hash128(&[dxil]) as u64).to_le_bytes());
    out.extend_from_slice(dxil);
    out
}

fn decode(bytes: &[u8], key: u128) -> Result<&[u8], &'static str> {
    if bytes.len() < HEADER_LEN {
        return Err("shorter than its header");
    }
    let (header, payload) = bytes.split_at(HEADER_LEN);
    let field = |at: usize, len: usize| &header[at..at + len];
    if field(0, 8) != MAGIC {
        return Err("not a cache entry");
    }
    if field(8, 4) != FORMAT_VERSION.to_le_bytes() {
        return Err("another format version");
    }
    if field(12, 16) != key.to_le_bytes() {
        return Err("another key");
    }
    if field(28, 8) != (payload.len() as u64).to_le_bytes() {
        return Err("truncated");
    }
    if field(36, 8) != (hash128(&[payload]) as u64).to_le_bytes() {
        return Err("checksum mismatch");
    }
    // Every DXIL container DXC writes starts with this.
    if !payload.starts_with(b"DXBC") {
        return Err("not a DXIL container");
    }
    Ok(payload)
}

/// The identity of the DXC that compiles: its reported version and the bytes of the two libraries it runs, so a
/// changed `dxcompiler.dll` or `dxil.dll` is a different compiler even at the same version. A library that cannot be
/// read is named as such rather than skipped.
pub(super) fn compiler_identity(version: (u32, u32), libraries: &[(&str, Option<&Path>)]) -> u128 {
    let mut parts: Vec<Vec<u8>> = Vec::new();
    parts.push(format!("dxc {}.{}", version.0, version.1).into_bytes());
    for (name, path) in libraries {
        let contents = path.and_then(|p| fs::read(p).ok());
        match contents {
            Some(bytes) => parts.push(format!("{name}:{:032x}", hash128(&[&bytes])).into_bytes()),
            None => parts.push(format!("{name}:unreadable").into_bytes()),
        }
    }
    let refs: Vec<&[u8]> = parts.iter().map(|p| p.as_slice()).collect();
    hash128(&refs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dxil(n: usize) -> Vec<u8> {
        let mut v = b"DXBC".to_vec();
        v.extend((0..n).map(|i| (i * 31 % 251) as u8));
        v
    }

    fn fresh(name: &str, max_bytes: u64) -> DxilCache {
        let dir = std::env::temp_dir().join(format!(
            "wgpu-dxil-cache-test-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        DxilCache::new(dir, max_bytes, 7).expect("cache")
    }

    fn args(entry: &str) -> Vec<String> {
        ["-E", entry, "-T", "ps_6_0", "-HV", "2018"]
            .iter()
            .map(|s| String::from(*s))
            .collect()
    }

    #[test]
    fn a_stored_entry_loads_back_and_a_missing_one_is_a_miss() {
        let cache = fresh("roundtrip", 1 << 20);
        let key = cache.key("float4 main() : SV_Target { return 1; }", &args("main"));
        assert_eq!(cache.load(key), None);
        cache.store(key, &dxil(1000));
        assert_eq!(cache.load(key), Some(dxil(1000)));
        let _ = fs::remove_dir_all(cache.dir());
    }

    #[test]
    fn every_input_moves_the_key() {
        let cache = fresh("key", 1 << 20);
        let base = cache.key("source", &args("main"));
        assert_ne!(base, cache.key("source ", &args("main")), "source");
        assert_ne!(base, cache.key("source", &args("other")), "entry point");
        let mut debug = args("main");
        debug.push(String::from("-Zi"));
        assert_ne!(base, cache.key("source", &debug), "debug flag");
        let other = DxilCache::new(cache.dir().to_path_buf(), 1 << 20, 8).unwrap();
        assert_ne!(
            base,
            other.key("source", &args("main")),
            "compiler identity"
        );
        assert_eq!(
            base,
            cache.key("source", &args("main")),
            "the key is stable"
        );
        let _ = fs::remove_dir_all(cache.dir());
    }

    #[test]
    fn a_corrupt_entry_is_discarded_and_reads_as_a_miss() {
        let cache = fresh("corrupt", 1 << 20);
        let key = cache.key("source", &args("main"));
        let path = cache.path(key);
        let whole = encode(key, &dxil(500));
        for (what, bytes) in [
            ("truncated", whole[..whole.len() - 7].to_vec()),
            ("header only", whole[..HEADER_LEN - 1].to_vec()),
            ("empty", Vec::new()),
            ("flipped payload byte", {
                let mut b = whole.clone();
                let at = b.len() - 3;
                b[at] ^= 0x40;
                b
            }),
            ("another key's entry", encode(key ^ 1, &dxil(500))),
            ("not DXIL", encode(key, b"XXXXnot a container")),
        ] {
            fs::write(&path, &bytes).unwrap();
            assert_eq!(cache.load(key), None, "{what}: must read as a miss");
            assert!(!path.exists(), "{what}: must be deleted");
        }
        let _ = fs::remove_dir_all(cache.dir());
    }

    #[test]
    fn past_the_cap_the_least_recently_used_entries_go() {
        // Each entry is ~10 KB; a 50 KB cap holds four, and trims to 37.5 KB when it overflows.
        let cache = fresh("cap", 50_000);
        let keys: Vec<u128> = (0..8)
            .map(|i| cache.key(&format!("s{i}"), &args("main")))
            .collect();
        for (i, key) in keys.iter().enumerate() {
            cache.store(*key, &dxil(10_000));
            // Use the first entry after every store: the most recently USED survives, not the most recently written.
            if i > 0 {
                std::thread::sleep(std::time::Duration::from_millis(20));
                assert!(
                    cache.load(keys[0]).is_some(),
                    "the entry in use was evicted after store {i}"
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let present: Vec<bool> = keys.iter().map(|k| cache.path(*k).exists()).collect();
        let total: u64 = fs::read_dir(cache.dir())
            .unwrap()
            .flatten()
            .map(|e| e.metadata().unwrap().len())
            .sum();
        assert!(
            total <= 50_000,
            "{total} bytes on disk past a 50000 cap: {present:?}"
        );
        assert!(present[0], "the entry in use was evicted: {present:?}");
        assert!(present[7], "the newest entry was evicted: {present:?}");
        assert!(
            !present[1],
            "the least recently used entry survived: {present:?}"
        );
        let _ = fs::remove_dir_all(cache.dir());
    }

    #[test]
    fn a_stale_temporary_file_is_swept_and_a_fresh_one_is_left() {
        let cache = fresh("tmp", 1 << 20);
        let stale = cache.dir().join("dead.1.0.tmp");
        let live = cache.dir().join("live.1.1.tmp");
        fs::write(&stale, b"partial").unwrap();
        fs::write(&live, b"partial").unwrap();
        let old =
            std::time::SystemTime::now() - std::time::Duration::from_secs(STALE_TMP_SECS + 60);
        fs::File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_modified(old)
            .unwrap();
        cache.trim();
        assert!(!stale.exists() && live.exists());
        let _ = fs::remove_dir_all(cache.dir());
    }

    #[test]
    fn a_changed_library_is_a_different_compiler() {
        let dir = std::env::temp_dir().join(format!("wgpu-dxil-identity-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let dll = dir.join("dxil.dll");
        fs::write(&dll, b"one").unwrap();
        let a = compiler_identity((1, 8), &[("dxil.dll", Some(&dll))]);
        fs::write(&dll, b"two").unwrap();
        let b = compiler_identity((1, 8), &[("dxil.dll", Some(&dll))]);
        let c = compiler_identity((1, 9), &[("dxil.dll", Some(&dll))]);
        let missing = compiler_identity((1, 9), &[("dxil.dll", None)]);
        assert!(a != b && b != c && c != missing);
        let _ = fs::remove_dir_all(&dir);
    }
}
