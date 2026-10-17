//! Resume interrupted response reads on the same connection. Never replay a
//! request: a mutating response may contain the only upload ownership receipt.
//! ureq's unversioned transport API is deliberately confined to this module.
use std::{io, time::Instant};
use ureq::unversioned::{
    resolver::DefaultResolver,
    transport::{
        Buffers, ConnectionDetails, Connector, DefaultConnector, NextTimeout, Transport,
        time::Duration,
    },
};

pub(super) fn agent(config: ureq::config::Config) -> ureq::Agent {
    ureq::Agent::with_parts(
        config,
        DefaultConnector::default().chain(ResumeReads),
        DefaultResolver::default(),
    )
}

#[derive(Debug)]
struct ResumeReads;

#[derive(Debug)]
struct Reader<T>(T);

impl<T: Transport> Connector<T> for ResumeReads {
    type Out = Reader<T>;

    fn connect(
        &self,
        _: &ConnectionDetails<'_>,
        chained: Option<T>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        Ok(chained.map(Reader))
    }
}

impl<T: Transport> Transport for Reader<T> {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.0.buffers()
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        self.0.transmit_output(amount, timeout)
    }

    fn await_input(&mut self, mut timeout: NextTimeout) -> Result<bool, ureq::Error> {
        let started = Instant::now();
        let budget = timeout.after;
        loop {
            match self.0.await_input(timeout) {
                Err(ureq::Error::Io(cause)) if cause.kind() == io::ErrorKind::Interrupted => {
                    if let Duration::Exact(budget) = budget {
                        let remaining = budget.saturating_sub(started.elapsed());
                        if remaining.is_zero() {
                            return Err(ureq::Error::Timeout(timeout.reason));
                        }
                        timeout.after = Duration::Exact(remaining);
                    }
                }
                result => return result,
            }
        }
    }

    fn is_open(&mut self) -> bool {
        self.0.is_open()
    }

    fn is_tls(&self) -> bool {
        self.0.is_tls()
    }
}
