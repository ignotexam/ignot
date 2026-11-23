//! Money, in whole numbers: what an answer cost, and how many tokens the same budget buys from each model.
//!
//! Costs and budgets are counted in billionths of a US dollar and token prices in trillionths, so no sum here
//! ever rounds.

use serde::{Deserialize, Serialize};

use crate::{Result, invalid};

/// The two ways a model sits the exam.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Track {
    /// Every model gets the same dollars per question.
    SameBudget,
    /// Every model may think as long as it likes, up to a fixed ceiling of tokens.
    Unlimited,
}

impl Track {
    /// Both tracks, in the order they are listed everywhere.
    pub const ALL: [Self; 2] = [Self::SameBudget, Self::Unlimited];

    /// The name used in files and on the command line.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::SameBudget => "same-budget",
            Self::Unlimited => "unlimited",
        }
    }
}

/// Decimals of a dollar in a cost or a budget (billionths).
pub const COST_DECIMALS: u32 = 9;
/// Decimals of a dollar in a token price (trillionths).
pub const PRICE_DECIMALS: u32 = 12;

/// The token ceiling on the unlimited track.
pub const UNLIMITED_CAP: u32 = 32_000;
/// No request asks for more output tokens than this, whatever the budget would buy.
const HARD_CAP: u32 = 65_536;
/// Below this many output tokens a model cannot give a useful answer, so the question is not attempted.
const MIN_USEFUL: u32 = 256;

/// What a model charges, in trillionths of a dollar per token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pricing {
    /// Price of one prompt token.
    pub prompt: u64,
    /// Price of one prompt token that the host writes to its cache, when that is dearer: some hosts cache long
    /// prompts without being asked and bill the writing. Zero when the model lists no such price.
    pub cache_write: u64,
    /// Price of one completion token.
    pub completion: u64,
    /// What a request costs before any token is counted. Most models charge nothing here.
    pub request: u64,
    /// The most completion tokens the model can produce, when its provider says so.
    pub max_completion: Option<u32>,
}

/// A decimal amount such as `0.02` as a whole number of units with `decimals` decimals. Exact or an error.
pub fn parse_amount(text: &str, decimals: u32) -> Result<u64> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let digits = |part: &str| part.bytes().all(|c| c.is_ascii_digit());
    if whole.is_empty() || !digits(whole) || !digits(fraction) || (fraction.is_empty() && text.contains('.')) {
        return invalid(format!("\"{text}\" is not an amount like 0.02"));
    }
    let kept = fraction.trim_end_matches('0');
    if kept.len() > decimals as usize {
        return invalid(format!("\"{text}\" has more than {decimals} decimals"));
    }
    let mut units: u64 = 0;
    for c in whole.bytes().chain(kept.bytes().chain(std::iter::repeat(b'0')).take(decimals as usize)) {
        units = match units.checked_mul(10).and_then(|n| n.checked_add(u64::from(c - b'0'))) {
            Some(n) => n,
            None => return invalid(format!("\"{text}\" is too large")),
        };
    }
    Ok(units)
}

/// Units with `decimals` decimals as a decimal amount, without trailing zeros: the reverse of [`parse_amount`].
#[must_use]
pub fn format_amount(units: u64, decimals: u32) -> String {
    let scale = 10u64.pow(decimals);
    let fraction = format!("{:0width$}", units % scale, width = decimals as usize);
    let fraction = fraction.trim_end_matches('0');
    if fraction.is_empty() { (units / scale).to_string() } else { format!("{}.{fraction}", units / scale) }
}

/// Tokens that the wrapping of a chat request adds to its text: the roles, and the marks between messages.
const WRAPPING_TOKENS: u64 = 64;

/// The most tokens a prompt of this many bytes can be.
///
/// No tokenizer makes more than one token of a byte, so this is a ceiling, not a guess. Text with many digits
/// and signs comes close to it (1.1 bytes a token in practice); prose is three to four bytes a token. A guess
/// in between would give a model more tokens than its budget pays for whenever the text is denser than the
/// guess, and a right answer would then cost too much through no fault of the model.
#[must_use]
pub fn most_prompt_tokens(bytes: usize) -> u64 {
    (bytes as u64).saturating_add(WRAPPING_TOKENS)
}

