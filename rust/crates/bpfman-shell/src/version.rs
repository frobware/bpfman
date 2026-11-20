use std::{
    env::consts::{ARCH, OS},
    io::{self, Write},
};

pub(super) fn write(output: &mut impl Write) -> io::Result<()> {
    let version = option_env!("BPFMAN_BUILD_VERSION")
        .filter(|version| !version.is_empty())
        .unwrap_or("(devel)");
    let architecture = match ARCH {
        "x86_64" => "amd64",
        "x86" => "386",
        "aarch64" => "arm64",
        "loongarch64" => "loong64",
        architecture => architecture,
    };
    writeln!(output, "Version:     {version}")?;
    writeln!(
        output,
        "Git commit:  {}\nGit branch:  {}\nGit state:   {}\nBuild date:  {}",
        option_env!("BPFMAN_BUILD_GIT_COMMIT").unwrap_or_default(),
        option_env!("BPFMAN_BUILD_GIT_BRANCH").unwrap_or_default(),
        option_env!("BPFMAN_BUILD_GIT_STATE").unwrap_or_default(),
        option_env!("BPFMAN_BUILD_BUILD_DATE").unwrap_or_default(),
    )?;
    writeln!(output, "Rust version: {}", env!("BPFMAN_SHELL_COMPILER"))?;
    writeln!(output, "Platform:    {OS}/{architecture}")?;
    if let (Some(image), Some(identity), Some(issuer)) = (
        option_env!("BPFMAN_BUILD_IMAGE_REF").filter(|value| !value.is_empty()),
        option_env!("BPFMAN_BUILD_SIGNER_IDENTITY").filter(|value| !value.is_empty()),
        option_env!("BPFMAN_BUILD_OIDC_ISSUER").filter(|value| !value.is_empty()),
    ) {
        writeln!(
            output,
            "Attestation: cosign verify {image}:latest --certificate-identity '{identity}' --certificate-oidc-issuer {issuer}"
        )?;
    }
    Ok(())
}
