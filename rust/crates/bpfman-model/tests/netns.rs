//! Pure namespace selector validation, independent of filesystem state.
use bpfman_model::NetworkNamespace;

#[test]
fn namespace_selection_accepts_current_or_absolute_paths_only() {
    for path in ["", "/run/netns/example", "/proc/123/ns/net"] {
        let namespace: NetworkNamespace = path.parse().expect("valid selector");
        assert_eq!(namespace.as_str(), path);
    }
    for path in ["relative", "../netns", "~/netns", "/run/netns/a\0b"] {
        assert!(path.parse::<NetworkNamespace>().is_err());
    }
    assert_eq!(NetworkNamespace::default().as_str(), "");
}
