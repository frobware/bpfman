FROM bpfman-base:latest
COPY ./target/debug/bpfman ./target/debug/bpfman-ns ./target/debug/bpfman-rpc /usr/local/bin/
ENTRYPOINT ["/usr/local/bin/bpfman-rpc", "--timeout=0"]
LABEL org.opencontainers.image.title="bpfman-fast"
LABEL org.opencontainers.image.description="Fast iterative bpfman build"
LABEL build-method="base + binary copy"
