/// Signals accepted by the shell's job control.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobSignal {
    /// Terminate.
    Term,
    /// Kill immediately.
    Kill,
    /// Interrupt.
    Int,
    /// Quit.
    Quit,
    /// Hang up.
    Hup,
    /// User signal 1.
    Usr1,
    /// User signal 2.
    Usr2,
    /// Suspend.
    Stop,
    /// Continue.
    Cont,
}

impl JobSignal {
    /// Supported signals in canonical listing order.
    pub const ALL: [Self; 9] = [
        Self::Term,
        Self::Kill,
        Self::Int,
        Self::Quit,
        Self::Hup,
        Self::Usr1,
        Self::Usr2,
        Self::Stop,
        Self::Cont,
    ];

    /// Accept a case-insensitive name with an optional SIG prefix.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let upper = text.trim().to_ascii_uppercase();
        let name = upper.strip_prefix("SIG").unwrap_or(&upper);
        Self::ALL.into_iter().find(|signal| signal.name() == name)
    }

    /// Canonical name used in shell command envelopes.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Term => "TERM",
            Self::Kill => "KILL",
            Self::Int => "INT",
            Self::Quit => "QUIT",
            Self::Hup => "HUP",
            Self::Usr1 => "USR1",
            Self::Usr2 => "USR2",
            Self::Stop => "STOP",
            Self::Cont => "CONT",
        }
    }
}
