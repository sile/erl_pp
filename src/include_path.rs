//! Erlang/OTP 29.0 compatible include path resolver.
//!
//! [`open_include`] is a standalone utility that a filesystem-backed
//! caller can invoke in response to [`crate::Event::AwaitingInclude`].
//! The Sans-I/O [`crate::Preprocessor`] never calls this itself; the
//! caller is free to use a different resolver, and this module can be
//! ignored entirely by callers that only feed in-memory sources.
//!
//! # Compatibility
//!
//! Behavior mirrors OTP 29.0 as observed in:
//!
//! - `lib/stdlib/src/epp.erl` (`scan_include` / `scan_include_lib` /
//!   `enter_file` / `enter_file2` / `expand_var`)
//! - `lib/kernel/src/file.erl` (`path_open`)
//! - `lib/compiler/src/compile.erl` (`do_parse_module`)
//! - `lib/kernel/src/code_server.erl` (`code:lib_dir/1`) — supplied
//!   by the caller through `app_lookup`
//!
//! The implementation is a snapshot pinned to OTP 29.0; later OTP
//! versions that change the search rules will not be tracked here.
//! To validate against OTP, feed the same directory layout to
//! `epp:parse_file/2` and compare the accepted / rejected paths — see
//! the module test suite for the expected trace shape.
//!
//! # What this module does NOT do
//!
//! - Automatic invocation from [`crate::Preprocessor`]
//! - Cycle detection, include depth, source-size, or total-read limits
//! - Encoding conversion after read open
//! - Async filesystem access
//! - Virtual filesystem abstractions
//! - Detecting the running Erlang VM's code path (the caller writes
//!   `app_lookup`)
//! - Prepending the current source's directory (or `-I`) to
//!   `include_paths` (the caller assembles the search list)

use std::env::VarError;
use std::fs::File;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::event::{IncludeKind, IncludeRequest};

/// Successful resolution of an include: the path that was opened and
/// the file handle produced by the successful `File::open`.
///
/// The caller does not need to re-open the file. The path is not
/// canonicalized — symlinks and `..` components are preserved
/// verbatim from the search that succeeded.
#[derive(Debug)]
pub struct OpenedInclude {
    /// Full path that resolved successfully (not canonicalized).
    pub path: PathBuf,
    /// Read-opened handle to `path`.
    pub file: File,
}

/// Failure modes of [`open_include`].
///
/// Flat five-variant enum. The caller inspects the variant to decide
/// how to render or act on the failure — this module does not embed
/// source-span information in its error messages (that is the
/// caller's responsibility, using [`IncludeRequest::directive_span`]).
#[derive(Debug)]
pub enum OpenIncludeError {
    /// Ordinary include path search exhausted every candidate with
    /// `NotFound` / `NotADirectory`.
    NotFound,
    /// `include_lib` requested an application whose lookup returned
    /// `None`.
    AppNotFound {
        /// Application name that could not be resolved.
        app: String,
    },
    /// `include_lib` fell back to the application directory but the
    /// remaining path components failed to open with `NotFound` /
    /// `NotADirectory`.
    AppFileNotFound {
        /// Application name that resolved to a directory.
        app: String,
        /// Remaining path components under the application directory.
        tail: PathBuf,
    },
    /// A path or environment value could not be treated as a valid
    /// filename on this platform (e.g. `std::env::var` returned
    /// `VarError::NotUnicode`, or `File::open` returned
    /// `InvalidInput` for a NUL byte in the path).
    InvalidPath {
        /// The offending path.
        path: PathBuf,
    },
    /// Any other I/O error surfaced during the search (permission
    /// denied, disk error, etc.).
    Io(io::Error),
}

