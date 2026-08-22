use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> noargs::Result<ExitCode> {
    let mut args = noargs::raw_args();
    args.metadata_mut().app_name = env!("CARGO_PKG_NAME");
    args.metadata_mut().app_description =
        "Preprocess all .erl/.hrl files under an OTP source root directory";
    noargs::HELP_FLAG.take_help(&mut args);

    let root: PathBuf = noargs::arg("<OTP_ROOT>")
        .doc("OTP source root directory (e.g. a checkout of erlang/otp)")
        .take(&mut args)
        .then(|a| a.value().parse())?;
    if let Some(help) = args.finish()? {
        print!("{help}");
        return Ok(ExitCode::SUCCESS);
    }

    let mut files = Vec::new();
    collect(&root, &mut files);
    files.retain(|p| is_target(p) && !is_skipped(p));
    files.sort();

    let erl_libs = vec![root.join("lib")];
    let global_include_dirs = collect_app_include_dirs(&root);
    let mut ok_files = 0usize;
    let mut err_files = 0usize;
    let mut event_total = 0usize;
    let mut token_total = 0usize;
    let mut warning_total = 0usize;
    let mut error_total = 0usize;
    let start = std::time::Instant::now();

    for path in &files {
        let display = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        let include_paths = build_include_paths(path, &root, &global_include_dirs);
        match run_one(path, &display, &include_paths, &erl_libs) {
            FileOutcome::Ok {
                events,
                tokens,
                warnings,
                errors,
            } => {
                ok_files += 1;
                event_total += events;
                token_total += tokens;
                warning_total += warnings;
                error_total += errors;
            }
            FileOutcome::Failed { reason } => {
                err_files += 1;
                eprintln!("{display}: {reason}");
            }
        }
    }

    println!(
        "FILES: {}\nOK FILES: {}\nFILES WITH ERRORS: {}\nTOTAL EVENTS: {}\nTOTAL TOKENS: {}\nTOTAL DIAGNOSTICS: {} warnings / {} errors\nELAPSED: {:?}",
        files.len(),
        ok_files,
        err_files,
        event_total,
        token_total,
        warning_total,
        error_total,
        start.elapsed(),
    );
    let _ = std::io::stdout().flush();
    Ok(ExitCode::from(u8::from(err_files > 0)))
}

enum FileOutcome {
    Ok {
        events: usize,
        tokens: usize,
        warnings: usize,
        errors: usize,
    },
    Failed {
        reason: String,
    },
}

fn run_one(
    path: &Path,
    display: &str,
    include_paths: &[PathBuf],
    erl_libs: &[PathBuf],
) -> FileOutcome {
    let src = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            return FileOutcome::Failed {
                reason: format!("read: {e}"),
            };
        }
    };
    let source = match erl_pp::Source::from_text(display, src) {
        Ok(source) => source,
        Err(e) => {
            return FileOutcome::Failed {
                reason: format!("scan_token: {e}"),
            };
        }
    };
    let mut pp = erl_pp::Preprocessor::new([source]);

    let mut events = 0usize;
    let mut token_count = 0usize;
    let mut warnings = 0usize;
    let mut errors = 0usize;
    let mut file_failed: Option<String> = None;

    loop {
        let event = pp
            .step()
            .expect("Preprocessor::step must not return ProtocolError");
        events += 1;
        match event {
            erl_pp::Event::Token(_) => token_count += 1,
            erl_pp::Event::MacroDefined(_) | erl_pp::Event::MacroUndefined(_) => {}
            erl_pp::Event::AwaitingInclude(req) => {
                let (included_source, load_failure) =
                    resolve_include(&req, include_paths, erl_libs);
                if let Some(reason) = load_failure
                    && file_failed.is_none()
                {
                    file_failed = Some(reason);
                }
                pp.resume_include(included_source)
                    .expect("resume_include after AwaitingInclude");
            }
            erl_pp::Event::AwaitingConditional(req) => {
                let branch = match req {
                    erl_pp::Conditional::Ifdef(d) | erl_pp::Conditional::Ifndef(d) => d.recommended,
                    erl_pp::Conditional::If(_) | erl_pp::Conditional::Elif(_) => {
                        erl_pp::Branch::Else
                    }
                };
                pp.resume_conditional(branch)
                    .expect("resume_conditional after AwaitingConditional");
            }
            erl_pp::Event::AwaitingMacroExpansion(_) => {
                pp.resume_macro_expansion(empty_source("<caller-driven>"))
                    .expect("resume_macro_expansion after AwaitingMacroExpansion");
            }
            erl_pp::Event::BranchBoundary(_) => {}
            erl_pp::Event::Diagnostic(d) => match d.severity {
                erl_pp::Severity::Error => errors += 1,
                erl_pp::Severity::Warning => warnings += 1,
            },
            erl_pp::Event::PreprocessError(e) => {
                if file_failed.is_none() {
                    file_failed = Some(format!("preprocess: {e:?}"));
                }
            }
            erl_pp::Event::Complete => break,
        }
    }

    match file_failed {
        Some(reason) => FileOutcome::Failed { reason },
        None => FileOutcome::Ok {
            events,
            tokens: token_count,
            warnings,
            errors,
        },
    }
}

