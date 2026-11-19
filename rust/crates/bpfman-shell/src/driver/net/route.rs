//! The harness-owned TEST-NET-2 policy rule.

use serde_json::{Map, Value as Json};

use crate::driver::process::Interrupt;

use super::topology::ip;

const PREFERENCE: &str = "BPFMAN_E2E_POLICY_RULE_PREF";
const DESTINATION: &str = "198.51.100.0";
const CIDR: &str = "198.51.100.0/24";

pub(super) fn ensure(interrupt: &Interrupt) -> Result<(), String> {
    let raw = std::env::var(PREFERENCE).unwrap_or_default();
    let preference = if raw.is_empty() {
        99
    } else {
        raw.parse::<u32>()
            .ok()
            .filter(|number| (1..=i32::MAX as u32).contains(number))
            .ok_or_else(|| format!("{PREFERENCE}={raw:?} is not a valid rule preference"))?
    };
    let at_preference = rules_at(preference, interrupt)?;
    if at_preference.iter().any(harness_rule) {
        return Ok(());
    }
    if !at_preference.is_empty() {
        return Err(format!(
            "ip rule preference {preference} is occupied by a foreign rule"
        ));
    }
    // The kernel rejects an identical rule installed by a concurrent creator.
    // Check again after a failed add before reporting the failure.
    interrupt.check()?;
    let added = ip(
        &[
            "-4",
            "rule",
            "add",
            "pref",
            &preference.to_string(),
            "to",
            CIDR,
            "lookup",
            "main",
        ],
        &Interrupt::unregistered(),
    )?;
    if !added.status.success() && !rules_at(preference, interrupt)?.iter().any(harness_rule) {
        return Err(format!(
            "install test-net rule: {}",
            String::from_utf8_lossy(&added.stderr).trim_end()
        ));
    }
    interrupt.check()
}

fn rules_at(preference: u32, interrupt: &Interrupt) -> Result<Vec<Map<String, Json>>, String> {
    let output = ip(&["-j", "-details", "-4", "rule", "show"], interrupt)?;
    if !output.status.success() {
        return Err(format!(
            "list IPv4 rules: {}",
            String::from_utf8_lossy(&output.stderr).trim_end()
        ));
    }
    let rules: Vec<Map<String, Json>> = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("decode IPv4 rules: {error}"))?;
    Ok(rules
        .into_iter()
        .filter(|rule| rule.get("priority").and_then(Json::as_u64) == Some(u64::from(preference)))
        .collect())
}

fn harness_rule(rule: &Map<String, Json>) -> bool {
    let text = |field| rule.get(field).and_then(Json::as_str);
    text("dst") == Some(DESTINATION)
        && rule.get("dstlen").and_then(Json::as_u64) == Some(24)
        && text("src") == Some("all")
        && (text("table") == Some("main") || rule.get("table").and_then(Json::as_u64) == Some(254))
        && rule.keys().all(|key| {
            matches!(
                key.as_str(),
                "priority" | "src" | "dst" | "dstlen" | "table" | "protocol"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_unconstrained_test_net_rules_are_owned() -> Result<(), serde_json::Error> {
        let mut rule = serde_json::from_str(
            r#"{"priority":99,"src":"all","dst":"198.51.100.0","dstlen":24,"table":"main","protocol":"unspec"}"#,
        )?;
        assert!(harness_rule(&rule));
        rule.insert("fwmark".to_owned(), Json::from(2));
        assert!(!harness_rule(&rule));
        rule.remove("fwmark");
        rule.insert("src".to_owned(), Json::from("192.0.2.0/24"));
        assert!(!harness_rule(&rule));
        Ok(())
    }
}
