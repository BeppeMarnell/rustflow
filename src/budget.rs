use thiserror::Error;

use crate::flow::{default_max_prompt_bytes, FlowDefinition};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reservation {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub spend_microusd: i64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelPrices {
    pub input_usd_per_million: f64,
    pub output_usd_per_million: f64,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BudgetError {
    #[error("model token prices must be configured as finite nonnegative numbers")]
    MissingOrInvalidPrices,
    #[error("model alias has no budget policy: {0}")]
    UnknownModel(String),
    #[error("invalid model node budget configuration")]
    InvalidConfig,
    #[error("run budget reservation overflow")]
    Overflow,
}

impl ModelPrices {
    pub fn parse(input: Option<&str>, output: Option<&str>) -> Result<Self, BudgetError> {
        let parse = |value: Option<&str>| {
            value
                .and_then(|raw| raw.parse::<f64>().ok())
                .filter(|rate| rate.is_finite() && (0.0..=1000.0).contains(rate))
                .ok_or(BudgetError::MissingOrInvalidPrices)
        };
        Ok(Self {
            input_usd_per_million: parse(input)?,
            output_usd_per_million: parse(output)?,
        })
    }

    pub fn from_env() -> Result<Self, BudgetError> {
        let input = std::env::var("MODEL_INPUT_USD_PER_MILLION").ok();
        let output = std::env::var("MODEL_OUTPUT_USD_PER_MILLION").ok();
        let legacy_input = std::env::var("OPENAI_INPUT_USD_PER_MILLION").ok();
        let legacy_output = std::env::var("OPENAI_OUTPUT_USD_PER_MILLION").ok();
        Self::from_config(
            input.as_deref(),
            output.as_deref(),
            legacy_input.as_deref(),
            legacy_output.as_deref(),
        )
    }

    fn from_config(
        input: Option<&str>,
        output: Option<&str>,
        legacy_input: Option<&str>,
        legacy_output: Option<&str>,
    ) -> Result<Self, BudgetError> {
        if input.is_some() || output.is_some() {
            Self::parse(input, output)
        } else {
            Self::parse(legacy_input, legacy_output)
        }
    }
}

fn micro_cost(tokens: i64, rate: f64) -> Result<i64, BudgetError> {
    let cost = (tokens as f64 * rate).ceil();
    if !cost.is_finite() || cost > i64::MAX as f64 {
        return Err(BudgetError::Overflow);
    }
    Ok(cost as i64)
}

pub fn reserve(
    flow: &FlowDefinition,
    prices: Option<ModelPrices>,
) -> Result<Reservation, BudgetError> {
    let mut reservation = Reservation {
        input_tokens: 0,
        output_tokens: 0,
        spend_microusd: 0,
    };
    for node in flow.nodes.iter().filter(|node| node.kind == "llm.chat") {
        let alias = node
            .config
            .get("model_alias")
            .and_then(serde_json::Value::as_str)
            .ok_or(BudgetError::InvalidConfig)?;
        let prompt_bytes = node
            .config
            .get("max_prompt_bytes")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(i64::from(default_max_prompt_bytes()));
        let output_tokens = node
            .config
            .get("max_output_tokens")
            .and_then(serde_json::Value::as_i64)
            .ok_or(BudgetError::InvalidConfig)?;
        if !(1..=65_536).contains(&prompt_bytes) || !(1..=4096).contains(&output_tokens) {
            return Err(BudgetError::InvalidConfig);
        }
        let attempts = i64::from(node.retry.max_attempts) + 1;
        // One input token cannot encode less than one UTF-8 byte of prompt content.
        // The extra 128 covers the fixed single-message chat envelope.
        let input = (prompt_bytes + 128)
            .checked_mul(attempts)
            .ok_or(BudgetError::Overflow)?;
        let output = output_tokens
            .checked_mul(attempts)
            .ok_or(BudgetError::Overflow)?;
        reservation.input_tokens = reservation
            .input_tokens
            .checked_add(input)
            .ok_or(BudgetError::Overflow)?;
        reservation.output_tokens = reservation
            .output_tokens
            .checked_add(output)
            .ok_or(BudgetError::Overflow)?;
        let cost = match alias {
            "mock" => 0,
            "openai" => {
                let prices = prices.ok_or(BudgetError::MissingOrInvalidPrices)?;
                micro_cost(input, prices.input_usd_per_million)?
                    .checked_add(micro_cost(output, prices.output_usd_per_million)?)
                    .ok_or(BudgetError::Overflow)?
            }
            other => return Err(BudgetError::UnknownModel(other.to_owned())),
        };
        reservation.spend_microusd = reservation
            .spend_microusd
            .checked_add(cost)
            .ok_or(BudgetError::Overflow)?;
    }
    Ok(reservation)
}

#[cfg(test)]
mod tests {
    use super::{reserve, BudgetError, ModelPrices, Reservation};
    use crate::flow::FlowDefinition;
    use serde_json::json;

    fn flow() -> FlowDefinition {
        serde_json::from_str(include_str!("../examples/basic-chat.json")).unwrap()
    }

    #[test]
    fn reserves_every_attempt_and_prompt_byte_limit() {
        let mut flow = flow();
        let model = flow
            .nodes
            .iter_mut()
            .find(|node| node.id == "model")
            .unwrap();
        model.config["max_prompt_bytes"] = json!(4096);
        model.retry.max_attempts = 2;
        assert_eq!(
            reserve(&flow, None).unwrap(),
            Reservation {
                input_tokens: (4096 + 128) * 3,
                output_tokens: 512 * 3,
                spend_microusd: 0,
            }
        );
    }

    #[test]
    fn prices_are_required_and_reserved_conservatively() {
        let mut flow = flow();
        let model = flow
            .nodes
            .iter_mut()
            .find(|node| node.id == "model")
            .unwrap();
        model.config["model_alias"] = json!("openai");
        model.config["max_prompt_bytes"] = json!(1000);
        model.retry.max_attempts = 0;
        assert_eq!(
            reserve(&flow, None),
            Err(BudgetError::MissingOrInvalidPrices)
        );
        let prices = ModelPrices::parse(Some("2.5"), Some("10")).unwrap();
        assert_eq!(reserve(&flow, Some(prices)).unwrap().spend_microusd, 7940);
        for bad in [None, Some("NaN"), Some("-1"), Some("1001")] {
            assert_eq!(
                ModelPrices::parse(bad, Some("1")).unwrap_err(),
                BudgetError::MissingOrInvalidPrices
            );
        }
    }

    #[test]
    fn model_price_pair_takes_precedence_without_mixing_legacy_values() {
        let prices = ModelPrices::from_config(Some("3"), Some("9"), Some("1"), Some("2")).unwrap();
        assert_eq!(prices.input_usd_per_million, 3.0);
        assert_eq!(prices.output_usd_per_million, 9.0);
        assert_eq!(
            ModelPrices::from_config(Some("3"), None, Some("1"), Some("2")),
            Err(BudgetError::MissingOrInvalidPrices)
        );
        let legacy = ModelPrices::from_config(None, None, Some("1"), Some("2")).unwrap();
        assert_eq!(legacy.input_usd_per_million, 1.0);
    }
}
