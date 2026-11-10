//! Attribution remains caller policy across file/image batches and store reopen.

use super::*;
use bpfman_model::{ImagePullPolicy, ProgramSource};

pub(super) fn exercise<S: Store>(backend: S) {
    let f = Fixture::new(backend);

    for image in [false, true] {
        for owner in [None, Some(""), Some("alice's label ☃")] {
            let request = if image {
                PreparedProgram::from_image(
                    &f.kernel,
                    b"fake object".to_vec(),
                    ProgramSource::Image {
                        url: "registry.invalid/counter:test".into(),
                        digest: format!("sha256:{}", "0".repeat(64)),
                        pull_policy: ImagePullPolicy::Never,
                    },
                    ProgramSpec::Tracepoint("entry".try_into().expect("symbol")),
                    Default::default(),
                    &Cancellation::new(),
                )
                .expect("prepare image")
            } else {
                f.request(false)
            };
            let request = match owner {
                Some(owner) => request.with_owner(owner),
                None => request,
            };
            let request = request
                .with_additional_programs(
                    &f.kernel,
                    vec![ProgramSpec::Tracepoint(
                        "second".try_into().expect("symbol"),
                    )],
                )
                .expect("prepare batch");
            let loaded = f.app.load_batch(request).expect("load batch");
            assert_eq!(loaded.len(), 2);

            let store = ActiveStore::open(f.store.clone(), &f.layout, Duration::from_secs(1))
                .expect("reopen store");
            let reopened = Bpfman::new(store, f.kernel.clone(), Duration::from_secs(1));

            for program in loaded {
                assert_eq!(program.record.owner, owner.unwrap_or_default());
                assert_eq!(
                    reopened
                        .get(program.record.id)
                        .expect("reopened record")
                        .record
                        .owner,
                    owner.unwrap_or_default(),
                );
                assert_eq!(
                    f.app
                        .unload(program.record.id)
                        .expect("unload")
                        .unresolved(),
                    0
                );
            }
            f.clean();
        }
    }
}
