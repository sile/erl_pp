use erl_pp::{MacroDef, Preprocessor};
use erl_tokenize::tokens::AtomToken;
use erl_tokenize::{Lexer, Position, PositionRange};
use std::env;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn main() -> noargs::Result<()> {
    let mut args = noargs::raw_args();
    args.metadata_mut().app_name = env!("CARGO_PKG_NAME");
    args.metadata_mut().app_description = env!("CARGO_PKG_DESCRIPTION");

    if noargs::VERSION_FLAG.take(&mut args).is_present() {
        println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    noargs::HELP_FLAG.take_help(&mut args);

    let silent = noargs::flag("silent")
        .doc("Suppress per-token output")
        .take(&mut args)
        .is_present();
    let current_dir: Option<PathBuf> = noargs::opt("current-dir")
        .ty("DIR")
        .doc("Change the current working directory before preprocessing")
        .take(&mut args)
        .present_and_then(|o| o.value().parse())?;
    let mut libs: Vec<PathBuf> = Vec::new();
    while let Some(lib) = noargs::opt("libs")
        .ty("DIR")
        .doc("Adds a code path entry used to resolve `include_lib` directives")
        .take(&mut args)
        .present_and_then(|o| o.value().parse::<PathBuf>())?
    {
        libs.push(lib);
    }
    let source_file: PathBuf = noargs::arg("<SOURCE_FILE>")
        .doc("Erlang source file to preprocess")
        .example("foo.erl")
        .take(&mut args)
        .then(|a| a.value().parse())?;

    if let Some(help) = args.finish()? {
        print!("{help}");
        return Ok(());
    }

    if let Some(dir) = current_dir {
        env::set_current_dir(dir)?;
    }

    let src_file: &Path = source_file.as_path();
    let mut src = String::new();
    let mut file = File::open(src_file).expect("Cannot open file");
    file.read_to_string(&mut src).expect("Cannot read file");

    let start_time = Instant::now();
    let mut count = 0;

    let mut lexer = Lexer::new(&src);
    lexer.set_filepath(src_file.file_name().unwrap());

    let mut preprocessor = Preprocessor::new(lexer);
    for dir in libs {
        preprocessor.code_paths_mut().push_back(dir);
    }
    preprocessor.macros_mut().insert(
        "MODULE".to_string(),
        MacroDef::Dynamic(vec![
            AtomToken::from_value(
                src_file.file_stem().unwrap().to_str().unwrap(),
                Position::new(),
            )
            .into(),
        ]),
    );

    for result in preprocessor {
        let token = result?;
        if !silent {
            println!("[{:?}] {:?}", token.start_position(), token.text());
        }
        count += 1;
    }
    println!("TOKEN COUNT: {count}");
    println!("ELAPSED: {:?} seconds", start_time.elapsed().as_secs_f64());
    Ok(())
}