/// Output tokens a model may spend on one question when every model gets the same budget.
///
/// The prompt is charged at the most it can cost, every token of it at the dearer of the prompt and the
/// cache-writing price, and the fee per request comes off first, so prompt and answer together never pass the
/// budget at the listed prices. Zero means the budget does not buy a useful answer from this model: the
/// question counts as not answered.
#[must_use]
pub fn same_budget_max_tokens(budget: u64, prompt_bytes: usize, pricing: &Pricing) -> u32 {
    let cap = pricing.max_completion.filter(|&n| n > 0).map_or(HARD_CAP, |n| n.min(HARD_CAP));
    let scale = u128::from(10u64.pow(PRICE_DECIMALS - COST_DECIMALS));
    let per_prompt_token = pricing.prompt.max(pricing.cache_write);
    let prompt_cost = u128::from(most_prompt_tokens(prompt_bytes)) * u128::from(per_prompt_token);
    let left =
        (u128::from(budget) * scale).checked_sub(prompt_cost).and_then(|n| n.checked_sub(u128::from(pricing.request)));
    let tokens = match left {
        // The prompt and the fee alone cost more than the budget, even when the answer is free.
        None => return 0,
        Some(_) if pricing.completion == 0 => cap,
        Some(left) => u32::try_from(left / u128::from(pricing.completion)).unwrap_or(u32::MAX).min(cap),
    };
    if tokens >= MIN_USEFUL { tokens } else { 0 }
}

