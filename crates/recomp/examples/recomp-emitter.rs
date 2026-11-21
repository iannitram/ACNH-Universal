//! Exercise the ported emitter independently of the Python Prepare driver.

use acnh_recomp::{
    analyse::{analyse, Analysis, Function},
    emit::{CacheOptions, Emitter, Error},
    load, Module,
};
use std::{
    collections::BTreeSet,
    error::Error as StdError,
    io::{self, BufRead, BufWriter, Write},
    path::Path,
};

fn hex(text: &str) -> Result<u64, Box<dyn StdError>> {
    let text = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .unwrap_or(text);
    Ok(u64::from_str_radix(text, 16)?)
}

fn instructions() -> Result<(), Box<dyn StdError>> {
    let module = Module::default();
    let analysis = Analysis::default();
    let emitter = Emitter::new(&module, &analysis, "sub")?;
    let mut stdout = BufWriter::new(io::stdout().lock());
    for line in io::stdin().lock().lines() {
        let line = line?;
        let fields: Vec<_> = line.split_whitespace().collect();
        let [address, word] = fields.as_slice() else {
            return Err("expected two hexadecimal fields: address and instruction word".into());
        };
        let address = hex(address)?;
        let word = u32::try_from(hex(word)?)?;
        let function = Function {
            start: address,
            end: address
                .checked_add(4)
                .ok_or("instruction address is too high")?,
            fde: false,
            name: String::new(),
        };
        match emitter.instruction(&function, address, word) {
            Ok(statement) => writeln!(stdout, "{statement}")?,
            Err(Error::Unsupported(_) | Error::Undecodable) => writeln!(stdout)?,
            Err(error) => return Err(error.into()),
        }
    }
    stdout.flush()?;
    Ok(())
}

fn functions(path: &Path, prefix: &str, hooks: Option<&Path>) -> Result<(), Box<dyn StdError>> {
    let module = load(path, Some(prefix))?;
    let analysis = analyse(&module, &mut |_| {}, &[])?;
    let mut emitter = Emitter::new(&module, &analysis, prefix)?;
    emitter.options.cache = CacheOptions::from_env();
    if let Some(path) = hooks {
        let text = std::fs::read_to_string(path)?;
        emitter.options.hooks = text
            .lines()
            .map(|line| line.split('#').next().unwrap_or_default().trim())
            .filter(|line| !line.is_empty())
            .map(hex)
            .collect::<Result<BTreeSet<_>, _>>()?;
    }
    emitter.options.trace_sp = std::env::var("NX_TRACE_SP")
        .unwrap_or_default()
        .split(',')
        .filter(|part| !part.is_empty())
        .map(hex)
        .collect::<Result<_, _>>()?;
    let mut stdout = BufWriter::new(io::stdout().lock());
    writeln!(stdout, "#include \"funcs.h\"\n")?;
    for (index, function) in analysis.functions.iter().enumerate() {
        if index != 0 {
            writeln!(stdout)?;
        }
        writeln!(stdout, "{}", emitter.function(function)?)?;
    }
    stdout.flush()?;
    Ok(())
}

fn main() -> Result<(), Box<dyn StdError>> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    match arguments.as_slice() {
        [command] if command == "instructions" => instructions(),
        [command, input, prefix] if command == "functions" => {
            functions(Path::new(input), prefix, None)
        }
        [command, input, prefix, hooks] if command == "functions" => {
            functions(Path::new(input), prefix, Some(Path::new(hooks)))
        }
        _ => Err(
            "usage: recomp-emitter instructions | functions <module.nso> <prefix> [hooks-file]"
                .into(),
        ),
    }
}
