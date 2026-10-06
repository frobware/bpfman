;; The new Rust workspace lives under rust/; the root Cargo.toml is the
;; legacy one.
((nil . ((eglot-workspace-configuration
          . (:rust-analyzer (:linkedProjects ["rust/Cargo.toml"]))))))