/// Output tokens a model may spend on one question on the unlimited track.
#[must_use]
pub fn unlimited_max_tokens(pricing: &Pricing) -> u32 {
    pricing.max_completion.filter(|&n| n > 0).map_or(UNLIMITED_CAP, |n| n.min(UNLIMITED_CAP))
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn price(prompt: u64, completion: u64, max: Option<u32>) -> Pricing {
        Pricing { prompt, cache_write: 0, completion, request: 0, max_completion: max }
    }

    #[test]
    fn amounts_parse_exactly() {
        assert_eq!(parse_amount("0.02", COST_DECIMALS).unwrap(), 20_000_000);
        assert_eq!(parse_amount("1", COST_DECIMALS).unwrap(), 1_000_000_000);
        assert_eq!(parse_amount("0.000000001", COST_DECIMALS).unwrap(), 1);
        assert_eq!(parse_amount("0.0000025", PRICE_DECIMALS).unwrap(), 2_500_000);
        assert_eq!(parse_amount("0.0200000000000", COST_DECIMALS).unwrap(), 20_000_000);
        assert_eq!(parse_amount("0", COST_DECIMALS).unwrap(), 0);
        for bad in ["", ".5", "5.", "-1", "+1", "1e3", "0.0000000001", "1,5", " 1", "99999999999999999999"] {
            assert!(parse_amount(bad, COST_DECIMALS).is_err(), "{bad}");
        }
    }

    #[test]
    fn amounts_print_and_parse_back() {
        for (units, text) in
            [(20_000_000, "0.02"), (1_000_000_000, "1"), (1, "0.000000001"), (0, "0"), (1_234_500_000, "1.2345")]
        {
            assert_eq!(format_amount(units, COST_DECIMALS), text);
            assert_eq!(parse_amount(text, COST_DECIMALS).unwrap(), units);
        }
    }

    #[test]
    fn the_prompt_is_charged_at_the_most_it_can_cost() {
        assert_eq!(most_prompt_tokens(0), 64);
        assert_eq!(most_prompt_tokens(1000), 1064);
        assert_eq!(most_prompt_tokens(usize::MAX), u64::MAX);
    }

    #[test]
    fn the_same_dollars_buy_fewer_tokens_from_a_dearer_model() {
        // $0.02 per question and a prompt of 1000 bytes, charged as 1064 tokens.
        let budget = 20_000_000;
        // $3 and $15 per million tokens: (0.02 - 1064 * 0.000003) / 0.000015 = 1120.5
        assert_eq!(same_budget_max_tokens(budget, 1000, &price(3_000_000, 15_000_000, None)), 1120);
        // $0.10 and $0.40 per million tokens: 49734 tokens
        assert_eq!(same_budget_max_tokens(budget, 1000, &price(100_000, 400_000, None)), 49_734);
        // The same, from a model that stops at 8192 tokens.
        assert_eq!(same_budget_max_tokens(budget, 1000, &price(100_000, 400_000, Some(8192))), 8192);
    }

    #[test]
    fn whatever_the_text_the_bill_stays_within_the_budget() {
        // The worst case: every byte of the prompt a token, the wrapping too, and every allowed token used.
        for (budget, bytes, prompt, completion) in [
            (20_000_000u64, 13_386usize, 1_250_000u64, 10_000_000u64),
            (20_000_000, 2_087, 5_000_000, 25_000_000),
            (1_000_000, 300, 49_800, 199_200),
        ] {
            let allowed = u128::from(same_budget_max_tokens(budget, bytes, &price(prompt, completion, None)));
            let bill = (bytes as u128 + 64) * u128::from(prompt) + allowed * u128::from(completion);
            assert!(bill <= u128::from(budget) * 1000, "{budget} {bytes} {prompt} {completion}");
        }
    }

    #[test]
    fn a_fee_per_request_comes_out_of_the_budget_first() {
        // $0.02, less a fee of $0.01 a request, less the prompt, at $0.40 per million output tokens.
        let fee = Pricing { request: 10_000_000_000, ..price(100_000, 400_000, None) };
        assert_eq!(same_budget_max_tokens(20_000_000, 1000, &fee), 24_734);
        let dear = Pricing { request: 20_000_000_000, ..price(100_000, 400_000, None) };
        assert_eq!(same_budget_max_tokens(20_000_000, 1000, &dear), 0);
    }

    #[test]
    fn a_host_that_bills_writing_its_cache_is_reserved_for() {
        // As a host that writes every prompt token to its cache at $0.125 per million instead of $0.10 bills it.
        let writes = Pricing { cache_write: 125_000, ..price(100_000, 400_000, None) };
        let allowed = same_budget_max_tokens(20_000_000, 1000, &writes);
        assert_eq!(allowed, 49_667);
        assert!(1064 * 125_000 + u128::from(allowed) * 400_000 <= 20_000_000 * 1000);
        // A cheaper cache price changes nothing.
        let reads = Pricing { cache_write: 50_000, ..price(100_000, 400_000, None) };
        assert_eq!(same_budget_max_tokens(20_000_000, 1000, &reads), 49_734);
    }

    #[test]
    fn free_output_still_has_to_leave_the_prompt_and_the_fee_inside_the_budget() {
        // $0.04 per million prompt tokens and output for nothing, as two models of the real list charge.
        let free = price(40_000, 0, None);
        assert_eq!(same_budget_max_tokens(20_000_000, 13_000, &free), HARD_CAP);
        // At $0.0001 a question the 13,064 prompt tokens alone cost more.
        assert_eq!(same_budget_max_tokens(100_000, 13_000, &free), 0);
        let fee = Pricing { request: 30_000_000_000, ..free };
        assert_eq!(same_budget_max_tokens(20_000_000, 1000, &fee), 0);
        // Exactly at the budget is inside it.
        assert_eq!(same_budget_max_tokens(1064 * 40, 1000, &free), HARD_CAP);
    }

    #[test]
    fn a_budget_that_buys_no_useful_answer_buys_nothing() {
        let budget = 20_000_000;
        // $75 per million output tokens leaves 224 tokens: under the useful minimum.
        assert_eq!(same_budget_max_tokens(budget, 1000, &price(3_000_000, 75_000_000, None)), 0);
        // The prompt alone costs more than the budget.
        assert_eq!(same_budget_max_tokens(budget, 1_000_000, &price(100_000_000, 15_000_000, None)), 0);
        assert_eq!(same_budget_max_tokens(0, 10, &price(1, 1, None)), 0);
    }

    #[test]
    fn free_models_and_huge_budgets_stop_at_the_ceiling() {
        assert_eq!(same_budget_max_tokens(0, 1000, &price(0, 0, None)), HARD_CAP);
        assert_eq!(same_budget_max_tokens(0, 1000, &price(0, 0, Some(4096))), 4096);
        assert_eq!(same_budget_max_tokens(u64::MAX, usize::MAX, &price(0, 1, Some(0))), HARD_CAP);
        assert_eq!(same_budget_max_tokens(u64::MAX, usize::MAX, &price(u64::MAX, u64::MAX, None)), 0);
    }

    #[test]
    fn the_unlimited_track_has_a_ceiling_too() {
        assert_eq!(unlimited_max_tokens(&price(1, 1, None)), UNLIMITED_CAP);
        assert_eq!(unlimited_max_tokens(&price(1, 1, Some(8192))), 8192);
        assert_eq!(unlimited_max_tokens(&price(1, 1, Some(200_000))), UNLIMITED_CAP);
        assert_eq!(unlimited_max_tokens(&price(1, 1, Some(0))), UNLIMITED_CAP);
    }

    #[test]
    fn track_names_match_the_files() {
        for track in Track::ALL {
            assert_eq!(serde_json::to_string(&track).unwrap(), format!("\"{}\"", track.name()));
        }
        assert!(Track::SameBudget < Track::Unlimited);
    }
}
