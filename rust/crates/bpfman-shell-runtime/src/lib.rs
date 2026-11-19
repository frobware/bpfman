//! Pure shell execution driven by typed observations from a caller.

mod machine;
mod value;

pub use machine::{
    ClockContinuation, CommandContinuation, CommandRequest, CommandResponse, DispatchLane,
    ExecutionError, OutputContinuation, OutputRequest, OutputStream, PathContinuation, PathRequest,
    PauseContinuation, QueryContinuation, QueryInput, QueryRequest, Step, execute,
};
pub use value::{Argument, JobHandle, KfuncHandle, Number, Value};