impl std::fmt::Display for OpenIncludeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => {
                f.write_str("include file was not found in any of the given include paths")
            }
            Self::AppNotFound { app } => {
                write!(f, "include_lib: application {app:?} was not found")
            }
            Self::AppFileNotFound { app, tail } => write!(
                f,
                "include_lib: file {} not found under application {app:?}",
                tail.display()
            ),
            Self::InvalidPath { path } => write!(f, "invalid path: {}", path.display()),
            Self::Io(e) => write!(f, "I/O error while opening include: {e}"),
        }
    }
}

impl std::error::Error for OpenIncludeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

/// Resolve an [`IncludeRequest`] against `include_paths` and, for
/// `include_lib`, an `app_lookup` callback that maps an application
/// name to its root directory.
///
/// # Search algorithm (OTP 29.0)
///
/// 1. If the raw path begins with `$`, its leading `[A-Za-z0-9_]+`
///    identifier is expanded via the process environment
///    (`std::env::var`). An undefined variable leaves `$VAR` intact,
///    matching `epp:expand_var/1`. A `VarError::NotUnicode` aborts
///    early with [`OpenIncludeError::InvalidPath`].
/// 2. The expanded path is classified as one of Erlang's
///    `filename:pathtype/1` results:
///    - `absolute` — `Path::is_absolute()` is true
///    - `volumerelative` — Windows-style rooted-but-not-absolute
///      (e.g. `C:foo`, `\foo`)
///    - `relative` — everything else
/// 3. `absolute` and `volumerelative` paths are opened directly and
///    do not consult `include_paths`.
/// 4. `relative` paths are tried against each entry of
///    `include_paths` in order, using `File::open` to decide success.
///    Only [`io::ErrorKind::NotFound`] and
///    [`io::ErrorKind::NotADirectory`] advance to the next candidate.
///    - `include`: any other I/O error is returned immediately as
///      [`OpenIncludeError::Io`] (no fallback).
///    - `include_lib`: any failure (I/O errors included) proceeds to
///      the application fallback, matching OTP `scan_include_lib`.
/// 5. `include_lib` fallback: the leading path component is treated
///    as the application name and passed to `app_lookup`. On
///    `Some(app_dir)`, `app_dir.join(tail)` is opened directly;
///    `NotFound` / `NotADirectory` there yield
///    [`OpenIncludeError::AppFileNotFound`], other I/O errors yield
///    [`OpenIncludeError::Io`]. `None` yields
///    [`OpenIncludeError::AppNotFound`].
///
/// # Non-goals
///
/// See the module docs for what this function deliberately does not
/// do (canonicalization, cycle detection, encoding conversion, etc.).
pub fn open_include<AppLookup>(
    request: &IncludeRequest,
    include_paths: &[PathBuf],
    app_lookup: AppLookup,
) -> Result<OpenedInclude, OpenIncludeError>
where
    AppLookup: Fn(&str) -> Option<PathBuf>,
{
    if let Some(rest) = request.path.as_str().strip_prefix('$') {
        let (name, _tail) = split_var_ident(rest);
        if !name.is_empty() {
            match std::env::var(name) {
                Ok(_) | Err(VarError::NotPresent) => {}
                Err(VarError::NotUnicode(_)) => {
                    return Err(OpenIncludeError::InvalidPath {
                        path: PathBuf::from(request.path.as_str()),
                    });
                }
            }
        }
    }
    open_include_with_env(request, include_paths, app_lookup, |name| {
        std::env::var(name).ok()
    })
}

