//! Argument contracts that depend on a proved lexical value shape.

use bpfman_shell_semantics::OriginKind;
use bpfman_shell_syntax::{Command, ExpressionKind};

use super::Analysis;
use crate::jobs::job_reference;

impl Analysis<'_> {
    pub(super) fn check_native_argument(&mut self, command: &Command) {
        let name = match command.head().kind() {
            ExpressionKind::Word(name) | ExpressionKind::Quoted(name) => name.as_str(),
            _ => return,
        };
        if self.defs.contains_key(name) {
            return;
        }

        match name {
            "wait" | "kill" => {
                if let Some((argument, variable)) = job_reference(command) {
                    let kind = self.path_shape_readonly(variable).origin();
                    if !matches!(kind, OriginKind::Unknown | OriginKind::Job) {
                        self.issue(
                            argument.span(),
                            format!("expected a $job argument, got a {kind} value"),
                        );
                    }
                }
            }
            "net" => self.check_net_target(command),
            _ => {}
        }
    }

    fn check_net_target(&mut self, command: &Command) {
        let [subcommand, target, ..] = command.arguments() else {
            return;
        };
        let (ExpressionKind::Word(subcommand), ExpressionKind::Variable(variable)) =
            (subcommand.kind(), target.kind())
        else {
            return;
        };
        let kind = self.path_shape_readonly(variable).origin();
        use OriginKind as K;
        let message = match (subcommand.as_str(), kind) {
            (_, K::Unknown | K::NetPair)
            | ("exec" | "start", K::NetnsVethEndpoint)
            | ("release", K::NetnsVethPair) => return,
            ("exec" | "start", K::NetnsVethPair) => {
                format!(
                    "net {subcommand}: netns-veth-pair has two endpoints; use $pair.a or $pair.b"
                )
            }
            ("exec" | "start", _) => {
                format!(
                    "net {subcommand}: expected a $pair or endpoint argument, got a {kind} value"
                )
            }
            ("release", K::NetnsVethEndpoint) => {
                "net release: endpoint belongs to a netns-veth-pair; release the pair".to_owned()
            }
            ("release", _) => format!("net release: expected a $pair argument, got a {kind} value"),
            _ => return,
        };
        self.issue(target.span(), message);
    }
}
