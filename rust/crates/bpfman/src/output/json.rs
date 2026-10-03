//! Go wire shapes. Domain and kernel values have no serialization derives.

use base64::{Engine, engine::general_purpose::STANDARD};
use bpfman_model::{
    ImagePullPolicy, KernelMap, KernelProgram, ObservedProgram, ProgramEntry, ProgramSource,
    ProgramSpec, StoredProgram,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn record(p: &StoredProgram) -> Value {
    let (source, image) = match &p.source {
        ProgramSource::File(path) => (json!(path), Value::Null),
        ProgramSource::Image {
            url,
            digest,
            pull_policy,
        } => (
            Value::Null,
            json!({"url":url,"digest":digest,"pull_policy":policy(*pull_policy)}),
        ),
    };
    let target = match &p.spec {
        ProgramSpec::Fentry { target, .. } | ProgramSpec::Fexit { target, .. } => {
            Some(target.as_str())
        }
        ProgramSpec::Lsm { hook, .. } => Some(hook.as_str()),
        _ => None,
    };

    // Go's SQLite decoder restores sharing in Handles.MapOwnerID only. It does
    // not restore the original load request's MapOwnerID; that wire field is null.
    let owner = (p.id != p.map_set).then_some(p.map_set.get());
    let globals = p
        .globals
        .iter()
        .map(|(k, v)| (k, v.as_ref().map(|v| STANDARD.encode(v))))
        .collect::<BTreeMap<_, _>>();
    json!({"program_id":p.id.get(),"license":p.license,"gpl_compatible":p.gpl_compatible,"created_at":p.created_at,"updated_at":p.updated_at,
        "load":{"object_path":p.object_path,"source_path":source,"program_name":p.spec.name().as_str(),"program_type":p.spec.kind().as_str(),"attach_func":target,"map_owner_id":null,"global_data":globals,"image_source":image},
        "handles":{"pin_path":p.pin_path,"map_pin_path":p.map_path,"map_owner_id":owner},
        "meta":{"name":p.spec.name().as_str(),"owner":p.owner,"description":p.description,"metadata":p.metadata}})
}

pub(super) fn policy(p: ImagePullPolicy) -> &'static str {
    match p {
        ImagePullPolicy::Always => "Always",
        ImagePullPolicy::IfNotPresent => "IfNotPresent",
        ImagePullPolicy::Never => "Never",
    }
}

fn kernel(p: &KernelProgram) -> Value {
    json!({"id":p.id.get(),"name":p.name,"program_type":p.kind,"tag":p.tag,"loaded_at":p.loaded_at.as_deref().unwrap_or("0001-01-01T00:00:00Z"),
        "uid":p.uid.unwrap_or_default(),"has_uid":p.uid.is_some(),"btf_id":p.btf_id.unwrap_or_default(),"has_btf_id":p.btf_id.is_some(),
        "map_ids":p.map_ids,"has_map_ids":p.map_ids.is_some(),"jited_size":p.jited_size,"xlated_size":p.xlated_size,"verified_insns":p.verified_insns,
        "memlock":p.memlock.unwrap_or_default(),"has_memlock":p.memlock.is_some(),"restricted":p.restricted})
}

fn map(m: &KernelMap, pin: &Option<String>, present: bool) -> Value {
    json!({"id":m.id,"name":m.name,"map_type":m.kind,"key_size":m.key_size,"value_size":m.value_size,"max_entries":m.max_entries,"flags":m.flags,
        "btf_id":m.btf_id.unwrap_or_default(),"has_btf_id":m.btf_id.is_some(),"map_extra":m.map_extra.unwrap_or_default(),"has_map_extra":m.map_extra.is_some(),
        "memlock":m.memlock.unwrap_or_default(),"has_memlock":m.memlock.is_some(),"frozen":m.frozen,"pin_path":pin.as_deref().unwrap_or(""),"present":present})
}

