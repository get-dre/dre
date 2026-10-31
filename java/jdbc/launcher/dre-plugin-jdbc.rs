use std::{env, path::PathBuf, process::Command};

fn main() {
    let Ok(executable) = env::current_exe() else {
        eprintln!("can't find the JDBC launcher executable");
        std::process::exit(1);
    };
    let Some(directory) = executable.parent() else {
        eprintln!("the JDBC launcher has no parent directory");
        std::process::exit(1);
    };
    let status = Command::new(directory.join("runtime").join("bin").join("java.exe"))
        .arg("--add-opens=java.base/java.nio=ALL-UNNAMED")
        .arg("-jar")
        .arg(directory.join("dre-plugin-jdbc.jar"))
        .args(env::args_os().skip(1))
        .status();
    match status {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(error) => {
            eprintln!("can't start the bundled JDBC runtime: {error}");
            std::process::exit(1);
        }
    }
}
