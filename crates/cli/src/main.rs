//! `acnh-universal`: the launcher without its window. The commands are in `acnh_cli`.

#![forbid(unsafe_code)]

fn main() {
    std::process::exit(acnh_cli::run_process(std::env::args().skip(1).collect()));
}
