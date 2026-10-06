use crate::{
    Kernel,
    failure::{filesystem, map},
};
use bpfman_fs::{LinkPin, LiveTracepoint, PreparedTracepointAttach, RuntimeWriter};
use bpfman_kernel::{Acquisition, Error, Removal, TracepointLinks};
use bpfman_model::Tracepoint;
use std::num::{NonZeroU32, NonZeroU64};

impl TracepointLinks for Kernel {
    type PreparedTracepoint = PreparedTracepointAttach<crate::AyaTracepoint>;
    type LiveTracepoint = LiveTracepoint<crate::AyaLink>;
    type LinkPin = LinkPin;

    fn prepare_tracepoint(
        &self,
        w: &RuntimeWriter<'_>,
        id: NonZeroU32,
    ) -> Result<PreparedTracepointAttach<crate::AyaTracepoint>, Error> {
        w.prepare_tracepoint_attach(self, id).map_err(filesystem)
    }

    fn attach_tracepoint(
        &self,
        w: &RuntimeWriter<'_>,
        p: PreparedTracepointAttach<crate::AyaTracepoint>,
        t: &Tracepoint,
    ) -> Result<LiveTracepoint<crate::AyaLink>, Error> {
        p.attach(w, t).map_err(filesystem)
    }

    fn pin_tracepoint(
        &self,
        w: &RuntimeWriter<'_>,
        l: LiveTracepoint<crate::AyaLink>,
        id: NonZeroU64,
    ) -> Acquisition<LinkPin> {
        l.pin(w, id).map_err(map)
    }

    fn link_id(pin: &LinkPin) -> NonZeroU32 {
        pin.kernel_id()
    }

    fn observe_link_pin(
        &self,
        w: &RuntimeWriter<'_>,
        id: NonZeroU64,
        p: NonZeroU32,
        k: Option<NonZeroU32>,
    ) -> Result<Option<LinkPin>, Error> {
        w.observe_link_pin(self, id, p, k).map_err(filesystem)
    }

    fn release_tracepoint(
        &self,
        w: &RuntimeWriter<'_>,
        l: LiveTracepoint<crate::AyaLink>,
    ) -> Removal<LiveTracepoint<crate::AyaLink>> {
        w.release_tracepoint(l).map_err(map)
    }

    fn remove_link(&self, w: &RuntimeWriter<'_>, p: LinkPin) -> Removal<LinkPin> {
        w.remove_link_pin(p).map_err(map)
    }
}
