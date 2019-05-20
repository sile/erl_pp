extern crate clap;
extern crate erl_pp;
extern crate erl_tokenize;
#[macro_use]
extern crate trackable;

use clap::{App, Arg};
use erl_pp::{MacroDef, Preprocessor};
use erl_tokenize::tokens::AtomToken;
use erl_tokenize::{Lexer, Position, PositionRange};
use std::env;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};
use trackable::error::{ErrorKindExt, Failed};

fn main() {
    let matches = App::new("pp")
        .arg(Arg::with_name("SOURCE_FILE").index(1).required(true))
        .arg(Arg::with_name("SILENT").long("silent"))
        .arg(
            Arg::with_name("CURRENT_DIR")
                .long("current-dir")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("ERL_LIBS")
                .long("libs")
                .takes_value(true)
                .multiple(true),
        )
        .get_matches();
    let src_file = Path::new(matches.value_of("SOURCE_FILE").unwrap());
    let silent = matches.is_present("SILENT");
    if let Some(dir) = matches.value_of("CURRENT_DIR") {
        track_try_unwrap!(env::set_current_dir(dir).map_err(|e| Failed.cause(e)));
    }

    let mut src = String::new();
    let mut file = File::open(&src_file).expect("Cannot open file");
    file.read_to_string(&mut src).expect("Cannot read file");

    let start_time = Instant::now();
    let mut count = 0;

    let mut lexer = Lexer::new(&src);
    lexer.set_filepath(src_file.file_name().unwrap());

    let mut preprocessor = Preprocessor::new(lexer);
    if let Some(libs) = matches.values_of("ERL_LIBS") {
        for dir in libs {
            preprocessor.code_paths_mut().push_back(dir.into());
        }
    }
    preprocessor.macros_mut().insert(
        "MODULE".to_string(),
        MacroDef::Dynamic(vec![AtomToken::from_value(
            src_file.file_stem().unwrap().to_str().unwrap(),
            Position::new(),
        )
        .into()]),
    );

    for result in preprocessor {
        let token = track_try_unwrap!(result);
        if !silent {
            println!("[{:?}] {:?}", token.start_position(), token.text());
        }
        count += 1;
    }
    println!("TOKEN COUNT: {}", count);
    println!(
        "ELAPSED: {:?} seconds",
        to_seconds(Instant::now() - start_time)
    );
}

fn to_seconds(duration: Duration) -> f64 {
    duration.as_secs() as f64 + f64::from(duration.subsec_nanos()) / 1_000_000_000.0
}
