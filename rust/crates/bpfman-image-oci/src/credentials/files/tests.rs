#![allow(clippy::expect_used)]
use super::*;
use crate::Reference;
use serde_json::json;
use std::{ffi::OsStr, os::unix::ffi::OsStrExt, sync::atomic::Ordering};

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

fn parse(
    value: serde_json::Value,
    registry: &str,
) -> std::result::Result<Option<Credentials>, Error> {
    serde_json::from_value::<Config>(value)
        .expect("config")
        .credentials(registry)
}

fn basic() -> serde_json::Value {
    json!({"auth": STANDARD.encode("file-user:password:with:colons")})
}

#[test]
fn paths_follow_go_process_context_order_and_preserve_native_bytes() {
    let mut env = BTreeMap::from([
        ("REGISTRY_AUTH_FILE", OsString::from("/explicit/auth.json")),
        ("XDG_RUNTIME_DIR", OsString::from("/run/user/0")),
        ("HOME", OsString::from("/root")),
        ("DOCKER_CONFIG", OsString::from("/docker")),
        ("SUDO_USER", OsString::from("someone-else")),
        ("XDG_CONFIG_HOME", OsString::from("/ignored")),
    ]);
    assert_eq!(
        paths(|key| env.get(key).cloned()),
        [
            "/explicit/auth.json",
            "/run/user/0/containers/auth.json",
            "/root/.config/containers/auth.json",
            "/docker/config.json"
        ]
        .map(PathBuf::from)
    );
    env.insert(
        "REGISTRY_AUTH_FILE",
        "/run/user/0/containers/auth.json".into(),
    );
    env.insert("DOCKER_CONFIG", "".into());
    assert_eq!(paths(|key| env.get(key).cloned()).len(), 3);
    assert_eq!(
        paths(|key| env.get(key).cloned()).last(),
        Some(&PathBuf::from("/root/.docker/config.json"))
    );
    env.remove("HOME");
    env.insert("DOCKER_CONFIG", "/ignored-without-home".into());
    assert_eq!(paths(|key| env.get(key).cloned()).len(), 1);
    env.clear();
    env.insert("HOME", OsStr::from_bytes(b"/home/native-\xff").to_owned());
    assert_eq!(
        paths(|key| env.get(key).cloned())[0].as_os_str().as_bytes(),
        b"/home/native-\xff/.config/containers/auth.json"
    );
}

#[test]
fn registry_matching_is_authority_scoped_and_aliases_are_deterministic() -> Result {
    for key in ["quay.io", "https://quay.io/", "http://quay.io/v2/"] {
        let credentials =
            parse(json!({"auths": {key: basic()}}), "quay.io")?.ok_or("missing credentials")?;
        assert_eq!(
            credentials.basic()?,
            format!(
                "Basic {}",
                STANDARD.encode("file-user:password:with:colons")
            )
        );
        assert!(!format!("{credentials:?}").contains("password"));
    }
    for key in [
        "quay.io.evil.test",
        "quay.io:443",
        "other.test/quay.io",
        "https://user@quay.io/",
    ] {
        assert!(parse(json!({"auths": {key: basic()}}), "quay.io")?.is_none());
    }
    for registry in ["docker.io", "index.docker.io", "registry-1.docker.io"] {
        assert!(
            parse(
                json!({"auths": {"https://index.docker.io/v1/": basic()}}),
                registry
            )?
            .is_some()
        );
    }
    assert!(parse(json!({"auths": {"quay.io": basic()}}), "quay.io:5000")?.is_none());
    assert!(
        parse(
            json!({"auths": {"https://quay.io/": basic(), "http://quay.io/": basic()}}),
            "quay.io"
        )
        .is_err()
    );
    // An exact key wins even when aliases also exist; malformed unrelated
    // entries cannot block this registry or leak another registry's credentials.
    assert!(parse(json!({"auths": {"quay.io": basic(), "http://quay.io/": false, "other.test": "secret"}}), "quay.io")?.is_some());
    Ok(())
}