pub(super) fn program(p: &ObservedProgram) -> Value {
    json!({"record":record(&p.record),"status":{"kernel":kernel(&p.kernel),
        "stats":p.stats.as_ref().map(|s|json!({"runtime":s.runtime_ns,"run_count":s.run_count,"recursion_misses":s.recursion_misses})),
        "prog_pin":p.prog_pin,"map_dir":p.map_dir,"bytecode":p.bytecode,"links":[],"maps":p.maps.iter().map(|m|map(&m.kernel,&m.pin_path,m.present)).collect::<Vec<_>>(),"map_used_by":p.map_used_by}})
}

pub(super) fn entry(p: &ProgramEntry) -> Value {
    json!({"program_id":p.record.id.get(),"managed":true,"application":p.record.metadata.get("bpfman.io/application").map(String::as_str).unwrap_or(""),
        "type":p.record.spec.kind().as_str(),"function_name":p.record.spec.name().as_str(),"links":p.record.links,"record":record(&p.record),"kernel":p.kernel.as_ref().map(kernel)})
}

#[cfg(test)]
#[path = "../../../../tests/observation.rs"]
mod sample;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_observations_are_distinct_from_empty_collections_and_zero() {
        let mut p = sample::program();
        p.stats = None;
        p.kernel.uid = None;
        p.kernel.btf_id = None;
        p.kernel.memlock = None;
        p.kernel.map_ids = None;
        p.kernel.loaded_at = None;
        p.maps.clear();
        p.map_used_by.clear();
        p.record.source = ProgramSource::File(None);
        let value = program(&p);

        assert!(value["record"]["updated_at"].is_null());
        assert_eq!(value["record"]["load"]["global_data"], json!({}));
        assert!(value["record"]["load"]["image_source"].is_null());
        assert!(value["record"]["load"]["source_path"].is_null());
        assert_eq!(value["record"]["meta"]["metadata"], json!({}));
        assert!(value["status"]["stats"].is_null());

        for key in ["links", "maps", "map_used_by"] {
            assert_eq!(value["status"][key], json!([]));
        }

        let k = &value["status"]["kernel"];

        assert!(k["map_ids"].is_null());
        assert_eq!(k["has_map_ids"], false);
        assert_eq!(k["uid"], 0);
        assert_eq!(k["has_uid"], false);
        assert_eq!(k["loaded_at"], "0001-01-01T00:00:00Z");
        p.kernel.uid = Some(0);
        p.kernel.map_ids = Some(Vec::new());
        p.stats = Some(bpfman_model::ProgramStats {
            runtime_ns: 0,
            run_count: 0,
            recursion_misses: 0,
        });

        let value = program(&p);

        assert_eq!(value["status"]["kernel"]["has_uid"], true);
        assert_eq!(value["status"]["kernel"]["map_ids"], json!([]));
        assert_eq!(value["status"]["kernel"]["has_map_ids"], true);
        assert_eq!(
            value["status"]["stats"],
            json!({"runtime":0,"run_count":0,"recursion_misses":0})
        );
    }

    #[test]
    fn globals_preserve_nil_empty_and_bytes_and_listing_uses_its_own_shape() {
        let mut p = sample::record();
        p.map_set = std::num::NonZeroU32::new(7).expect("owner ID");
        p.globals.insert("nil".into(), None);
        p.globals.insert("empty".into(), Some(Vec::new()));
        p.globals.insert("bytes".into(), Some(vec![0, 1, 255]));
        let value = entry(&ProgramEntry {
            record: p,
            kernel: None,
        });

        assert_eq!(
            value["record"]["load"]["global_data"],
            json!({"nil":null,"empty":"","bytes":"AAH/"})
        );
        assert!(value["kernel"].is_null());
        assert!(value.get("status").is_none());
        assert_eq!(value["links"], json!([]));
        assert_eq!(value["managed"], true);
        assert_eq!(value["record"]["handles"]["map_owner_id"], 7);
        assert!(value["record"]["load"]["map_owner_id"].is_null());
        assert_eq!(value.as_object().map(|o| o.len()), Some(8));
    }
}
