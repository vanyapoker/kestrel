//! File properties beyond what a directory listing carries: owner, symlink target,
//! checksums, and changing permissions and ownership.

use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq)]
pub struct Props {
    pub owner: String,
    pub group: String,
    /// Where a symlink points, and whether that exists.
    pub link: Option<(PathBuf, bool)>,
}

pub fn read(path: &Path) -> Option<Props> {
    let meta = fs::symlink_metadata(path).ok()?;
    let link = meta.is_symlink().then(|| {
        let target = fs::read_link(path).unwrap_or_default();
        (target, fs::metadata(path).is_ok())
    });
    Some(Props {
        owner: user_name(meta.uid()),
        group: group_name(meta.gid()),
        link,
    })
}

/// Starting size for the buffers `getpw*_r`/`getgr*_r` fill; they report ERANGE if too small.
const LOOKUP_BUF: usize = 1024;

/// Runs a reentrant libc lookup, growing the buffer while it reports ERANGE.
fn lookup(mut call: impl FnMut(*mut libc::c_char, usize) -> libc::c_int) -> Option<()> {
    let mut size = LOOKUP_BUF;
    loop {
        let mut buf = vec![0 as libc::c_char; size];
        match call(buf.as_mut_ptr(), size) {
            0 => return Some(()),
            libc::ERANGE if size < 1 << 20 => size *= 4,
            _ => return None,
        }
    }
}