fn resolve_include(
    include: &erl_pp::IncludeDirective,
    include_paths: &[PathBuf],
    erl_libs: &[PathBuf],
) -> (erl_pp::Source, Option<String>) {
    let raw_path = include.path.as_str();
    match erl_pp::open_include(include, include_paths, erl_libs) {
        Ok(path) => match fs::read_to_string(&path) {
            Ok(text) => {
                match erl_pp::Source::from_text(path.to_string_lossy().into_owned(), text) {
                    Ok(source) => (source, None),
                    Err(e) => (
                        empty_source(raw_path),
                        Some(format!(
                            "include scan_token failed for {}: {e}",
                            path.display()
                        )),
                    ),
                }
            }
            Err(e) => (
                empty_source(raw_path),
                Some(format!("include read failed for {}: {e}", path.display())),
            ),
        },
        Err(e) => (
            empty_source(raw_path),
            Some(format!("open_include({raw_path}): {e}")),
        ),
    }
}

fn empty_source(name: &str) -> erl_pp::Source {
    erl_pp::Source::new(name.to_string(), String::new(), Vec::new())
}

fn build_include_paths(target: &Path, root: &Path, globals: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(dir) = target.parent() {
        paths.push(dir.to_path_buf());
        if let Some(parent) = dir.parent() {
            let include = parent.join("include");
            if include != *dir {
                paths.push(include);
            }
        }
    }
    // If the target sits inside an application's `src/` tree, add
    // every subdirectory of that `src/` so bare `-include(...)` can
    // reach headers kept next to the caller's cousins (e.g.
    // `lib/inets/src/http_server/mod_alias.erl` reaching for
    // `inets_internal.hrl` under `lib/inets/src/inets_app/`).
    if let Some(app_src) = find_app_src(target) {
        for sub in walk_dirs(&app_src) {
            if !paths.iter().any(|p| p == &sub) {
                paths.push(sub);
            }
        }
    }
    // `erts/preloaded/src/**` reaches for `lib/kernel/src/` headers
    // (`inet_boot.hrl` / `file_int.hrl` / `inet_int.hrl`) that are
    // application-private to kernel. Add `lib/kernel/src/` when the
    // target is a preloaded module so those bare `-include(...)`
    // resolve without a separate skip list entry.
    if target.to_string_lossy().contains("/erts/preloaded/src/") {
        let kernel_src = root.join("lib").join("kernel").join("src");
        if kernel_src.is_dir() && !paths.iter().any(|p| p == &kernel_src) {
            paths.push(kernel_src);
        }
    }
    for g in globals {
        if !paths.iter().any(|p| p == g) {
            paths.push(g.clone());
        }
    }
    paths
}

fn find_app_src(target: &Path) -> Option<PathBuf> {
    let mut cur = target.parent()?;
    loop {
        if cur.file_name().and_then(|n| n.to_str()) == Some("src") {
            return Some(cur.to_path_buf());
        }
        cur = cur.parent()?;
    }
}

