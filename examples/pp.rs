use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> noargs::Result<ExitCode> {
    let mut args = noargs::raw_args();
    args.metadata_mut().app_name = "pp";
    args.metadata_mut().app_description =
        "Drive erl_pp over a file or stdin and print lexical tokens";
    noargs::HELP_FLAG.take_help(&mut args);

    let include_opt = noargs::opt("include")
        .short('I')
        .ty("DIR")
        .doc("Directory searched by open_include (repeatable)");
    let mut include_paths = Vec::<PathBuf>::new();
    while let Some(dir) = include_opt
        .take(&mut args)
        .present_and_then(|o| o.value().parse())?
    {
        include_paths.push(dir);
    }

    let file: Option<PathBuf> = noargs::arg("[FILE]")
        .doc("Erlang source file (default: stdin)")
        .take(&mut args)
        .present_and_then(|a| a.value().parse())?;
    if let Some(help) = args.finish()? {
        print!("{help}");
        return Ok(ExitCode::SUCCESS);
    }

    let (display, text) = match &file {
        Some(path) => match fs::read_to_string(path) {
            Ok(text) => (path.to_string_lossy().into_owned(), text),
            Err(e) => {
                eprintln!("read {}: {e}", path.display());
                return Ok(ExitCode::FAILURE);
            }
        },
        None => {
            let mut text = String::new();
            if let Err(e) = io::stdin().read_to_string(&mut text) {
                eprintln!("read stdin: {e}");
                return Ok(ExitCode::FAILURE);
            }
            ("stdin".to_owned(), text)
        }
    };

    if let Some(path) = &file
        && let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        include_paths.insert(0, dir.to_path_buf());
    }

    let tokens = match scan_source(&text) {
        Ok(tokens) => tokens,
        Err(e) => {
            eprintln!("scan_token: {e}");
            return Ok(ExitCode::FAILURE);
        }
    };
    let source = erl_pp::Source::new(display, text, tokens);
    let mut pp = erl_pp::Preprocessor::new([source]);

    loop {
        match pp.step() {
            Ok(erl_pp::Event::Token(t)) => print!("{} ", t.text()),
            Ok(erl_pp::Event::MacroDefined(_) | erl_pp::Event::MacroUndefined(_)) => {}
            Ok(erl_pp::Event::AwaitingInclude(inc)) => {
                let included = resolve_include(&inc, &include_paths);
                if let Err(e) = pp.resume_include(included) {
                    eprintln!("resume_include: {e}");
                    return Ok(ExitCode::FAILURE);
                }
            }
            Ok(erl_pp::Event::AwaitingConditional(cond)) => {
                let branch = match cond {
                    erl_pp::Conditional::Ifdef(d) | erl_pp::Conditional::Ifndef(d) => d.recommended,
                    // Caller evaluates -if / -elif. This CLI skips the branch.
                    erl_pp::Conditional::If(_) | erl_pp::Conditional::Elif(_) => {
                        erl_pp::Branch::Else
                    }
                };
                if let Err(e) = pp.resume_conditional(branch) {
                    eprintln!("resume_conditional: {e}");
                    return Ok(ExitCode::FAILURE);
                }
            }
            Ok(erl_pp::Event::AwaitingMacroExpansion(_)) => {
                // Empty Source skips. Compiler-like tools would error instead.
                if let Err(e) = pp.resume_macro_expansion(empty_source("<skipped-macro>")) {
                    eprintln!("resume_macro_expansion: {e}");
                    return Ok(ExitCode::FAILURE);
                }
            }
            Ok(erl_pp::Event::BranchBoundary(_)) => {}
            Ok(erl_pp::Event::Diagnostic(d)) => {
                // Record and continue. Fatal: break and drop `pp`.
                eprintln!("diagnostic {:?}: {:?}", d.severity, d.directive_span);
            }
            Ok(erl_pp::Event::PreprocessError(e)) => {
                // Record and continue. Fatal: break and drop `pp`.
                eprintln!("preprocess: {e:?}");
            }
            Ok(erl_pp::Event::Complete) => break,
            Err(e) => {
                eprintln!("protocol: {e}");
                return Ok(ExitCode::FAILURE);
            }
        }
    }
    println!();
    Ok(ExitCode::SUCCESS)
}

fn scan_source(text: &str) -> Result<Vec<erl_tokenize::Token>, String> {
    let mut tokens = Vec::new();
    let mut pos = erl_tokenize::Position::new();
    loop {
        match erl_tokenize::scan_token(text, pos) {
            Ok(Some(t)) => {
                pos = t.end();
                tokens.push(t);
            }
            Ok(None) => return Ok(tokens),
            Err(e) => return Err(format!("{e}")),
        }
    }
}

fn resolve_include(
    include: &erl_pp::IncludeDirective,
    include_paths: &[PathBuf],
) -> erl_pp::Source {
    let raw_path = include.path.as_str();
    match erl_pp::open_include(include, include_paths, &[] as &[PathBuf]) {
        Ok(path) => match load_source(&path) {
            Ok(source) => source,
            Err(e) => {
                eprintln!("include {}: {e}", path.display());
                empty_source(raw_path)
            }
        },
        Err(e) => {
            eprintln!("open_include({raw_path}): {e}");
            empty_source(raw_path)
        }
    }
}

fn load_source(path: &Path) -> Result<erl_pp::Source, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    let tokens = scan_source(&text)?;
    Ok(erl_pp::Source::new(
        path.to_string_lossy().into_owned(),
        text,
        tokens,
    ))
}

fn empty_source(name: &str) -> erl_pp::Source {
    erl_pp::Source::new(name.to_owned(), String::new(), Vec::new())
}
