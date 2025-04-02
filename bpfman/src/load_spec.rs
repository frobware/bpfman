#![allow(dead_code)]

use std::collections::HashMap;

use derive_builder::Builder;
use serde_json;

#[derive(Debug, Builder)]
#[builder(pattern = "mutable", build_fn(name = "build_partial"))]
pub struct LoadSpec2 {
    #[builder(setter(into))]
    function_names: Vec<String>,

    #[builder(setter(strip_option), default)]
    global_data: Option<Vec<(String, Vec<u8>)>>,

    #[builder(setter(strip_option), default)]
    metadata: Option<Vec<(String, String)>>,

    #[builder(default)]
    map_owner_id: Option<u32>,

    #[builder(setter(into))]
    program_bytes: Vec<u8>,

    #[builder(setter(strip_option), default)]
    raw_programs: Option<Vec<(String, Vec<String>)>>,

    #[builder(setter(skip), default)]
    global_data_json: String,

    #[builder(setter(skip), default)]
    metadata_json: String,
}

impl LoadSpec2Builder {
    pub fn build(&mut self) -> Result<LoadSpec2, String> {
        let mut spec = self.build_partial().map_err(|e| e.to_string())?;

        // Convert global_data → global_data_json
        let global_data_map = Self::global_data_to_map(spec.global_data.as_deref().unwrap_or(&[]));
        spec.global_data_json = serde_json::to_string(&global_data_map)
            .map_err(|e| format!("Failed to serialize global data: {}", e))?;

        // Convert metadata → metadata_json
        let metadata_map = Self::metadata_to_map(spec.metadata.as_deref().unwrap_or(&[]));
        spec.metadata_json = serde_json::to_string(&metadata_map)
            .map_err(|e| format!("Failed to serialize metadata: {}", e))?;

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
    use super::*;
    // Use the following import to test like a client would.
    //use crate::load_spec::LoadSpec2Builder;

    #[test]
    fn test_build_fails_with_no_fields() {
        let result = LoadSpec2Builder::default().build();

        assert!(result.is_err());
        assert!(
            result.as_ref().unwrap_err().contains("function_names"),
            "Error should mention missing `function_names`, got: {:?}",
            result
        );
    }

    #[test]
    fn test_build_fails_without_program_bytes() {
        let result = LoadSpec2Builder::default()
            .function_names(vec!["main".into()])
            .build();

        assert!(result.is_err());
        assert!(
            result.as_ref().unwrap_err().contains("program_bytes"),
            "Error should mention missing `program_bytes`, got: {:?}",
            result
        );
    }

    #[test]
    fn test_build_success_with_required_fields() {
        let result = LoadSpec2Builder::default()
            .function_names(vec!["main".into()])
            .program_bytes(vec![0xde, 0xad])
            .build();

        assert!(
            result.is_ok(),
            "Expected build to succeed with required fields"
        );
        let spec = result.unwrap();
        assert_eq!(&spec.function_names, &vec!["main".to_string()]);
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
}
