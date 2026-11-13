//! The platform's closed vocabulary for observed capability outcomes.

use serde::{Deserialize, Serialize};

/// What the platform says happened to one effect.
///
/// Six words, and never a seventh. See
/// `docs/architecture/execution.md#results-and-effects` for the effect rules.
/// Each word claims only what the platform knows.
/// Before `wamn-b2m6.3`, the tree made these four invalid claims when every failure rendered as "refused".
/// These claims must never recur:
///
/// * A TIMEOUT IS NOT A ROLLBACK. The platform sent the request and got no
///   answer. What the far side did with that request is unknown, so
///   [`Self::Timeout`] states nothing about the far side.
/// * A REFUSAL BEFORE DISPATCH IS NOT AN UNDONE COMMAND. A wiring failure
///   refuses one effect. The command that reached the component committed
///   already, and [`Self::RefusedBeforeDispatch`] describes the effect alone.
/// * AN ATTEMPT THAT FAILED IN FLIGHT IS NOT A TIMEOUT. A deadline that never
///   elapsed is as false a claim as the other two, one word earlier. That
///   attempt is [`Self::EffectUncertain`] (owner ruling, `wamn-b2m6.3`).
/// * A LOST RESPONSE IS NEITHER UNCERTAIN NOR DELIVERED. The far side acted and
///   the answer confirms it, so nothing is unknown. The guest received no
///   response, so nothing was delivered. That is [`Self::ResponseLost`], and it
///   is a word because its remedy is its own (owner ruling, `wamn-b2m6.3`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EffectOutcome {
    /// The platform declined the effect and sent nothing.
    RefusedBeforeDispatch,
    /// The far side answered. A refusal inside that answer is still an answer.
    Responded,
    /// The request went out and the deadline elapsed with no answer.
    Timeout,
    /// The effect was abandoned before it settled.
    Cancelled,
    /// The platform sent an attempt and recorded no outcome for it.
    ///
    /// The same state the premium durable shelf contract in
    /// `docs/architecture/execution.md#results-and-effects` describes.
    /// The two use one vocabulary for this fact.
    EffectUncertain,
    /// The far side acted and its response did not arrive.
    ///
    /// The status line and headers confirm that the far side acted. The body did not
    /// reach the guest. The remedy is to RE-READ the result, never to resend
    /// the request, which is what separates this word from
    /// [`Self::EffectUncertain`].
    ResponseLost,
}

impl EffectOutcome {
    /// The complete effect vocabulary, in its stable declaration order.
    pub const ALL: [Self; 6] = [
        Self::RefusedBeforeDispatch,
        Self::Responded,
        Self::Timeout,
        Self::Cancelled,
        Self::EffectUncertain,
        Self::ResponseLost,
    ];

    /// The frozen attribute value one outcome is recorded as.
    ///
    /// A trace reader matches these six strings, so a rename breaks every
    /// saved query written against them.
    pub const fn label(self) -> &'static str {
        match self {
            Self::RefusedBeforeDispatch => "refused-before-dispatch",
            Self::Responded => "responded",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::EffectUncertain => "effect-uncertain",
            Self::ResponseLost => "response-lost",
        }
    }
}

impl std::fmt::Display for EffectOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::EffectOutcome;

    #[test]
    fn serialized_outcomes_and_schema_labels_keep_the_frozen_vocabulary() {
        let expected = [
            "refused-before-dispatch",
            "responded",
            "timeout",
            "cancelled",
            "effect-uncertain",
            "response-lost",
        ];
        assert_eq!(EffectOutcome::ALL.map(EffectOutcome::label), expected);
        for (outcome, label) in EffectOutcome::ALL.into_iter().zip(expected) {
            let wire = serde_json::Value::String(label.to_owned());
            assert_eq!(serde_json::to_value(outcome).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<EffectOutcome>(wire).unwrap(),
                outcome
            );
        }
        assert!(serde_json::from_str::<EffectOutcome>("\"failed\"").is_err());
        assert!(serde_json::from_str::<EffectOutcome>("\"rolled-back\"").is_err());
    }
}
