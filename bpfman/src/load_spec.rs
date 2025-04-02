#![allow(dead_code)]

use std::collections::HashMap;

use derive_builder::Builder;
use serde_json;

use crate::ProgramType;

#[derive(Debug, Builder)]
#[builder(pattern = "mutable", build_fn(name = "build_partial"))]
pub struct LoadSpec2 {
    #[builder(setter(into))]
    function_names: Option<Vec<String>>,

    #[builder(setter(strip_option), default)]
    global_data: Option<Vec<(String, Vec<u8>)>>,

    #[builder(setter(strip_option), default)]
    metadata: Option<Vec<(String, String)>>,

    #[builder(default)]
    map_owner_id: Option<u32>,

    #[builder(setter(into))]
    program_bytes: Vec<u8>,

    #[builder(setter(into), default)]
    raw_programs: Vec<(String, Vec<String>)>,

    #[builder(setter(skip), default = "String::from(\"{}\")")]
    global_data_json: String,

    #[builder(setter(skip), default = "String::from(\"{}\")")]
    metadata_json: String,

    #[builder(setter(skip), default)]
    programs_by_type: Vec<(ProgramType, String)>,
}

impl LoadSpec2Builder {
    pub fn build(&mut self) -> Result<LoadSpec2, String> {
        let mut spec = self.build_partial().map_err(|e| e.to_string())?;

        let global_data_map = Self::global_data_to_map(spec.global_data.as_deref().unwrap_or_default());
        spec.global_data_json = serde_json::to_string(&global_data_map)
            .map_err(|e| format!("Failed to serialise global data to JSON: {}", e))?;

        let metadata_map = Self::metadata_to_map(spec.metadata.as_deref().unwrap_or_default());
        spec.metadata_json = serde_json::to_string(&metadata_map)
            .map_err(|e| format!("Failed to serialise metadata to JSON: {}", e))?;

        let mut validated_programs = Vec::new();

        for (program_type_str, parts) in self.raw_programs.as_ref().unwrap_or(&vec![]) {
            let name = parts
                .first()
                .ok_or_else(|| format!("Missing program name for {}", program_type_str))?;

            if matches!(program_type_str.as_str(), "fentry" | "fexit") && parts.len() != 2 {
                return Err(format!(
                    "Missing function name for {} program",
                    program_type_str
                ));
            }

            let fn_name = if matches!(program_type_str.as_str(), "fentry" | "fexit") {
                parts.get(1).map(|s| s.as_str())
            } else {
                None
            };

            let program_type = ProgramType::from_str(program_type_str, fn_name)
                .map_err(|e| format!("Invalid program type: {}", e))?;

            validated_programs.push((program_type, name.clone()));
        }

        spec.programs_by_type = validated_programs;

        Ok(spec)
    }

    fn global_data_to_map(data: &[(String, Vec<u8>)]) -> HashMap<String, Vec<u8>> {
        data.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    fn metadata_to_map(data: &[(String, String)]) -> HashMap<String, String> {
        data.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }
}

#[cfg(test)]
mod tests {
    //use super::*;
    // Use the following import to test like a client would.
    use crate::load_spec::LoadSpec2Builder;

    #[test]
    fn test_build_fails_with_no_fields() {
        let result = LoadSpec2Builder::default().build();
        assert!(result.is_err());
    }

    #[test]
    fn test_build_global_data_serialises_to_json() {
        let result = LoadSpec2Builder::default()
            .function_names(vec!["main".into()])
            .program_bytes(vec![0xde, 0xad])
            .global_data(vec![
                ("key1".into(), b"value1".to_vec()),
                ("key2".into(), b"value2".to_vec()),
            ])
            .build();

        assert!(result.is_ok());
        let spec = result.unwrap();

        let json: serde_json::Value = serde_json::from_str(&spec.global_data_json).unwrap();
        assert!(json.get("key1").is_some(), "expected key1 in JSON");
        assert!(json.get("key2").is_some(), "expected key2 in JSON");
    }

    #[test]
    fn test_build_metadata_serialises_to_json() {
        let result = LoadSpec2Builder::default()
            .function_names(vec!["main".into()])
            .program_bytes(vec![0xde, 0xad])
            .metadata(vec![
                ("key1".into(), "value1".to_string()),
                ("key2".into(), "value2".to_string()),
            ])
            .build();

        assert!(result.is_ok());
        let spec = result.unwrap();

        let json: serde_json::Value = serde_json::from_str(&spec.metadata_json).unwrap();
        assert!(json.get("key1").is_some(), "expected key1 in JSON");
        assert!(json.get("key2").is_some(), "expected key2 in JSON");
    }

    #[test]
    fn test_build_valid_program_types() {
        let result = LoadSpec2Builder::default()
            .function_names(vec!["main".into()])
            .program_bytes(vec![0xde, 0xad])
            .raw_programs(vec![
                ("fentry".into(), vec!["program1".into(), "func1".into()]),
                ("fexit".into(), vec!["program2".into(), "func2".into()]),
            ])
            .build();

        assert!(
            result.is_ok(),
            "Expected build to succeed with valid program types"
        );
        let spec = result.unwrap();

        assert_eq!(spec.raw_programs.len(), 2);
    }

    #[test]
    fn test_build_invalid_program_types() {
        let result = LoadSpec2Builder::default()
            .function_names(Some(vec!["main".into()]))
            .program_bytes(vec![0xde, 0xad])
            .raw_programs(vec![
                ("invalid_type".into(), vec!["program1".into()]),
            ])
            .build();

        assert!(
            result.is_err(),
            "Expected build to fail with invalid program types"
        );
    }

    #[test]
    fn test_build_missing_fentry_function_name() {
        let result = LoadSpec2Builder::default()
            .function_names(Some(vec!["main".into()]))
            .program_bytes(vec![0xde, 0xad])
            .raw_programs(vec![
                ("fentry".into(), vec!["program2".into()]),
            ])
            .build();

        assert!(
            result.is_err(),
            "Expected build to fail with invalid program types"
        );
    }
}