fn walk_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if root.is_dir() {
        out.push(root.to_path_buf());
        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    out.extend(walk_dirs(&p));
                }
            }
        }
    }
    out
}

/// Return `otp/lib/<app>/include/` for every application. Ordinary
/// `-include("foo.hrl")` outside the current app's tree cannot be
/// resolved without an `-I` list from the OTP build; giving the
/// resolver every `include/` directory approximates what `erlc` sees
/// when the build system has assembled the full `-I` set.
fn collect_app_include_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let lib = root.join("lib");
    if let Ok(entries) = fs::read_dir(&lib) {
        for entry in entries.flatten() {
            let include = entry.path().join("include");
            if include.is_dir() {
                out.push(include);
            }
        }
    }
    out.sort();
    out
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                collect(&p, out);
            } else if let Some(ext) = p.extension().and_then(|e| e.to_str())
                && (ext == "erl" || ext == "hrl")
            {
                out.push(p);
            }
        }
    }
}

/// Target set: OTP application source (`lib/<app>/src/**/*.erl` and
/// public headers `lib/<app>/include/**/*.hrl`) plus preloaded ERTS
/// modules (`erts/preloaded/src/**`).
///
/// Everything under `test/`, `examples/`, `api_gen/`, `doc/`, `c_src/`
/// etc. is deliberately outside this set: those trees rely on
/// build-time `-I` flags and generated headers that a plain source
/// checkout does not carry.
fn is_target(path: &Path) -> bool {
    let s = path.to_string_lossy();
    let in_lib_src_or_include = s.contains("/lib/")
        && (s.contains("/src/") || s.contains("/include/"))
        && !s.contains("/test/");
    let in_erts_preloaded = s.contains("/erts/preloaded/src/");
    in_lib_src_or_include || in_erts_preloaded
}

fn is_skipped(path: &Path) -> bool {
    let s = path.to_string_lossy();
    // Applications whose source tree requires build-generated
    // headers before it can be preprocessed:
    // - wx: the C++-side code generator produces `wxe.hrl`
    // - snmp: the MIB compiler produces `SNMPv2-TM.hrl`,
    //   `STANDARD-MIB.hrl`, `SNMP-USER-BASED-SM-MIB.hrl`, …
    // - public_key / eldap: asn1 produces
    //   `AlgorithmInformation-2009.hrl`, `PKCS-FRAME.hrl`,
    //   `ELDAPv3.hrl`
    if s.contains("/lib/wx/")
        || s.contains("/lib/snmp/")
        || s.contains("/lib/public_key/src/")
        || s.contains("/lib/eldap/src/")
    {
        return true;
    }
    // Individual files inside the target set that reach for a header
    // produced by the OTP build, a cross-application bare
    // `-include(...)`, or use `-if` / `-elif` (not supported by the
    // preprocessor and left as raw tokens, unbalancing the
    // surrounding `-endif` / `-else`).
    const INDIVIDUAL_SKIPS: &[&str] = &[
        // -if / -elif (unsupported by the preprocessor).
        "/lib/compiler/src/beam_ssa_alias.erl",
        "/lib/compiler/src/beam_ssa_alias_debug.hrl",
        "/lib/compiler/src/beam_ssa_ss.erl",
        "/lib/stdlib/src/graph.erl",
        "/lib/stdlib/src/peer.erl",
        // Build-generated `beam_opcodes.hrl`.
        "/lib/compiler/src/beam_asm.erl",
        "/lib/compiler/src/beam_disasm.erl",
        // Build-generated `inet_dns_record_adts.hrl`.
        "/lib/kernel/src/inet_dns.erl",
        // `snmp_types.hrl` — depends on snmp (already skipped above).
        "/lib/common_test/src/ct_snmp.erl",
    ];
    if INDIVIDUAL_SKIPS.iter().any(|suffix| s.ends_with(suffix)) {
        return true;
    }
    false
}