/// Testable core of [`open_include`] that takes an `EnvLookup`
/// closure instead of reading the process environment. Kept
/// crate-visible so unit tests can drive environment expansion
/// without touching `std::env::set_var` (unsafe in Rust 2024).
pub(crate) fn open_include_with_env<AppLookup, EnvLookup>(
    request: &IncludeRequest,
    include_paths: &[PathBuf],
    app_lookup: AppLookup,
    env_lookup: EnvLookup,
) -> Result<OpenedInclude, OpenIncludeError>
where
    AppLookup: Fn(&str) -> Option<PathBuf>,
    EnvLookup: Fn(&str) -> Option<String>,
{
    let expanded = expand_var(request.path.as_str(), &env_lookup);
    let candidate = PathBuf::from(&expanded);

    match path_type(&candidate) {
        PathType::Absolute | PathType::VolumeRelative => open_direct(&candidate),
        PathType::Relative => match request.kind {
            IncludeKind::Include => {
                match search_relative(&candidate, include_paths, /* fallback_on_io = */ false)? {
                    Some(opened) => Ok(opened),
                    None => Err(OpenIncludeError::NotFound),
                }
            }
            IncludeKind::IncludeLib => {
                match search_relative(&candidate, include_paths, /* fallback_on_io = */ true)? {
                    Some(opened) => Ok(opened),
                    None => include_lib_fallback(&candidate, &app_lookup),
                }
            }
        },
    }
}

fn open_direct(p: &Path) -> Result<OpenedInclude, OpenIncludeError> {
    match File::open(p) {
        Ok(file) => Ok(OpenedInclude {
            path: p.to_path_buf(),
            file,
        }),
        Err(e) => Err(classify_open_error(e, p)),
    }
}

fn search_relative(
    candidate: &Path,
    include_paths: &[PathBuf],
    fallback_on_io: bool,
) -> Result<Option<OpenedInclude>, OpenIncludeError> {
    for dir in include_paths {
        let full = dir.join(candidate);
        match File::open(&full) {
            Ok(file) => return Ok(Some(OpenedInclude { path: full, file })),
            Err(e) => match e.kind() {
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => continue,
                _ => {
                    if fallback_on_io {
                        return Ok(None);
                    }
                    return Err(classify_open_error(e, &full));
                }
            },
        }
    }
    Ok(None)
}

fn include_lib_fallback<AppLookup>(
    candidate: &Path,
    app_lookup: &AppLookup,
) -> Result<OpenedInclude, OpenIncludeError>
where
    AppLookup: Fn(&str) -> Option<PathBuf>,
{
    let mut components = candidate.components();
    let head = match components.next() {
        Some(Component::Normal(s)) => match s.to_str() {
            Some(s) => s.to_owned(),
            None => {
                return Err(OpenIncludeError::InvalidPath {
                    path: candidate.to_path_buf(),
                });
            }
        },
        _ => {
            return Err(OpenIncludeError::InvalidPath {
                path: candidate.to_path_buf(),
            });
        }
    };
    let tail: PathBuf = components.collect();
    match app_lookup(&head) {
        None => Err(OpenIncludeError::AppNotFound { app: head }),
        Some(app_dir) => {
            let full = if tail.as_os_str().is_empty() {
                app_dir
            } else {
                app_dir.join(&tail)
            };
            match File::open(&full) {
                Ok(file) => Ok(OpenedInclude { path: full, file }),
                Err(e) => match e.kind() {
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => {
                        Err(OpenIncludeError::AppFileNotFound { app: head, tail })
                    }
                    io::ErrorKind::InvalidInput => {
                        Err(OpenIncludeError::InvalidPath { path: full })
                    }
                    _ => Err(OpenIncludeError::Io(e)),
                },
            }
        }
    }
}

