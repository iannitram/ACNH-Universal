//! Build errors with the player's private details taken out.

use super::*;

/// `Error::Build` for the failed `log` of a script run for `stage`: the stage is the one the
/// log names last (`== <stage>`, when upstream's script failed in a stage we were not waiting
/// on), else `stage`; the text is its last 20 lines, each through `redact`.
pub(super) fn build_error(stage: Stage, log: &[String], home: &Path) -> Error {
    let stage = log
        .iter()
        .rev()
        .find_map(|line| Stage::from_line(line))
        .unwrap_or(stage);
    let tail: Vec<String> = log[log.len().saturating_sub(20)..]
        .iter()
        .map(|line| redact(line, home))
        .collect();
    Error::Build(format!("stage {}: {}", stage.key(), tail.join("\n")))
}

/// A log line made safe to show or report: the home folder (`home`) becomes `~`, and anything
/// that looks like a key (a run of 32 or more hex digits that is not a build id the gate
/// knows) becomes `<hex>`. Game file names stay; their contents never reach a log.
pub fn redact(line: &str, home: &Path) -> String {
    let home = home.display().to_string();
    let line = if home.is_empty() {
        line.to_owned()
    } else {
        line.replace(&home, "~")
    };
    let mut redacted = String::with_capacity(line.len());
    let mut run_start = None;

    for (index, character) in line.char_indices() {
        if character.is_ascii_hexdigit() {
            run_start.get_or_insert(index);
        } else if let Some(start) = run_start.take() {
            redact_hex_run(&line[start..index], &mut redacted);
            redacted.push(character);
        } else {
            redacted.push(character);
        }
    }

    if let Some(start) = run_start {
        redact_hex_run(&line[start..], &mut redacted);
    }

    redacted
}

pub(super) fn redact_hex_run(run: &str, redacted: &mut String) {
    if run.len() >= 32 && crate::version::from_build_id(run).is_err() {
        redacted.push_str("<hex>");
    } else {
        redacted.push_str(run);
    }
}
