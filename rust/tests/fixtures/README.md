# Signed OCI fixture

`signed-xdp-v0.5.6.json` captures unmodified public registry response bodies from
`quay.io/bpfman-bytecode/xdp_pass:v0.5.6`, captured on 17 October 2026. The index is
`sha256:89728a806e0cccfaf6491e560a2624dc8f4b070547196fd9e9ce64c44afdf01b`.
The capture includes the index, four Linux platform children/configs/bytecode and all three legacy
Cosign signature layers with their certificates, chains and Rekor bundles.
Bodies are base64 encoded to retain exact signed/hashed bytes. The fixture is
public Apache-2.0 bpfman release bytecode and public signing evidence; it contains
no registry credentials or private keys.

`signed-xdp-v0.5.6-rekor.json` captures the public Rekor lookup response for the
first signature, including its inclusion proof and signed checkpoint. It was
retrieved by posting that signature's hashedrekord body to
`https://rekor.sigstore.dev/api/v1/log/entries/retrieve` on the same date.

Tests use the real Sigstore production trust root and the authenticated historical
signing time, without replacing certificate validation or disabling SCT/Rekor
checks. All fixture-based checks run without external network access.