#[test]
fn entry_precedence_helpers_tokens_and_redacted_failures() -> Result {
    assert!(
        parse(
            json!({"auths": {"quay.io": {"username":"legacy", "password":"password"}}}),
            "quay.io"
        )?
        .is_some()
    );
    let credentials = parse(json!({"auths": {"quay.io": {"auth": STANDARD.encode("preferred:secret"), "username":"ignored", "password":"ignored"}}}), "quay.io")?.ok_or("missing")?;
    assert_eq!(
        credentials.basic()?,
        format!("Basic {}", STANDARD.encode("preferred:secret"))
    );
    assert!(parse(json!({"auths":{"quay.io":{}}}), "quay.io")?.is_none());
    for value in [
        json!({"credHelpers":{"quay.io":"helper-secret"}, "auths":{"quay.io":basic()}}),
        json!({"credsStore":"helper-secret", "auths":{"quay.io":basic()}}),
        json!({"auths":{"quay.io":{"identitytoken":"identity-secret"}}}),
        json!({"auths":{"quay.io":{"auth":"encoding-secret!"}}}),
        json!({"auths":{"quay.io":{"auth":STANDARD.encode("decoded-secret")}}}),
        json!({"auths":{"quay.io":{"password":42}}}),
        json!({"auths":{"quay.io":[]}}),
        json!({"auths":{"quay.io":{"registrytoken":"token-secret\n"}}}),
    ] {
        let error = parse(value, "quay.io").expect_err("unsupported/malformed");
        assert!(!format!("{error:?} {error}").contains("secret"));
    }
    assert!(
        parse(
            json!({"credHelpers":{"other.test":"ignored"}, "auths":{"quay.io":basic()}}),
            "quay.io"
        )?
        .is_some()
    );
    let credentials = parse(
        json!({"auths":{"quay.io":{"registrytoken":"registry-secret"}}}),
        "quay.io",
    )?
    .ok_or("missing")?;
    assert_eq!(credentials.bearer(), Some("Bearer registry-secret"));
    assert!(credentials.basic().is_err());
    assert!(!format!("{credentials:?}").contains("registry-secret"));
    Ok(())
}

#[test]
fn first_existing_file_wins_without_merging_or_writes() -> Result {
    let temp = tempfile::tempdir()?;
    let first = temp.path().join("first.json");
    let second = temp.path().join("second.json");
    let missing = temp.path().join("absent.json");
    let flag = AtomicBool::new(false);
    std::fs::write(&second, json!({"auths":{"quay.io":basic()}}).to_string())?;
    assert!(resolve("quay.io", &[missing.clone(), second.clone()], &flag)?.is_some());
    std::fs::write(&first, r#"{"auths":{}}"#)?;
    let before = std::fs::read(&first)?;
    assert!(resolve("quay.io", &[missing, first.clone(), second.clone()], &flag)?.is_none());
    assert_eq!(std::fs::read(&first)?, before);
    std::fs::write(&first, r#"{"auths": "private-secret"}"#)?;
    let error = resolve("quay.io", &[first.clone(), second], &flag).expect_err("no fallback");
    assert!(!format!("{error:?} {error}").contains("private-secret"));
    let credentials = Credentials {
        source: Source::Files {
            registry: "quay.io".into(),
            paths: vec![first],
        },
    };
    assert!(
        credentials
            .resolve(&Reference::parse("other.test/a")?, &flag)
            .is_err()
    );
    flag.store(true, Ordering::Relaxed);
    assert!(
        credentials
            .resolve(&Reference::parse("quay.io/a")?, &flag)
            .expect_err("cancelled")
            .is_cancelled()
    );
    assert_eq!(std::fs::read_dir(temp.path())?.count(), 2);
    Ok(())
}

#[test]
fn nonregular_oversized_symlinked_and_invalid_files() -> Result {
    let temp = tempfile::tempdir()?;
    let flag = AtomicBool::new(false);
    assert!(resolve("quay.io", &[temp.path().to_owned()], &flag).is_err());
    let file = temp.path().join("auth.json");
    std::fs::File::create(&file)?.set_len(FILE_LIMIT as u64 + 1)?;
    assert!(resolve("quay.io", std::slice::from_ref(&file), &flag).is_err());
    std::fs::write(&file, json!({"auths":{"quay.io":basic()}}).to_string())?;
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&file, &link)?;
    assert!(resolve("quay.io", &[link], &flag)?.is_some());
    for bytes in [
        b"not-json-secret".as_slice(),
        b"null",
        b"[]",
        b"{\"auths\": false}",
        b"{} trailing-secret",
    ] {
        std::fs::write(&file, bytes)?;
        let error = resolve("quay.io", std::slice::from_ref(&file), &flag).expect_err("bad file");
        assert!(!format!("{error:?} {error}").contains("secret"));
    }
    Ok(())
}
