//! Go's logging vocabulary, translated to the corresponding Rust components.

use std::collections::BTreeMap;

pub(super) fn directive(spec: &str) -> anyhow::Result<String> {
    let mut base = "warn";
    let mut components = BTreeMap::new();
    for (index, part) in spec.trim().split(',').enumerate() {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((component, raw)) = part.split_once('=') {
            let component = component.trim();
            if component.is_empty() {
                anyhow::bail!("empty component name in log spec");
            }
            components.insert(component, level(raw)?);
        } else if index == 0 {
            base = level(part)?;
        } else {
            anyhow::bail!("base log level must be first in spec");
        }
    }
    let mut result = base.to_owned();
    for (component, level) in components {
        let targets: &[&str] = match component {
            "manager" => &["bpfman_runtime"],
            "store" => &["bpfman_store_sqlite"],
            "kernel" => &["bpfman_kernel_aya"],
            "fs" | "setup" => &["bpfman_fs"],
            "lock" => &["bpfman_lock"],
            "image" => &["bpfman_image_oci"],
            other => &[other],
        };
        for target in targets {
            // EnvFilter accepts a larger grammar than a component name. Keep
            // Go's arbitrary names inert instead of interpreting them as code.
            if target
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_:-".contains(&byte))
            {
                result.push_str(&format!(",{target}={level}"));
            }
        }
    }
    Ok(result)
}

fn level(raw: &str) -> anyhow::Result<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "trace" => Ok("trace"),
        "debug" => Ok("debug"),
        "info" => Ok("info"),
        "warn" | "warning" => Ok("warn"),
        "error" | "err" => Ok("error"),
        _ => anyhow::bail!("unknown log level: {raw:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_components_aliases_whitespace_and_last_override() -> anyhow::Result<()> {
        assert_eq!(
            directive(" WARNING, manager=TRACE, store=err, manager=DEBUG ")?,
            "warn,bpfman_runtime=debug,bpfman_store_sqlite=error"
        );
        assert_eq!(directive("")?, "warn");
        assert_eq!(directive("store=debug")?, "warn,bpfman_store_sqlite=debug");
        for invalid in ["off", "info,debug", "info,=debug", "manager=nope"] {
            assert!(directive(invalid).is_err(), "{invalid}");
        }
        Ok(())
    }
}
