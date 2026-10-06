use crate::{
    Dispatcher, Kernel,
    failure::{filesystem, map},
};
use aya::programs::Xdp;
use bpfman_fs::{
    PreparedXdp, RuntimeWriter, XdpExtensionPin, XdpOuter, XdpProgramPin, XdpRevision,
};
use bpfman_kernel::{Acquisition, Error, ErrorKind, Removal, XdpArtifacts, XdpLifecycle};
use bpfman_model::{InterfaceName, XdpKey, XdpProceedOn, XdpSnapshot};
use std::num::NonZeroU32;

fn kernel(e: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::new(ErrorKind::Unavailable, "XDP kernel operation", e)
}

fn missing() -> Error {
    Error::new(
        ErrorKind::InvalidData,
        "embedded dispatcher missing",
        std::io::Error::other("xdp_dispatcher"),
    )
}

impl XdpLifecycle for Kernel {
    type PreparedXdp = PreparedXdp<crate::AyaExtension>;
    type Dispatcher = Dispatcher;
    type Outer = XdpOuter<crate::AyaOuter>;
    type Extension = XdpExtensionPin;
    type DispatcherPin = XdpProgramPin;
    type Revision = XdpRevision;

    fn prepare_xdp(
        &self,
        w: &RuntimeWriter<'_>,
        program: NonZeroU32,
        interface: &InterfaceName,
    ) -> Result<(XdpKey, PreparedXdp<crate::AyaExtension>), Error> {
        let p = w
            .prepare_xdp(self, program, interface)
            .map_err(filesystem)?;
        Ok((p.key(), p))
    }

    fn load_dispatcher(&self, proceed_on: XdpProceedOn) -> Result<Dispatcher, Error> {
        let config = bpfman_model::xdp_config(proceed_on);
        let mut bpf = aya::EbpfLoader::new()
            .override_global("conf", config.as_slice(), true)
            .load(aya::include_bytes_aligned!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../../dispatcher/xdp_dispatcher_v2.bpf.o"
            )))
            .map_err(kernel)?;
        let program: &mut Xdp = bpf
            .program_mut("xdp_dispatcher")
            .ok_or_else(missing)?
            .try_into()
            .map_err(kernel)?;
        program.load().map_err(kernel)?;
        Ok(Dispatcher(bpf))
    }

    fn create_revision(
        &self,
        w: &RuntimeWriter<'_>,
        p: &PreparedXdp<crate::AyaExtension>,
    ) -> Acquisition<XdpRevision> {
        p.create_revision(w, NonZeroU32::MIN).map_err(map)
    }

    fn pin_dispatcher(
        &self,
        w: &RuntimeWriter<'_>,
        r: &XdpRevision,
        k: &mut Dispatcher,
    ) -> Acquisition<XdpProgramPin> {
        r.pin_program(w, k).map_err(map)
    }

    fn pin_extension(
        &self,
        w: &RuntimeWriter<'_>,
        p: &mut PreparedXdp<crate::AyaExtension>,
        r: &XdpRevision,
        k: &Dispatcher,
    ) -> Acquisition<XdpExtensionPin> {
        p.pin_extension(w, r, k).map_err(map)
    }

    fn pin_outer(
        &self,
        w: &RuntimeWriter<'_>,
        p: &PreparedXdp<crate::AyaExtension>,
        k: &Dispatcher,
    ) -> Acquisition<XdpOuter<crate::AyaOuter>> {
        p.pin_outer(self, w, k).map_err(map)
    }

    fn dispatcher_id(p: &XdpProgramPin) -> NonZeroU32 {
        p.id()
    }

    fn extension_id(p: &XdpExtensionPin) -> NonZeroU32 {
        p.id()
    }

    fn outer_id(p: &XdpOuter<crate::AyaOuter>) -> Result<NonZeroU32, Error> {
        p.id().map_err(filesystem)
    }

    fn observe_xdp(
        &self,
        w: &RuntimeWriter<'_>,
        s: &XdpSnapshot,
    ) -> Result<XdpArtifacts<Self>, Error> {
        let a = w.observe_xdp(self, s).map_err(filesystem)?;
        Ok(XdpArtifacts {
            outer: a.outer,
            extension: a.extension,
            program: a.program,
            directory: a.directory,
        })
    }

    fn remove_outer(
        &self,
        w: &RuntimeWriter<'_>,
        r: XdpOuter<crate::AyaOuter>,
    ) -> Removal<XdpOuter<crate::AyaOuter>> {
        w.remove_xdp_outer(self, r).map_err(map)
    }

    fn remove_extension(
        &self,
        w: &RuntimeWriter<'_>,
        r: XdpExtensionPin,
    ) -> Removal<XdpExtensionPin> {
        w.remove_xdp_extension(r).map_err(map)
    }

    fn remove_dispatcher(&self, w: &RuntimeWriter<'_>, r: XdpProgramPin) -> Removal<XdpProgramPin> {
        w.remove_xdp_program(r).map_err(map)
    }

    fn remove_revision(&self, w: &RuntimeWriter<'_>, r: XdpRevision) -> Removal<XdpRevision> {
        w.remove_xdp_revision(r).map_err(map)
    }
}
