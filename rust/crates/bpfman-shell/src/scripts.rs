use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

use bpfman_core::MetadataSelector;
use serde::Deserialize;
use serde_json::{Map, Value};

type Labels = BTreeMap<String, String>;

pub(super) fn list(
    paths: &[PathBuf],
    selector: &str,
    output: &mut impl Write,
) -> Result<(), String> {
    let filter = selector
        .parse::<MetadataSelector>()
        .map_err(|error| format!("parse --selector={selector:?}: {error}"))?;
    let mut scripts = Vec::new();
    for path in paths {
        let metadata =
            fs::metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
        if metadata.is_dir() {
            if !fs::symlink_metadata(path)
                .map_err(|error| format!("{}: {error}", path.display()))?
                .is_symlink()
            {
                visit(path.strip_prefix(".").unwrap_or(path), &mut scripts)?;
            }
        } else {
            scripts.push(path.clone());
        }
    }
    if scripts.is_empty() {
        return Err("--list-scripts requires at least one script file or directory".to_owned());
    }
    scripts.sort_unstable();
    for path in scripts {
        let file = File::open(&path).map_err(|error| {
            format!("open script {} for header prescan: {error}", path.display())
        })?;
        let labels = scan(BufReader::new(file), &path)?;
        if filter.matches(&labels) {
            writeln!(output, "{}", path.display()).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn visit(path: &Path, scripts: &mut Vec<PathBuf>) -> Result<(), String> {
    let directory = if path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        path
    };
    let entries =
        fs::read_dir(directory).map_err(|error| format!("{}: {error}", directory.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("{}: {error}", directory.display()))?;
        let path = path.join(entry.file_name());
        let kind = entry
            .file_type()
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if kind.is_dir() {
            visit(&path, scripts)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "bpfman")
        {
            scripts.push(path);
        }
    }
    Ok(())
}

fn scan(reader: impl BufRead, path: &Path) -> Result<Labels, String> {
    let mut labels = Labels::new();
    let mut cases_seen = false;
    for line in reader.lines().take(20) {
        let line = line.map_err(|error| {
            format!("read script {} for header prescan: {error}", path.display())
        })?;
        let Some(pragma) = line.trim().strip_prefix("#pragma").map(str::trim) else {
            continue;
        };
        if let Some(raw) = pragma.strip_prefix("labels=") {
            labels.extend(parse_labels(raw.trim()).map_err(|error| {
                format!("parse {} labels pragma {raw:?}: {error}", path.display())
            })?);
        }
        if let Some(raw) = pragma.strip_prefix("cases=") {
            if cases_seen {
                return Err(format!("parse {}: repeated cases pragma", path.display()));
            }
            validate_cases(raw)
                .map_err(|error| format!("parse {} cases pragma: {error}", path.display()))?;
            cases_seen = true;
        }
    }
    Ok(labels)
}

#[derive(Deserialize)]
#[serde(untagged)]
enum LabelPragma {
    Map(BTreeMap<String, Option<String>>),
    List(Vec<Option<String>>),
    Csv(String),
}

fn parse_labels(raw: &str) -> Result<Labels, &'static str> {
    let pragma = serde_json::from_str::<Option<LabelPragma>>(raw)
        .map_err(|_| "expected JSON string, JSON string array, or JSON object")?;
    Ok(match pragma {
        None => Labels::new(),
        Some(LabelPragma::Map(entries)) => entries
            .into_iter()
            .filter_map(|(key, value)| {
                let key = key.trim().to_lowercase();
                (!key.is_empty()).then(|| (key, value.unwrap_or_default().trim().to_lowercase()))
            })
            .collect(),
        Some(LabelPragma::List(entries)) => entries
            .into_iter()
            .map(|key| (key.unwrap_or_default(), "true".to_owned()))
            .collect(),
        Some(LabelPragma::Csv(csv)) => csv
            .split(',')
            .map(|key| key.trim().to_lowercase())
            .filter(|key| !key.is_empty())
            .map(|key| (key, "true".to_owned()))
            .collect(),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    #[serde(default)]
    name: String,
    params: Option<Map<String, Value>>,
}

fn validate_cases(raw: &str) -> Result<(), String> {
    let mut reader = serde_json::Deserializer::from_str(raw);
    let cases = Option::<Vec<Case>>::deserialize(&mut reader)
        .map_err(|error| error.to_string())?
        .unwrap_or_default();
    if !(1..=64).contains(&cases.len()) {
        return Err("expected 1 to 64 cases".to_owned());
    }
    let mut names = BTreeSet::new();
    for Case { name, params } in cases {
        let mut bytes = name.bytes();
        if !bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
            || !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            || params.is_none()
            || !names.insert(name.clone())
        {
            return Err(format!(
                "invalid/duplicate name or missing params: {name:?}"
            ));
        }
    }
    reader.end().map_err(|_| "trailing JSON".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_merges_labels_and_bounds_metadata() -> Result<(), String> {
        let mut source = "#pragma labels={\" Key \":\" VALUE \"}\n#pragma labels=[\"Exact\"]\n#pragma labels=\" spaced, second \"\n#pragma cases=[{\"name\":\"First-1\",\"params\":{}}]\n".to_owned();
        source.push_str(&"# padding\n".repeat(16));
        source.push_str("#pragma labels=invalid\n");
        let result = scan(source.as_bytes(), Path::new("example.bpfman"))?;
        assert_eq!(result["key"], "value");
        for name in ["Exact", "spaced", "second"] {
            assert_eq!(result[name], "true");
        }
        Ok(())
    }

    #[test]
    fn invalid_cases_cannot_be_hidden_by_a_selector() {
        for cases in [
            r#"[{"name":"abc","params":{}},{"name":"abc","params":{}}]"#,
            r#"[{"name":"abc","params":null}]"#,
            r#"[{"name":"abc","params":{},"unknown":true}]"#,
            r#"[{"name":"abc","params":{}}] {}"#,
        ] {
            assert!(validate_cases(cases).is_err(), "{cases}");
        }
    }
}
