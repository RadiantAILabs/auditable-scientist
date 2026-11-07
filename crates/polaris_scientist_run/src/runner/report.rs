//! Turns what one agent session did into the report the workflow reads.
//!
//! Everything here is a pure function of the turn's outcome and the call's own
//! meter, so the mapping from an ending to a [`StopCause`] can be read, and
//! tested, without a server.

use polaris_code::run::{TurnOutcome, TurnReason};
use polaris_code::usage::RunUsage;
use polaris_cosmos::flow::{AgentFailure, Micros, StopCause, Usage};

/// Why the runner asked the turn to stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Why {
    /// The call's time limit ran out.
    Timeout,
    /// The campaign asked every call to stop.
    Stop,
}

impl Why {
    fn cause(self) -> StopCause {
        match self {
            Self::Timeout => StopCause::Timeout,
            Self::Stop => StopCause::Interrupted,
        }
    }
}

/// How the turn came to an end.
pub(super) enum Ended<'a> {
    /// The turn returned an outcome.
    Turn(&'a TurnOutcome),
    /// The turn did not stop when asked and was dropped.
    Abandoned,
}

/// The text the executor gives when a loop node runs more times than the
/// session's turn limit allows, for a limit of `max_turns`. The runner's tests
/// pin it, so a change in the executor's wording fails there.
fn turn_cap_message(max_turns: u32) -> String {
    format!("max iterations ({max_turns}) exceeded on loop node")
}

/// The start of the refusal the call's meter gives a request that could take
/// the call past its spend limit.
const COST_CAP_PREFIX: &str = "cost-cap:";

/// The cause to report for a turn that ended as `ended`.
///
/// The order is the contract. An ending the runner asked for — the time limit
/// or the campaign's stop — is named for the request, whether the turn then
/// reported itself interrupted or failed while unwinding. Otherwise a spend
/// refusal outranks any other failure, because the refusal is what stopped the
/// call; then the turn limit; then any other failure is a model error. A turn
/// that finished on its own is `Finished` even if a request to stop arrived in
/// the same instant.
///
/// # Errors
///
/// [`AgentFailure::Internal`] for an ending a call never reaches here: the
/// runner starts no coding topology and sets no run ceiling.
pub(super) fn stop_cause(
    ended: Ended<'_>,
    why: Option<Why>,
    cost_capped: bool,
    max_turns: u32,
) -> Result<StopCause, AgentFailure> {
    let outcome = match ended {
        Ended::Abandoned => return Ok(why.map_or(StopCause::Interrupted, Why::cause)),
        Ended::Turn(outcome) => outcome,
    };
    match &outcome.reason {
        TurnReason::Completed => Ok(StopCause::Finished),
        TurnReason::CostCapped => Ok(StopCause::CostCap),
        TurnReason::Interrupted => Ok(why.map_or(StopCause::Interrupted, Why::cause)),
        TurnReason::Failed(failure) => {
            let message = failure.message.as_str();
            if let Some(why) = why {
                Ok(why.cause())
            } else if cost_capped || message.trim_start().starts_with(COST_CAP_PREFIX) {
                Ok(StopCause::CostCap)
            } else if message.contains(&turn_cap_message(max_turns)) {
                Ok(StopCause::TurnCap)
            } else {
                Ok(StopCause::ModelError(format!(
                    "{}: {message}",
                    failure.kind.label()
                )))
            }
        }
        reason @ (TurnReason::NoTopologyStarted | TurnReason::CeilingReached) => {
            Err(AgentFailure::Internal(format!(
                "the session ended in a way a call cannot: {reason:?}"
            )))
        }
    }
}

/// Whole micro-dollars to charge for `usd`, rounded up.
///
/// A fraction of a micro-dollar is never dropped: the campaign's ledger would
/// then total less than was spent. A rate such as $2 per million tokens gives
/// whole micro-dollars per token, but the sum of float products can land a hair
/// above the exact figure, and that hair must not cost an extra micro-dollar.
/// Prices have at most two decimals, so a real fraction is a multiple of
/// 0.01 micro-dollars and is far larger than the tolerance.
#[must_use]
pub(super) fn micros_up(usd: f64) -> Micros {
    let micros = usd * 1_000_000.0 - 1e-4;
    if micros <= 0.0 || !micros.is_finite() {
        return 0;
    }
    // The cast cannot lose a value a run could reach: it saturates.
    micros.ceil() as Micros
}

