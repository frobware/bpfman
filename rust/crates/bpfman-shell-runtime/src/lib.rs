//! Pure shell execution driven by typed observations from a caller.

mod machine;
mod value;

pub use machine::{
    ClockContinuation, CommandContinuation, CommandRequest, CommandResponse, DispatchLane,
    ExecutionError, OutputContinuation, OutputRequest, OutputStream, PauseContinuation, Step,
    execute,
};
pub use value::{Argument, Number, Value};
