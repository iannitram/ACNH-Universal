//! `acnh-universal`: the launcher without its window. The Steam shortcut in Game Mode runs
//! `acnh-universal play` (SPEC.md M2, M6); the other commands do what the app's pages do.
//!
//! ```text
//! acnh-universal check  --game <folder>
//! acnh-universal build  --game <folder> [--target <os>] [--out <folder>]
//! acnh-universal play   [--profile <id>]
//! acnh-universal steam-add
//! acnh-universal hub    serve|pair|pull
//! acnh-universal fleet
//! ```
//!
//! Skeleton: every command says it is not built yet and exits 1.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("help");
    match command {
        "check" | "build" | "play" | "steam-add" | "hub" | "fleet" => {
            eprintln!("acnh-universal {command}: not built yet");
            std::process::exit(1);
        }
        _ => {
            println!("usage: acnh-universal check|build|play|steam-add|hub|fleet");
        }
    }
}
