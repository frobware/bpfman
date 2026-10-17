use bpfman_model::ProgramType;

use super::{Global, Metadata, ProgramSpec, Symbol};

fn symbol(value: &str) -> Result<Symbol, String> {
    Symbol::try_from(value.trim()).map_err(|error| error.to_string())
}

pub(super) fn program(value: &str) -> Result<ProgramSpec, String> {
    let mut fields = value.trim().split(':');
    let kind = fields.next().unwrap_or_default().trim().parse::<ProgramType>()
        .map_err(|_| "unknown program type; use xdp, tc, tcx, tracepoint, kprobe, kretprobe, uprobe, uretprobe, fentry, fexit, or lsm".to_owned())?;
    let name = symbol(fields.next().ok_or("expected TYPE:NAME[:TARGET]")?)?;
    let target = fields.next();

    if fields.next().is_some() {
        return Err("expected at most TYPE:NAME:TARGET".into());
    }

    match kind {
        ProgramType::Fentry | ProgramType::Fexit | ProgramType::Lsm => {
            let target = symbol(target.ok_or("fentry/fexit/lsm require TYPE:NAME:TARGET")?)?;

            Ok(match kind {
                ProgramType::Fentry => ProgramSpec::Fentry { name, target },
                ProgramType::Fexit => ProgramSpec::Fexit { name, target },
                _ => ProgramSpec::Lsm { name, hook: target },
            })
        }
        _ => {
            if target.is_some() {
                return Err("a load-time target is only valid for fentry, fexit, or lsm".into());
            }

            Ok(match kind {
                ProgramType::Xdp => ProgramSpec::Xdp(name),
                ProgramType::Tc => ProgramSpec::Tc(name),
                ProgramType::Tcx => ProgramSpec::Tcx(name),
                ProgramType::Tracepoint => ProgramSpec::Tracepoint(name),
                ProgramType::Kprobe => ProgramSpec::Kprobe(name),
                ProgramType::Kretprobe => ProgramSpec::Kretprobe(name),
                ProgramType::Uprobe => ProgramSpec::Uprobe(name),
                ProgramType::Uretprobe => ProgramSpec::Uretprobe(name),

                // The enclosing match has already handled these variants.
                // No panic/unreachable fallback: construct valid targeted values above.
                ProgramType::Fentry | ProgramType::Fexit | ProgramType::Lsm => {
                    return Err("missing load-time target".into());
                }
            })
        }
    }
}

pub(super) fn metadata(value: &str) -> Result<Metadata, String> {
    let (key, value) = value.split_once('=').ok_or("expected KEY=VALUE")?;
    let key = key.trim();

    if key.is_empty() {
        return Err("metadata key must not be empty".into());
    }

    Ok(Metadata(key.into(), value.into()))
}

pub(super) fn global(value: &str) -> Result<Global, String> {
    let (name, hex) = value.split_once('=').ok_or("expected NAME=HEX")?;
    let name = name.trim();

    if name.is_empty() {
        return Err("global name must not be empty".into());
    }

    let hex = hex.trim();
    let hex = hex
        .strip_prefix("0x")
        .or_else(|| hex.strip_prefix("0X"))
        .unwrap_or(hex);

    if hex.len() % 2 != 0 {
        return Err("global data requires an even number of hex digits".into());
    }

    let data = hex
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = char::from(pair[0]).to_digit(16).ok_or("invalid hex byte")?;
            let low = char::from(pair[1]).to_digit(16).ok_or("invalid hex byte")?;

            Ok((high * 16 + low) as u8)
        })
        .collect::<Result<Vec<_>, &str>>()?;

    Ok(Global(name.into(), data))
}

pub(super) fn image_reference(value: &str) -> Result<String, String> {
    bpfman_image_oci::Reference::parse(value).map_err(|e| e.to_string())?;

    Ok(value.into())
}
