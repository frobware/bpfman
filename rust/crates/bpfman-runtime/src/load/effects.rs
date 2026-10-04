//! Private substitution boundary for the production load interpreter.

use crate::{LoadCleanup, kernel::LocalObject, load_error::Failure};
use bpfman_core::EffectFailure;
use bpfman_fs::RuntimeWriter;
use bpfman_model::{StoredProgramSummary, Symbol};
use std::{collections::BTreeMap, num::NonZeroU32};

pub(crate) struct Inputs<'a> {
    pub(super) cancellation: &'a crate::Cancellation,
    pub(super) object: &'a LocalObject,
    pub(super) source: &'a str,
    pub(super) name: &'a Symbol,
    pub(super) metadata: &'a BTreeMap<String, String>,
    pub(super) created_at: &'a str,
}

pub(crate) type FailureFor<F> = Failure<
    <F as LoadCleanup>::ProgramPin,
    <F as LoadCleanup>::MapPin,
    <F as LoadCleanup>::Bytecode,
    <F as CleanupEffects>::MapDirectory,
    <F as LoadCleanup>::Error,
>;

// Failed acquisition can return a resource even when the effect did not finish.
type AcquisitionResult<R, E> = Result<R, EffectFailure<Option<R>, E>>;
type PublicationResult<R, E> = Result<R, EffectFailure<Vec<R>, E>>;

// Every mutation borrows the same scoped writer. Receipts remain adapter-owned;
// no production injection flags or arbitrary path mutation APIs are exposed.
pub(crate) trait LoadEffects: CleanupEffects {
    type Store;
    type Prepared;
    type Kernel;

    fn cancelled(&self) -> Self::Error;

    fn open_store(&mut self, writer: &RuntimeWriter<'_>) -> Result<Self::Store, Self::Error>;

    fn prepare(&mut self, writer: &RuntimeWriter<'_>) -> Result<Self::Prepared, Self::Error>;

    fn load_kernel(
        &mut self,
        writer: &RuntimeWriter<'_>,
        input: &Inputs<'_>,
    ) -> Result<Self::Kernel, Self::Error>;

    fn pin_program(
        &mut self,
        writer: &RuntimeWriter<'_>,
        prepared: &Self::Prepared,
        kernel: &mut Self::Kernel,
        name: &Symbol,
    ) -> AcquisitionResult<Self::ProgramPin, Self::Error>;

    fn program_id(pin: &Self::ProgramPin) -> NonZeroU32;

    fn create_map_directory(
        &mut self,
        writer: &RuntimeWriter<'_>,
        prepared: &Self::Prepared,
        id: NonZeroU32,
    ) -> AcquisitionResult<Self::MapDirectory, Self::Error>;

    fn pin_map(
        &mut self,
        writer: &RuntimeWriter<'_>,
        kernel: &Self::Kernel,
        directory: &Self::MapDirectory,
        name: &str,
    ) -> AcquisitionResult<Self::MapPin, Self::Error>;

    fn publish(
        &mut self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU32,
        input: &Inputs<'_>,
    ) -> PublicationResult<Self::Bytecode, Self::Error>;

    // Err must mean not committed. A successful commit ends compensation authority.
    fn persist(
        &mut self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU32,
        input: &Inputs<'_>,
    ) -> Result<StoredProgramSummary, Self::Error>;
}

// Cleanup never requires a store, including explicit retries after load failure.
pub(crate) trait CleanupEffects: LoadCleanup {
    type MapDirectory;

    fn remove_map_directory(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::MapDirectory,
    ) -> Result<(), EffectFailure<Self::MapDirectory, Self::Error>>;
}
