//! Expose the shell's own ELF attachment point and drive it synchronously.

use std::{fs, path::Path};

use bpfman_shell_runtime::{Argument, CommandResponse, Value};
use object::{Object, ObjectSegment, ObjectSymbol, SymbolKind};

use crate::fixturemode::uprobe::{DOTTED_SYMBOL, SYMBOL, fire_once};

use super::process::Interrupt;

pub(super) fn invoke(
    head: &str,
    arguments: &[Argument],
    interrupt: &Interrupt,
) -> Result<CommandResponse, String> {
    if head == "uprobe-target" {
        if !arguments.is_empty() {
            return Err("uprobe-target takes no arguments".to_owned());
        }
        return target(false).map(CommandResponse::success);
    }
    let (subcommand, rest) = arguments
        .split_first()
        .ok_or("uprobe: subcommand required")?;
    let subcommand = super::argument(subcommand)?;
    match (subcommand.as_str(), rest) {
        ("target", []) => target(true).map(CommandResponse::success),
        ("fire", [count]) => {
            let count = super::argument(count)?
                .parse::<usize>()
                .map_err(|error| format!("uprobe fire: invalid count: {error}"))?;
            for _ in 0..count {
                interrupt.check()?;
                fire_once();
            }
            Ok(CommandResponse::success(Value::Absent))
        }
        ("target", _) => Err("uprobe target takes no arguments".to_owned()),
        ("fire", _) => Err("uprobe fire requires a count".to_owned()),
        _ => Err(format!("uprobe: unknown subcommand {subcommand:?}")),
    }
}

fn target(full: bool) -> Result<Value, String> {
    let path =
        std::env::current_exe().map_err(|error| format!("resolve shell executable: {error}"))?;
    let mut target = serde_json::json!({
        "path": path,
        "symbol": SYMBOL,
    });
    if full {
        let offset = symbol_offset(&path)?;
        target["pid"] = serde_json::json!(std::process::id());
        target["symbol_offset"] = serde_json::json!(offset);
        target["go_symbol"] = serde_json::json!(DOTTED_SYMBOL);
    }
    super::bpfman::project(target)
}

fn symbol_offset(path: &Path) -> Result<u64, String> {
    let bytes = fs::read(path).map_err(|error| format!("read ELF {}: {error}", path.display()))?;
    let file = object::File::parse(bytes.as_slice())
        .map_err(|error| format!("read ELF {}: {error}", path.display()))?;
    let symbol = file
        .symbols()
        .chain(file.dynamic_symbols())
        .find(|symbol| {
            symbol.kind() == SymbolKind::Text
                && symbol.is_definition()
                && symbol.name().is_ok_and(|name| name == SYMBOL)
        })
        .ok_or_else(|| format!("symbol {SYMBOL} missing from {}", path.display()))?;
    file.segments()
        .find_map(|segment| {
            let (start, size) = segment.file_range();
            let relative = symbol.address().checked_sub(segment.address())?;
            (relative < size)
                .then(|| start.checked_add(relative))
                .flatten()
        })
        .ok_or_else(|| format!("symbol {SYMBOL} has no file-backed load segment"))
}