fn classify_open_error(e: io::Error, path: &Path) -> OpenIncludeError {
    match e.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => OpenIncludeError::NotFound,
        io::ErrorKind::InvalidInput => OpenIncludeError::InvalidPath {
            path: path.to_path_buf(),
        },
        _ => OpenIncludeError::Io(e),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathType {
    Absolute,
    VolumeRelative,
    Relative,
}

fn path_type(p: &Path) -> PathType {
    if p.is_absolute() {
        PathType::Absolute
    } else if p.has_root() || p.components().any(|c| matches!(c, Component::Prefix(_))) {
        PathType::VolumeRelative
    } else {
        PathType::Relative
    }
}

fn expand_var<F>(raw: &str, env_lookup: &F) -> String
where
    F: Fn(&str) -> Option<String>,
{
    let Some(rest) = raw.strip_prefix('$') else {
        return raw.to_owned();
    };
    let (name, tail) = split_var_ident(rest);
    if name.is_empty() {
        return raw.to_owned();
    }
    match env_lookup(name) {
        Some(v) => {
            let mut out = v;
            out.push_str(tail);
            out
        }
        None => raw.to_owned(),
    }
}

fn split_var_ident(s: &str) -> (&str, &str) {
    let end = s
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
        .count();
    s.split_at(end)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use erl_tokenize::Position;

    use crate::event::{IncludeKind, IncludeRequest};
    use crate::origin::Origin;
    use crate::source::{Source, SourceSpan, SourceStore};
    use crate::source_string::SourceString;

    use super::*;

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let mut base = std::env::temp_dir();
            base.push(format!("erl_pp_it_{label}_{pid}_{n}"));
            std::fs::create_dir_all(&base).expect("create tempdir");
            Self { path: base }
        }

        fn write(&self, name: &str, content: &[u8]) -> PathBuf {
            let p = self.path.join(name);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).expect("mkdir parent");
            }
            std::fs::write(&p, content).expect("write");
            p
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn dummy_request(kind: IncludeKind, path: &str) -> IncludeRequest {
        let store = SourceStore::new();
        let id = store.append(Source::from_text("m.erl", "-include(...)."));
        let span = SourceSpan::new(id, Position::new(), Position::new());
        IncludeRequest {
            kind,
            path: SourceString::new(path, span),
            directive_span: span,
            parent_origin: Arc::new(Origin::Source),
        }
    }

    // expand_var ------------------------------------------------------------

    #[test]
    fn expand_var_no_dollar_is_verbatim() {
        let out = expand_var("hdr.hrl", &|_| unreachable!());
        assert_eq!(out, "hdr.hrl");
    }

    #[test]
    fn expand_var_replaces_leading_identifier() {
        let env = |name: &str| (name == "MYINC").then(|| "/opt/erlang".to_string());
        assert_eq!(expand_var("$MYINC/hdr.hrl", &env), "/opt/erlang/hdr.hrl");
    }

    #[test]
    fn expand_var_leaves_undefined_var_intact() {
        assert_eq!(expand_var("$UNDEF/hdr.hrl", &|_| None), "$UNDEF/hdr.hrl");
    }

    #[test]
    fn expand_var_bare_dollar_kept() {
        assert_eq!(expand_var("$/hdr.hrl", &|_| unreachable!()), "$/hdr.hrl");
    }

    #[test]
    fn expand_var_identifier_stops_at_non_ident_char() {
        let env = |name: &str| (name == "A").then(|| "X".to_string());
        assert_eq!(expand_var("$A-suffix", &env), "X-suffix");
    }

    // path_type -------------------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn path_type_unix_classification() {
        assert_eq!(path_type(Path::new("/foo/bar")), PathType::Absolute);
        assert_eq!(path_type(Path::new("foo/bar")), PathType::Relative);
        assert_eq!(path_type(Path::new("./x")), PathType::Relative);
    }

    #[cfg(windows)]
    #[test]
    fn path_type_windows_classification() {
        assert_eq!(path_type(Path::new(r"C:\foo")), PathType::Absolute);
        assert_eq!(path_type(Path::new(r"C:foo")), PathType::VolumeRelative);
        assert_eq!(path_type(Path::new(r"\foo")), PathType::VolumeRelative);
        assert_eq!(path_type(Path::new("foo")), PathType::Relative);
    }

    // absolute path directness ----------------------------------------------

    #[test]
    fn absolute_path_ignores_include_paths() {
        let tmp = TempDir::new("abs");
        let target = tmp.write("hdr.hrl", b"c");
        let req = dummy_request(IncludeKind::Include, target.to_str().unwrap());
        let opened =
            open_include_with_env(&req, &[], |_| None, |_| None).expect("resolve absolute");
        assert_eq!(opened.path, target);
    }

    #[test]
    fn absolute_path_missing_returns_not_found() {
        let tmp = TempDir::new("abs-missing");
        let missing = tmp.path.join("missing.hrl");
        let req = dummy_request(IncludeKind::Include, missing.to_str().unwrap());
        let e = open_include_with_env(&req, &[], |_| None, |_| None).unwrap_err();
        assert!(matches!(e, OpenIncludeError::NotFound));
    }

    // relative search -------------------------------------------------------

    #[test]
    fn relative_search_order_matches_include_paths() {
        let tmp1 = TempDir::new("rel1");
        let tmp2 = TempDir::new("rel2");
        let target = tmp2.write("hdr.hrl", b"c");
        let req = dummy_request(IncludeKind::Include, "hdr.hrl");
        let opened = open_include_with_env(
            &req,
            &[tmp1.path.clone(), tmp2.path.clone()],
            |_| None,
            |_| None,
        )
        .expect("resolve relative");
        assert_eq!(opened.path, target);
    }

    #[test]
    fn relative_first_match_wins() {
        let tmp1 = TempDir::new("rel-first");
        let tmp2 = TempDir::new("rel-second");
        let target = tmp1.write("hdr.hrl", b"c");
        let _shadow = tmp2.write("hdr.hrl", b"c");
        let req = dummy_request(IncludeKind::Include, "hdr.hrl");
        let opened = open_include_with_env(
            &req,
            &[tmp1.path.clone(), tmp2.path.clone()],
            |_| None,
            |_| None,
        )
        .expect("resolve");
        assert_eq!(opened.path, target);
    }

    #[test]
    fn not_found_when_no_candidate_matches() {
        let tmp = TempDir::new("nf");
        let req = dummy_request(IncludeKind::Include, "missing.hrl");
        let e = open_include_with_env(&req, std::slice::from_ref(&tmp.path), |_| None, |_| None)
            .unwrap_err();
        assert!(matches!(e, OpenIncludeError::NotFound));
    }

    #[test]
    fn not_a_directory_advances_to_next_candidate() {
        let tmp1 = TempDir::new("nad-file");
        tmp1.write("sub", b"i-am-a-file");
        let tmp2 = TempDir::new("nad-dir");
        let target = tmp2.write("sub/hdr.hrl", b"c");
        let req = dummy_request(IncludeKind::Include, "sub/hdr.hrl");
        let opened = open_include_with_env(
            &req,
            &[tmp1.path.clone(), tmp2.path.clone()],
            |_| None,
            |_| None,
        )
        .expect("resolve");
        assert_eq!(opened.path, target);
    }

    // include vs include_lib ------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn include_other_io_error_bubbles_up() {
        let tmp = TempDir::new("io-err");
        let link = tmp.path.join("hdr.hrl");
        std::os::unix::fs::symlink(&link, &link).expect("create symlink loop");
        let req = dummy_request(IncludeKind::Include, "hdr.hrl");
        let e = open_include_with_env(&req, std::slice::from_ref(&tmp.path), |_| None, |_| None)
            .unwrap_err();
        assert!(matches!(e, OpenIncludeError::Io(_)), "got {e:?}");
    }

    #[test]
    fn include_lib_prefers_normal_search() {
        let tmp = TempDir::new("lib-normal");
        let target = tmp.write("myapp/include/hdr.hrl", b"c");
        let req = dummy_request(IncludeKind::IncludeLib, "myapp/include/hdr.hrl");
        let app_lookup = |_: &str| Some(PathBuf::from("/nonexistent"));
        let opened =
            open_include_with_env(&req, std::slice::from_ref(&tmp.path), app_lookup, |_| None)
                .expect("normal search wins");
        assert_eq!(opened.path, target);
    }

    #[test]
    fn include_lib_falls_back_to_app_lookup() {
        let app_root = TempDir::new("lib-app");
        let target = app_root.write("include/hdr.hrl", b"c");
        let req = dummy_request(IncludeKind::IncludeLib, "myapp/include/hdr.hrl");
        let app_path = app_root.path.clone();
        let app_lookup = |name: &str| (name == "myapp").then(|| app_path.clone());
        let opened = open_include_with_env(&req, &[], app_lookup, |_| None).expect("app fallback");
        assert_eq!(opened.path, target);
    }

    #[cfg(unix)]
    #[test]
    fn include_lib_falls_back_on_io_error_from_normal_search() {
        // Symlink loop in the include path causes an I/O error
        // (ELOOP) during the ordinary search. For `include_lib`,
        // that must NOT surface as `Io(...)`; the resolver falls
        // through to the application lookup.
        let bad = TempDir::new("lib-io-bad");
        let link = bad.path.join("myapp");
        std::os::unix::fs::symlink(&link, &link).expect("symlink loop");
        let app_root = TempDir::new("lib-io-app");
        let target = app_root.write("include/hdr.hrl", b"c");
        let req = dummy_request(IncludeKind::IncludeLib, "myapp/include/hdr.hrl");
        let app_path = app_root.path.clone();
        let app_lookup = |name: &str| (name == "myapp").then(|| app_path.clone());
        let opened =
            open_include_with_env(&req, std::slice::from_ref(&bad.path), app_lookup, |_| None)
                .expect("fallback resolves despite I/O error");
        assert_eq!(opened.path, target);
    }

    #[test]
    fn include_lib_app_not_found_returns_app_not_found() {
        let req = dummy_request(IncludeKind::IncludeLib, "myapp/include/hdr.hrl");
        let e = open_include_with_env(&req, &[], |_| None, |_| None).unwrap_err();
        match e {
            OpenIncludeError::AppNotFound { app } => assert_eq!(app, "myapp"),
            other => panic!("expected AppNotFound, got {other:?}"),
        }
    }

    #[test]
    fn include_lib_app_file_not_found() {
        let app_root = TempDir::new("lib-nofile");
        let req = dummy_request(IncludeKind::IncludeLib, "myapp/include/missing.hrl");
        let app_path = app_root.path.clone();
        let app_lookup = |name: &str| (name == "myapp").then(|| app_path.clone());
        let e = open_include_with_env(&req, &[], app_lookup, |_| None).unwrap_err();
        match e {
            OpenIncludeError::AppFileNotFound { app, tail } => {
                assert_eq!(app, "myapp");
                assert_eq!(tail, PathBuf::from("include/missing.hrl"));
            }
            other => panic!("expected AppFileNotFound, got {other:?}"),
        }
    }

    // Non-canonicalization + file/path identity -----------------------------

    #[test]
    fn does_not_canonicalize_dotdot() {
        let tmp = TempDir::new("nocanon");
        tmp.write("sub/hdr.hrl", b"c");
        let req = dummy_request(IncludeKind::Include, "sub/../sub/hdr.hrl");
        let opened =
            open_include_with_env(&req, std::slice::from_ref(&tmp.path), |_| None, |_| None)
                .expect("resolve");
        let expected = tmp.path.join("sub/../sub/hdr.hrl");
        assert_eq!(opened.path, expected);
    }

    #[cfg(unix)]
    #[test]
    fn opened_file_matches_inode_of_resolved_path() {
        use std::os::unix::fs::MetadataExt;
        let tmp = TempDir::new("inode");
        let _target = tmp.write("hdr.hrl", b"c");
        let req = dummy_request(IncludeKind::Include, "hdr.hrl");
        let opened =
            open_include_with_env(&req, std::slice::from_ref(&tmp.path), |_| None, |_| None)
                .expect("resolve");
        let m_path = std::fs::metadata(&opened.path).expect("stat path");
        let m_file = opened.file.metadata().expect("stat file");
        assert_eq!(m_path.dev(), m_file.dev());
        assert_eq!(m_path.ino(), m_file.ino());
    }
}