/// The user name for `uid` from the system databases (passwd, homed, LDAP…), else the number.
fn user_name(uid: u32) -> String {
    let mut pwd: libc::passwd = unsafe_zeroed();
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let mut name = None;
    lookup(|buf, len| {
        // SAFETY: `pwd`, `buf` and `result` are valid for the call; `buf` holds `len` bytes.
        let rc = unsafe { libc::getpwuid_r(uid, &mut pwd, buf, len, &mut result) };
        if rc == 0 && !result.is_null() {
            // SAFETY: on success `pw_name` is a NUL-terminated string inside `buf`.
            name = Some(
                unsafe { std::ffi::CStr::from_ptr(pwd.pw_name) }
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        rc
    });
    name.unwrap_or_else(|| uid.to_string())
}

fn group_name(gid: u32) -> String {
    let mut grp: libc::group = unsafe_zeroed();
    let mut result: *mut libc::group = std::ptr::null_mut();
    let mut name = None;
    lookup(|buf, len| {
        // SAFETY: `grp`, `buf` and `result` are valid for the call; `buf` holds `len` bytes.
        let rc = unsafe { libc::getgrgid_r(gid, &mut grp, buf, len, &mut result) };
        if rc == 0 && !result.is_null() {
            // SAFETY: on success `gr_name` is a NUL-terminated string inside `buf`.
            name = Some(
                unsafe { std::ffi::CStr::from_ptr(grp.gr_name) }
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        rc
    });
    name.unwrap_or_else(|| gid.to_string())
}

/// A zeroed libc record, to be filled in by a lookup call.
fn unsafe_zeroed<T>() -> T {
    // SAFETY: only used for the plain-old-data `passwd` and `group` structs, for which
    // all-zero bytes (null pointers, zero ids) is a valid value.
    unsafe { std::mem::zeroed() }
}

fn user_id(name: &str) -> Option<u32> {
    if let Ok(id) = name.parse() {
        return Some(id);
    }
    let cname = std::ffi::CString::new(name).ok()?;
    let mut pwd: libc::passwd = unsafe_zeroed();
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let mut found = None;
    lookup(|buf, len| {
        // SAFETY: as in `user_name`; `cname` is a valid C string.
        let rc = unsafe { libc::getpwnam_r(cname.as_ptr(), &mut pwd, buf, len, &mut result) };
        if rc == 0 && !result.is_null() {
            found = Some(pwd.pw_uid);
        }
        rc
    });
    found
}

fn group_id(name: &str) -> Option<u32> {
    if let Ok(id) = name.parse() {
        return Some(id);
    }
    let cname = std::ffi::CString::new(name).ok()?;
    let mut grp: libc::group = unsafe_zeroed();
    let mut result: *mut libc::group = std::ptr::null_mut();
    let mut found = None;
    lookup(|buf, len| {
        // SAFETY: as in `group_name`; `cname` is a valid C string.
        let rc = unsafe { libc::getgrnam_r(cname.as_ptr(), &mut grp, buf, len, &mut result) };
        if rc == 0 && !result.is_null() {
            found = Some(grp.gr_gid);
        }
        rc
    });
    found
}

/// A permission/ownership change. `None` leaves that part alone.
#[derive(Clone, Debug, Default)]
pub struct Change {
    pub mode: Option<u32>,
    pub owner: Option<String>,
    pub group: Option<String>,
    /// Also apply to everything inside a folder (symlinks are skipped).
    pub recursive: bool,
}

/// Applies `change` to `path`, returning one message per failure.
pub fn apply(path: &Path, change: &Change) -> Vec<String> {
    let mut errors = Vec::new();
    let uid = change.owner.as_deref().map(user_id);
    let gid = change.group.as_deref().map(group_id);
    if matches!(uid, Some(None)) {
        errors.push(format!(
            "Unknown user \"{}\"",
            change.owner.as_deref().unwrap_or("")
        ));
    }
    if matches!(gid, Some(None)) {
        errors.push(format!(
            "Unknown group \"{}\"",
            change.group.as_deref().unwrap_or("")
        ));
    }
    if !errors.is_empty() {
        return errors;
    }
    let (uid, gid) = (uid.flatten(), gid.flatten());
    apply_one(path, change, uid, gid, true, &mut errors);
    errors
}

fn apply_one(
    path: &Path,
    change: &Change,
    uid: Option<u32>,
    gid: Option<u32>,
    top: bool,
    errors: &mut Vec<String>,
) {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return;
    };
    if meta.is_symlink() && !top {
        return;
    }
    let is_dir = fs::metadata(path).is_ok_and(|m| m.is_dir());
    let mut report = |what: &str, r: io::Result<()>| {
        if let Err(e) = r {
            errors.push(format!("{what} {}: {e}", path.display()));
        }
    };
    if let Some(mode) = change.mode {
        let current = fs::metadata(path).map(|m| m.mode()).unwrap_or(0);
        report(
            "chmod",
            fs::set_permissions(
                path,
                fs::Permissions::from_mode(mode_for(mode, current, is_dir, top)),
            ),
        );
    }
    if uid.is_some() || gid.is_some() {
        report("chown", std::os::unix::fs::chown(path, uid, gid));
    }
    if change.recursive
        && is_dir
        && let Ok(rd) = fs::read_dir(path)
    {
        for entry in rd.flatten() {
            apply_one(&entry.path(), change, uid, gid, false, errors);
        }
    }
}

/// The mode to set on an item. Inside a recursive change, files only keep an execute
/// bit where they were executable to begin with.
fn mode_for(wanted: u32, current: u32, is_dir: bool, top: bool) -> u32 {
    let wanted = wanted & 0o7777;
    if top || is_dir || current & 0o111 != 0 {
        wanted
    } else {
        wanted & !0o111
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Algo {
    Sha256,
    Md5,
}

impl Algo {
    pub const ALL: [Algo; 2] = [Algo::Sha256, Algo::Md5];

    pub fn label(self) -> &'static str {
        match self {
            Algo::Sha256 => "SHA-256",
            Algo::Md5 => "MD5",
        }
    }

    fn tool(self) -> &'static str {
        match self {
            Algo::Sha256 => "sha256sum",
            Algo::Md5 => "md5sum",
        }
    }
}

/// Checksums of one file, with the modification time they were computed for.
pub type Sums = (
    Option<std::time::SystemTime>,
    Vec<(Algo, Result<String, String>)>,
);

/// Hex digest of a file, computed by the coreutils tool.
pub fn checksum(path: &Path, algo: Algo) -> Result<String, String> {
    let mut cmd = std::process::Command::new(algo.tool());
    cmd.arg("--").arg(path);
    let out = crate::thumbs::run_tool_for(&mut cmd, Duration::from_secs(30 * 60))?;
    String::from_utf8_lossy(&out)
        .split_whitespace()
        .next()
        .map(str::to_string)
        .ok_or_else(|| "No output".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kestrel-props-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn reads_owner_and_link_target() {
        let d = tmp("read");
        fs::write(d.join("f"), "x").unwrap();
        std::os::unix::fs::symlink("f", d.join("l")).unwrap();
        std::os::unix::fs::symlink("nowhere", d.join("broken")).unwrap();
        assert!(!read(&d.join("f")).unwrap().owner.is_empty());
        assert_eq!(read(&d.join("f")).unwrap().link, None);
        assert_eq!(
            read(&d.join("l")).unwrap().link,
            Some((PathBuf::from("f"), true))
        );
        assert_eq!(
            read(&d.join("broken")).unwrap().link,
            Some((PathBuf::from("nowhere"), false))
        );
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn chmod_recursive_keeps_plain_files_non_executable() {
        let d = tmp("chmod");
        fs::create_dir(d.join("sub")).unwrap();
        fs::write(d.join("sub/plain"), "x").unwrap();
        fs::write(d.join("sub/tool"), "x").unwrap();
        fs::set_permissions(d.join("sub/tool"), fs::Permissions::from_mode(0o700)).unwrap();
        let change = Change {
            mode: Some(0o755),
            recursive: true,
            ..Default::default()
        };
        assert!(apply(&d, &change).is_empty());
        let mode = |p: &str| fs::metadata(d.join(p)).unwrap().mode() & 0o777;
        assert_eq!(mode("sub"), 0o755);
        assert_eq!(mode("sub/plain"), 0o644);
        assert_eq!(mode("sub/tool"), 0o755);
        assert!(
            apply(
                &d,
                &Change {
                    owner: Some("no-such-user-xyz".into()),
                    ..Default::default()
                }
            )[0]
            .contains("Unknown user")
        );
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn checksums_match_known_values() {
        let d = tmp("sum");
        fs::write(d.join("f"), "abc").unwrap();
        assert_eq!(
            checksum(&d.join("f"), Algo::Md5).unwrap(),
            "900150983cd24fb0d6963f7d28e17f72"
        );
        assert!(
            checksum(&d.join("f"), Algo::Sha256)
                .unwrap()
                .starts_with("ba7816bf")
        );
        fs::remove_dir_all(d).unwrap();
    }
}