/// Dollars for a whole number of micro-dollars, for the call's meter limit.
#[must_use]
pub(super) fn usd_of(micros: Micros) -> f64 {
    micros as f64 / 1_000_000.0
}

/// The workflow's usage figures from a meter snapshot.
///
/// `spent` is what the call's spend limit counted as spent, in dollars. It is
/// the larger of the two figures that is reported: the tokens of finished
/// requests are priced exactly, but a request cut off while in flight left no
/// token count behind and is charged at its estimate by the spend limit alone.
///
/// # Errors
///
/// [`AgentFailure::Unmeasured`] when the model has no price, so the spend
/// cannot be stated at all.
pub(super) fn usage_of(snapshot: &RunUsage, spent: Option<f64>) -> Result<Usage, AgentFailure> {
    let Some(priced) = snapshot.estimated_cost_usd else {
        return Err(AgentFailure::Unmeasured(format!(
            "no price is known for model {}, so the call's spend cannot be measured",
            snapshot.model_id
        )));
    };
    let cost = spent
        .filter(|spent| spent.is_finite())
        .map_or(priced, |spent| spent.max(priced));
    Ok(Usage {
        // Left empty so the spend ledger names the provider from its settings.
        provider: String::new(),
        model_id: snapshot.model_id.clone(),
        input: snapshot.input_tokens,
        output: snapshot.output_tokens,
        cache_read: snapshot.cache_read_tokens,
        cache_write: snapshot.cache_creation_tokens,
        requests: u32::try_from(snapshot.num_turns).unwrap_or(u32::MAX),
        micros: micros_up(cost),
    })
}

/// Refuses a finished call whose provider reported no tokens at all.
///
/// A call that ended some other way may have spent nothing — a refused first
/// request bills nothing — but a call that finished must have been billed.
///
/// # Errors
///
/// [`AgentFailure::Unmeasured`] when `stop` is `Finished` and the snapshot
/// holds no tokens.
pub(super) fn require_usage(stop: &StopCause, snapshot: &RunUsage) -> Result<(), AgentFailure> {
    if matches!(stop, StopCause::Finished) && snapshot.total_tokens == 0 {
        return Err(AgentFailure::Unmeasured(
            "the call finished but the model reported no token usage".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn micros_round_up_and_ignore_float_noise() {
        assert_eq!(micros_up(0.0), 0);
        assert_eq!(micros_up(0.0002), 200);
        // 0.1 + 0.2 is slightly above 0.3 in floating point.
        assert_eq!(micros_up(0.1 + 0.2), 300_000);
        assert_eq!(micros_up(0.000_000_3), 1);
        assert_eq!(micros_up(13.5), 13_500_000);
        assert_eq!(micros_up(-1.0), 0);
        assert_eq!(micros_up(f64::NAN), 0);
    }

    #[test]
    fn a_cut_off_request_is_charged_at_its_estimate() {
        use polaris::models::pricing::PriceStore;
        use polaris_code::usage::TokenMeter;

        let meter = TokenMeter::new().with_pricing_store(PriceStore::builtin());
        meter.add(100, 10);
        let snapshot = meter.snapshot("anthropic/claude-sonnet-5-5");
        // $2 per million input tokens and $10 per million output tokens.
        assert_eq!(usage_of(&snapshot, None).unwrap().micros, 300);
        assert_eq!(usage_of(&snapshot, Some(0.0003)).unwrap().micros, 300);
        assert_eq!(usage_of(&snapshot, Some(0.0125)).unwrap().micros, 12_500);
        assert_eq!(usage_of(&snapshot, Some(f64::NAN)).unwrap().micros, 300);
    }

    #[test]
    fn the_turn_cap_message_names_the_limit() {
        assert_eq!(
            turn_cap_message(2),
            "max iterations (2) exceeded on loop node"
        );
    }

    #[test]
    fn whole_micros_convert_to_the_same_dollars() {
        assert_eq!(micros_up(usd_of(13_500_000)), 13_500_000);
        assert_eq!(micros_up(usd_of(1)), 1);
        assert_eq!(micros_up(usd_of(123_456_789)), 123_456_789);
    }
}
