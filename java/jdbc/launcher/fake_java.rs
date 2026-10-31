use std::{env, fs::File, io::Write, path::PathBuf};

fn main() {
    let output = env::var_os("DRE_FAKE_ARGS").expect("DRE_FAKE_ARGS");
    let mut file = File::create(PathBuf::from(output)).expect("create argument log");
    for argument in env::args_os().skip(1) {
        writeln!(file, "{}", argument.to_string_lossy()).expect("write argument log");
    }
}
