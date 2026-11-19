//! Pure shell execution driven by typed observations from a caller.

mod machine;
mod value;

pub use machine::{
    CommandContinuation, CommandRequest, CommandResponse, DispatchLane, ExecutionError,
    OutputContinuation, OutputRequest, Step, execute,
};
pub use value::{Argument, Number, Value};
