use std::collections::{BTreeMap, HashMap};

use bpfman_shell_ir::{BlockId, CollectMode, LoopBindings, LoopId, Temp};

use bpfman_shell_source::Span;

use super::{Machine, Slot, Value};

pub(super) struct Iteration {
    owner: LoopId,
    span: Span,
    items: std::vec::IntoIter<Value>,
    names: Vec<String>,
    frame_depth: usize,
    body: BlockId,
    exit: BlockId,
    collection: Option<Collection>,
}

pub(super) struct Collection {
    target: String,
    mode: CollectMode,
    ok: bool,
    values: Vec<Value>,
    results: Vec<Value>,
}

impl Collection {
    pub(super) fn new(target: String, mode: CollectMode) -> Self {
        Self {
            target,
            mode,
            ok: true,
            values: Vec::new(),
            results: Vec::new(),
        }
    }

    fn finish(self) -> (String, Value) {
        let values = Value::List(self.values);
        let value = match self.mode {
            CollectMode::Guard => values,
            CollectMode::Capture => Value::Record(BTreeMap::from([
                ("ok".to_owned(), Value::Bool(self.ok)),
                ("values".to_owned(), values),
                ("results".to_owned(), Value::List(self.results)),
            ])),
        };
        (self.target, value)
    }
}

impl Machine<'_> {
    pub(super) fn start_loop(
        &mut self,
        list: Temp,
        bindings: &LoopBindings,
        body: BlockId,
        exit: BlockId,
        collection: Option<Collection>,
        span: &Span,
    ) -> Result<(), String> {
        let Value::List(items) = self.value(list)?.clone() else {
            return Err("foreach expects a list".to_owned());
        };
        let owner = LoopId(self.block);
        self.step_iteration(
            Iteration {
                owner,
                span: span.clone(),
                items: items.into_iter(),
                names: bindings.names().to_vec(),
                frame_depth: self.frames.len(),
                body,
                exit,
                collection,
            },
            false,
        )
    }

    pub(super) fn advance_loop(&mut self, owner: LoopId, exiting: bool) -> Result<(), String> {
        let state = self
            .loops
            .pop()
            .filter(|state| state.owner == owner)
            .ok_or("loop owner is not active")?;
        self.step_iteration(state, exiting)
    }

    fn step_iteration(&mut self, mut state: Iteration, exiting: bool) -> Result<(), String> {
        self.frames.truncate(state.frame_depth);
        if let Some(value) = (!exiting).then(|| state.items.next()).flatten() {
            self.frames.push(HashMap::new());
            match state.names.as_slice() {
                [name] => {
                    if self.trace_enabled {
                        self.trace_value(&state.span, format!("foreach {name} = "), &value);
                    }
                    self.bind(name, value)?;
                }
                names => {
                    self.trace_bindings(&state.span, "foreach", names, &value);
                    self.destructure(names, value)?;
                }
            }
            self.jump(state.body);
            self.loops.push(state);
        } else {
            if let Some(collection) = state.collection {
                let (name, value) = collection.finish();
                self.bind(&name, value)?;
            }
            self.jump(state.exit);
        }
        Ok(())
    }

    pub(super) fn destructure(&mut self, names: &[String], value: Value) -> Result<(), String> {
        let Value::List(items) = value else {
            return Err(format!("expected a list to bind {} names", names.len()));
        };
        if items.len() != names.len() {
            return Err(format!(
                "list has {} elements, cannot bind {} names",
                items.len(),
                names.len()
            ));
        }
        for (name, item) in names.iter().zip(items) {
            self.bind(name, item)?;
        }
        Ok(())
    }

    pub(super) fn collect_result(&mut self, owner: LoopId, result: Temp) -> Result<(), String> {
        let Some(Slot::Result(response)) = self.temps.get(result.0).and_then(Option::as_ref) else {
            return Err(format!("expected command result at t{}", result.0));
        };
        let collection = self
            .loops
            .last_mut()
            .filter(|state| state.owner == owner)
            .and_then(|state| state.collection.as_mut())
            .ok_or("collecting loop owner is not active")?;
        let outcome = Value::Outcome(Box::new(response.clone()));
        if response.exit_code == 0 {
            collection
                .values
                .push(if response.primary == Value::Absent {
                    outcome.clone()
                } else {
                    response.primary.clone()
                });
        } else {
            collection.ok = false;
            if collection.mode == CollectMode::Guard {
                return Err(format!(
                    "guard {}: command failed (exit {})",
                    collection.target, response.exit_code
                ));
            }
        }
        collection.results.push(outcome);
        self.advance_loop(owner, false)
    }
}
