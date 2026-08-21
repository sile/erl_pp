//! Integration tests for the public `erl_pp::open_include` resolver.
//!
//! `erl_pp::IncludeDirective` values are obtained through the real
//! `erl_pp::Preprocessor` event loop rather than constructed by hand, so
//! the integration test exercises the actual caller path.
//!
//! Environment-variable expansion goes through `std::env::var`
//! inside `erl_pp::open_include`. Because `std::env::set_var` is `unsafe`
//! in Rust 2024, `$VAR` behavior is covered by unit tests via the
//! internal `open_include_with_env` closure form; this integration
//! surface only walks paths that do not touch the process
//! environment.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let mut base = std::env::temp_dir();
        base.push(format!("erl_pp_open_include_{label}_{pid}_{n}"));
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

fn scan_all(text: &str) -> Vec<erl_tokenize::Token> {
    let mut tokens = Vec::new();
    let mut position = erl_tokenize::Position::new();
    while let Some(t) = erl_tokenize::scan_token(text, position).expect("tokenize test input") {
        position = t.end();
        tokens.push(t);
    }
    tokens
}

fn take_include_directive(source_text: &str) -> erl_pp::IncludeDirective {
    let tokens = scan_all(source_text);
    let source = erl_pp::Source::new("caller.erl", source_text.to_string(), tokens);
    let mut pp = erl_pp::Preprocessor::new([source]);
    loop {
        match pp.step().expect("no protocol error") {
            erl_pp::Event::AwaitingInclude(req) => return req,
            erl_pp::Event::Token(_) => continue,
            other => panic!("unexpected event before AwaitingInclude: {other:?}"),
        }
    }
}

#[test]
fn absolute_path_opens_directly() {
    let tmp = TempDir::new("abs");
    let target = tmp.write("hdr.hrl", b"c");
    let path_str = target.to_str().expect("utf8 path");
    let include = take_include_directive(&format!(r#"-include("{path_str}")."#));
    assert_eq!(include.kind, erl_pp::IncludeKind::Include);
    let path = erl_pp::open_include(&include, &[], &[]).expect("resolve");
    assert_eq!(path, target);
}

#[test]
fn relative_path_walks_include_paths_in_order() {
    let tmp1 = TempDir::new("rel1");
    let tmp2 = TempDir::new("rel2");
    let target = tmp2.write("hdr.hrl", b"c");
    let include = take_include_directive(r#"-include("hdr.hrl")."#);
    let include_paths = vec![tmp1.path.clone(), tmp2.path.clone()];
    let path = erl_pp::open_include(&include, &include_paths, &[]).expect("resolve");
    assert_eq!(path, target);
}

#[test]
fn missing_relative_include_returns_not_found() {
    let tmp = TempDir::new("nf");
    let include = take_include_directive(r#"-include("missing.hrl")."#);
    let err = erl_pp::open_include(&include, std::slice::from_ref(&tmp.path), &[])
        .expect_err("missing include should not resolve");
    assert!(
        matches!(err, erl_pp::OpenIncludeError::NotFound),
        "got {err:?}"
    );
}

#[test]
fn include_lib_falls_back_via_erl_libs() {
    let lib = TempDir::new("lib-app");
    let target = lib.write("myapp/include/hdr.hrl", b"c");
    let include = take_include_directive(r#"-include_lib("myapp/include/hdr.hrl")."#);
    assert_eq!(include.kind, erl_pp::IncludeKind::IncludeLib);
    let path =
        erl_pp::open_include(&include, &[], std::slice::from_ref(&lib.path)).expect("resolve");
    assert_eq!(path, target);
}

#[test]
fn include_lib_picks_highest_version_from_erl_libs() {
    let lib = TempDir::new("lib-highver");
    lib.write("myapp-1.0/include/hdr.hrl", b"old");
    lib.write("myapp-1.10/include/hdr.hrl", b"middle-natural");
    let target = lib.write("myapp-2.0/include/hdr.hrl", b"newest");
    let include = take_include_directive(r#"-include_lib("myapp/include/hdr.hrl")."#);
    let path =
        erl_pp::open_include(&include, &[], std::slice::from_ref(&lib.path)).expect("resolve");
    assert_eq!(path, target);
}

#[test]
fn include_lib_unknown_app_returns_app_not_found() {
    let include = take_include_directive(r#"-include_lib("unknown_app/include/hdr.hrl")."#);
    let err = erl_pp::open_include(&include, &[], &[]).expect_err("unknown app should not resolve");
    match err {
        erl_pp::OpenIncludeError::AppNotFound { app } => assert_eq!(app, "unknown_app"),
        other => panic!("expected AppNotFound, got {other:?}"),
    }
}
