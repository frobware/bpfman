`tuf-root.json` is the Sigstore production TUF bootstrap shipped in
`sigstore-trust-root` 0.14.0, `repository/tuf_root.json`, from the official
[sigstore/sigstore-rust](https://github.com/sigstore/sigstore-rust) package.
It is signed root version 15, expiring 20 November 2026. This is public trust
metadata, containing no private keys.

Update this pinned bootstrap only through a reviewed trust-material change.
Runtime updates authenticate every subsequent root with both the previous root's
threshold and the new root's self-threshold. The bootstrap's expiration does not
prevent walking to a newer unexpired root. Cached roots never become independent
trust anchors. A newer reviewed bootstrap supersedes cached history at or below
its version; later roots must still form a consecutive authenticated chain.
