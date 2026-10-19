//! Cost accounting: a port of the cost half of claude-config's
//! `scripts/audit-lib.mjs` (`priceFor`, `priceUsage` with its `longPrompt`
//! tier, `depthOf`) over a byte-for-byte copy of its `prices.json`
//! (`data/prices.json`, checked against upstream in CI), plus strate's
//! context-window table (`data/context-windows.json`).
//!
//! Every request is priced on its own; deduplication (one row per
//! `requestId`, the record with the most output) is the store's job.

use std::sync::OnceLock;

use serde_json::Value;

/// The vendored price list, verbatim.
pub const PRICES_JSON: &str = include_str!("../data/prices.json");
const WINDOWS_JSON: &str = include_str!("../data/context-windows.json");
const MTOK: f64 = 1e6;

/// The model Claude Code writes on records it made up itself (an
/// interrupted turn, an API error). It is never priced or attributed.
pub const SYNTHETIC: &str = "<synthetic>";

/// $ per million tokens, per token class.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rates {
    pub input: f64,
    pub output: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
    pub cache_read: f64,
}

/// One model's prices: its base rates, and the tier that replaces them
/// for a request whose prompt is over `over` tokens (`longPrompt`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelPrice {
    pub base: Rates,
    pub long_prompt: Option<LongPrompt>,
}

/// A model's `longPrompt` tier.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LongPrompt {
    /// Prompt tokens a request must be strictly over to get these rates.
    pub over: f64,
    /// The base rates with the tier's own spread over them.
    pub rates: Rates,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Prices {
    web_search_per_1k: f64,
    models: Vec<(String, ModelPrice)>,
}

/// One record's `message.usage`, with the cache write split into its tiers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    /// `cache_creation_input_tokens`: the total write, which depth counts.
    pub cache_creation: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub web_search_requests: u64,
}

/// The vendored prices.
pub fn prices() -> &'static Prices {
    static PRICES: OnceLock<Prices> = OnceLock::new();
    PRICES.get_or_init(|| Prices::parse(PRICES_JSON).expect("data/prices.json parses"))
}

/// The model's context window in tokens; `None` for a model the table
/// does not know, which gets no context % rather than a guess.
pub fn context_window(model: &str) -> Option<u64> {
    static WINDOWS: OnceLock<Vec<(String, u64)>> = OnceLock::new();
    let table = WINDOWS.get_or_init(|| {
        let json: Value =
            serde_json::from_str(WINDOWS_JSON).expect("data/context-windows.json parses");
        json["models"]
            .as_object()
            .expect("a models object")
            .iter()
            .map(|(id, w)| (id.clone(), w.as_u64().expect("a token count")))
            .collect()
    });
    resolve(table, model).copied()
}

/// Context use in percent: `depth` over the model's window.
pub fn context_pct(depth: u64, model: &str) -> Option<f64> {
    context_window(model).map(|window| depth as f64 * 100.0 / window as f64)
}

/// The entry for `model`, else for the base id of a dated snapshot
/// (`claude-haiku-4-5-20251001` resolves to `claude-haiku-4-5`).
fn resolve<'a, T>(table: &'a [(String, T)], model: &str) -> Option<&'a T> {
    let snapshot_of = |id: &str| {
        model
            .strip_prefix(id)
            .and_then(|rest| rest.strip_prefix('-'))
            .is_some_and(|date| date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()))
    };
    table
        .iter()
        .find(|(id, _)| id == model)
        .or_else(|| table.iter().find(|(id, _)| snapshot_of(id)))
        .map(|(_, entry)| entry)
}

/// JavaScript truthiness, for audit-lib's `x ? … : …` and `x || …` reads.
pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null | Value::Bool(false) => false,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

impl Prices {
    fn parse(json: &str) -> Option<Self> {
        let json: Value = serde_json::from_str(json).ok()?;
        // A model's `exception` (a price Claude Code charges that differs
        // from the published one) is never read: it feeds only audit.mjs's
        // cost-state price check, which strate does not run, so strate
        // prices every request at the published rates.
        let models = json["models"]
            .as_object()?
            .iter()
            .map(|(id, p)| {
                let rate = |key: &str| p[key].as_f64();
                let base = Rates {
                    input: rate("input")?,
                    output: rate("output")?,
                    cache_write_5m: rate("cacheWrite5m")?,
                    cache_write_1h: rate("cacheWrite1h")?,
                    cache_read: rate("cacheRead")?,
                };
                // `{ ...base, ...longRates }`: a rate the tier omits stays
                // at base, and a tier without `over` never applies.
                let tier = &p["longPrompt"];
                let long_prompt = tier["over"].as_f64().map(|over| {
                    let rate = |key: &str, base: f64| tier[key].as_f64().unwrap_or(base);
                    LongPrompt {
                        over,
                        rates: Rates {
                            input: rate("input", base.input),
                            output: rate("output", base.output),
                            cache_write_5m: rate("cacheWrite5m", base.cache_write_5m),
                            cache_write_1h: rate("cacheWrite1h", base.cache_write_1h),
                            cache_read: rate("cacheRead", base.cache_read),
                        },
                    }
                });
                Some((id.clone(), ModelPrice { base, long_prompt }))
            })
            .collect::<Option<_>>()?;
        Some(Self {
            web_search_per_1k: json["webSearchPer1k"].as_f64().unwrap_or(0.0),
            models,
        })
    }

    /// `priceFor`: the model's prices, `None` when it has none.
    pub fn model(&self, model: &str) -> Option<&ModelPrice> {
        resolve(&self.models, model)
    }

    /// `priceUsage`: one request's $, `None` when its model is unpriced
    /// (never folded in as $0), at the long-prompt rates when its prompt is
    /// over the model's `longPrompt.over`. Same operation order as
    /// audit-lib, so a request's $ matches it bit for bit.
    pub fn price(&self, usage: &Usage, model: &str) -> Option<f64> {
        let p = self.model(model)?.rates_for(usage);
        Some(
            (usage.input as f64 * p.input
                + usage.output as f64 * p.output
                + usage.cache_read as f64 * p.cache_read
                + usage.cache_write_5m as f64 * p.cache_write_5m
                + usage.cache_write_1h as f64 * p.cache_write_1h)
                / MTOK
                + usage.web_search_requests as f64 * self.web_search_per_1k / 1000.0,
        )
    }
}

impl ModelPrice {
    /// The long-prompt rates for a request whose prompt is strictly over
    /// the tier's `over`, else the base rates.
    fn rates_for(&self, usage: &Usage) -> &Rates {
        match &self.long_prompt {
            Some(tier) if usage.prompt() as f64 > tier.over => &tier.rates,
            _ => &self.base,
        }
    }
}

impl Usage {
    /// Reads `message.usage`. Absent or non-integer counts are 0. With no
    /// `cache_creation` tier split, the whole write is 5m (the API's
    /// default TTL).
    pub fn from_json(usage: &Value) -> Self {
        let count = |v: &Value| v.as_u64().unwrap_or(0);
        let cache_creation = count(&usage["cache_creation_input_tokens"]);
        let split = &usage["cache_creation"];
        let (cache_write_5m, cache_write_1h) = if truthy(split) {
            (
                count(&split["ephemeral_5m_input_tokens"]),
                count(&split["ephemeral_1h_input_tokens"]),
            )
        } else {
            (cache_creation, 0)
        };
        Self {
            input: count(&usage["input_tokens"]),
            output: count(&usage["output_tokens"]),
            cache_read: count(&usage["cache_read_input_tokens"]),
            cache_creation,
            cache_write_5m,
            cache_write_1h,
            web_search_requests: count(&usage["server_tool_use"]["web_search_requests"]),
        }
    }

    /// `depthOf`: the context the request read, input + cache read +
    /// cache write.
    pub fn depth(&self) -> u64 {
        self.input + self.cache_read + self.cache_creation
    }

    /// The prompt audit-lib's `longPrompt` threshold reads: input + cache
    /// read + the two priced write tiers (not the write total, which a
    /// tier split may not sum to).
    fn prompt(&self) -> u64 {
        self.input + self.cache_read + self.cache_write_5m + self.cache_write_1h
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_dated_snapshot_prices_as_its_base_id_and_an_unknown_model_is_unpriced() {
        let p = prices();
        let haiku = p.model("claude-haiku-4-5").expect("haiku");
        assert_eq!(p.model("claude-haiku-4-5-20251001"), Some(haiku));
        assert_eq!(haiku.base.input, 1.0);
        // Not 8 digits after the base id: no match.
        assert_eq!(p.model("claude-haiku-4-5-2025"), None);
        assert_eq!(p.model("claude-haiku-4-5-preview"), None);
        // claude-opus-5-5 is its own entry, not a snapshot of claude-opus-5.
        assert_eq!(
            p.model("claude-opus-5-5").expect("opus 5.5").base.input,
            4.0
        );
        assert_eq!(p.model("claude-lorem-1"), None);
        assert_eq!(p.model(SYNTHETIC), None);
        let usage = Usage {
            input: 1000,
            ..Usage::default()
        };
        assert_eq!(p.price(&usage, "claude-lorem-1"), None);
    }

    #[test]
    fn a_request_is_priced_per_token_class_and_tier_plus_web_search() {
        let usage = Usage::from_json(&json!({
            "input_tokens": 50,
            "cache_creation_input_tokens": 300,
            "cache_read_input_tokens": 20000,
            "output_tokens": 60,
            "cache_creation": {"ephemeral_5m_input_tokens": 100, "ephemeral_1h_input_tokens": 200},
            "server_tool_use": {"web_search_requests": 2}
        }));
        assert_eq!(
            usage,
            Usage {
                input: 50,
                output: 60,
                cache_read: 20000,
                cache_creation: 300,
                cache_write_5m: 100,
                cache_write_1h: 200,
                web_search_requests: 2,
            }
        );
        // claude-opus-5-5 ($/MTok: input 4, output 20, read 0.2, 5m 5, 1h 8),
        // web search $10/1k:
        // (50*4 + 60*20 + 20000*0.2 + 100*5 + 200*8) / 1e6 + 2*10/1000
        // = (200 + 1200 + 4000 + 500 + 1600) / 1e6 + 0.02 = 0.0075 + 0.02
        let usd = prices().price(&usage, "claude-opus-5-5").expect("priced");
        assert!((usd - 0.0275).abs() < 1e-12, "{usd}");
    }

    #[test]
    fn a_write_without_a_tier_split_is_all_5m() {
        let usage = Usage::from_json(&json!({
            "input_tokens": 20,
            "cache_creation_input_tokens": 400,
            "cache_read_input_tokens": 30000,
            "output_tokens": 30
        }));
        assert_eq!((usage.cache_write_5m, usage.cache_write_1h), (400, 0));
        // A split that is present wins over the total, even when all zero.
        let split = Usage::from_json(&json!({
            "cache_creation_input_tokens": 400,
            "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 0}
        }));
        assert_eq!((split.cache_write_5m, split.cache_write_1h), (0, 0));
        assert_eq!(split.cache_creation, 400);
        // claude-sonnet-5 (input 2, output 10, read 0.2, 5m 2.5):
        // (20*2 + 30*10 + 30000*0.2 + 400*2.5) / 1e6 = 7340 / 1e6
        let usd = prices().price(&usage, "claude-sonnet-5").expect("priced");
        assert!((usd - 0.00734).abs() < 1e-12, "{usd}");
    }

    /// A Haiku 5.5 request whose prompt (input + cache read + both write
    /// tiers) is `input` + 90000 + 9000 + 0 tokens, and 100 output.
    fn haiku_5_5(input: u64) -> Usage {
        Usage::from_json(&json!({
            "input_tokens": input,
            "cache_creation_input_tokens": 9000,
            "cache_read_input_tokens": 90000,
            "output_tokens": 100,
            "cache_creation": {"ephemeral_5m_input_tokens": 9000, "ephemeral_1h_input_tokens": 0}
        }))
    }

    #[test]
    fn a_prompt_over_the_long_prompt_threshold_is_priced_at_the_long_prompt_rates() {
        let p = prices();
        // claude-haiku-5-5, base ($/MTok: input 0.1, output 0.5, read 0.01,
        // 5m 0.125), over 100000 prompt tokens: input 0.5, output 2.5,
        // read 0.05, 5m 0.625. At exactly 100000 the base rates hold:
        // (1000*0.1 + 100*0.5 + 90000*0.01 + 9000*0.125) / 1e6 = 2175 / 1e6
        let at = p
            .price(&haiku_5_5(1000), "claude-haiku-5-5")
            .expect("priced");
        assert!((at - 0.002175).abs() < 1e-12, "{at}");
        // 100001 is over: every token class at the long-prompt rate.
        // (1001*0.5 + 100*2.5 + 90000*0.05 + 9000*0.625) / 1e6 = 10875.5 / 1e6
        let over = p
            .price(&haiku_5_5(1001), "claude-haiku-5-5")
            .expect("priced");
        assert!((over - 0.0108755).abs() < 1e-12, "{over}");
        // A dated snapshot gets its base id's tier too.
        let snapshot = p.price(&haiku_5_5(1001), "claude-haiku-5-5-20261001");
        assert_eq!(snapshot, Some(over));
    }

    #[test]
    fn the_long_prompt_threshold_counts_the_priced_write_tiers_not_the_write_total() {
        // audit-lib's prompt is input + cache read + 5m + 1h: a tier split
        // under the total (100 unsplit) leaves this one at 99901, not over.
        let usage = Usage::from_json(&json!({
            "input_tokens": 1001,
            "cache_creation_input_tokens": 9000,
            "cache_read_input_tokens": 90000,
            "cache_creation": {"ephemeral_5m_input_tokens": 8900, "ephemeral_1h_input_tokens": 0}
        }));
        assert_eq!(usage.depth(), 100_001);
        // (1001*0.1 + 90000*0.01 + 8900*0.125) / 1e6 = 2112.6 / 1e6
        let usd = prices().price(&usage, "claude-haiku-5-5").expect("priced");
        assert!((usd - 0.0021126).abs() < 1e-12, "{usd}");
    }

    #[test]
    fn a_long_prompt_tier_replaces_only_the_rates_it_names() {
        // audit-lib spreads the tier over the base rates: a rate the tier
        // omits stays at base, and a tier without `over` never applies.
        let p = Prices::parse(
            r#"{"models": {
                "claude-lorem-1": {"input": 1, "output": 2, "cacheWrite5m": 3, "cacheWrite1h": 4,
                    "cacheRead": 5, "longPrompt": {"over": 10, "input": 100}},
                "claude-lorem-2": {"input": 1, "output": 2, "cacheWrite5m": 3, "cacheWrite1h": 4,
                    "cacheRead": 5, "longPrompt": {"input": 100}}
            }}"#,
        )
        .expect("parses");
        let usage = Usage {
            input: 11,
            output: 1,
            ..Usage::default()
        };
        // (11*100 + 1*2) / 1e6
        assert_eq!(p.price(&usage, "claude-lorem-1"), Some(1102.0 / MTOK));
        // (11*1 + 1*2) / 1e6
        assert_eq!(p.price(&usage, "claude-lorem-2"), Some(13.0 / MTOK));
    }

    #[test]
    fn sonnet_5_5_is_priced_at_its_published_cache_read_not_its_exception() {
        // prices.json's Sonnet 5.5 `exception` (cacheRead 0.2, what older
        // Claude Code charged) feeds only audit.mjs's cost-state check.
        let usage = Usage {
            cache_read: 1_000_000,
            ..Usage::default()
        };
        let usd = prices().price(&usage, "claude-sonnet-5-5").expect("priced");
        assert!((usd - 0.1).abs() < 1e-12, "{usd}");
        let table: Value = serde_json::from_str(PRICES_JSON).expect("json");
        assert_eq!(
            table["models"]["claude-sonnet-5-5"]["exception"]["price"]["cacheRead"],
            0.2
        );
    }

    #[test]
    fn absent_or_odd_counts_read_as_zero() {
        assert_eq!(Usage::from_json(&json!({})), Usage::default());
        assert_eq!(Usage::from_json(&json!("lorem")), Usage::default());
        let odd = Usage::from_json(&json!({"input_tokens": "12", "output_tokens": null}));
        assert_eq!(odd, Usage::default());
    }

    #[test]
    fn depth_is_input_plus_cache_read_plus_cache_write() {
        let usage = Usage::from_json(&json!({
            "input_tokens": 6,
            "cache_creation_input_tokens": 50000,
            "cache_read_input_tokens": 150000,
            "output_tokens": 80,
            "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 50000}
        }));
        // 6 + 150000 + 50000; output is not context.
        assert_eq!(usage.depth(), 200_006);
    }

    #[test]
    fn context_pct_divides_by_the_model_window_and_never_guesses() {
        assert_eq!(context_window("claude-opus-5-5"), Some(1_000_000));
        assert_eq!(context_window("claude-haiku-5-5"), Some(1_000_000));
        assert_eq!(context_window("claude-haiku-5-5-20261001"), Some(1_000_000));
        assert_eq!(context_window("claude-haiku-4-5-20251001"), Some(200_000));
        assert_eq!(context_window("claude-lorem-1"), None);
        // 200006 / 1000000 = 20.0006%
        let pct = context_pct(200_006, "claude-opus-5-5").expect("known");
        assert!((pct - 20.0006).abs() < 1e-9, "{pct}");
        // 50008 / 200000 = 25.004%
        let pct = context_pct(50_008, "claude-haiku-4-5-20251001").expect("known");
        assert!((pct - 25.004).abs() < 1e-9, "{pct}");
        // 250010 / 1000000 = 25.001%
        let pct = context_pct(250_010, "claude-haiku-5-5").expect("known");
        assert!((pct - 25.001).abs() < 1e-9, "{pct}");
        assert_eq!(context_pct(1000, "claude-lorem-1"), None);
        assert_eq!(context_pct(1000, SYNTHETIC), None);
    }

    #[test]
    fn the_windows_table_carries_its_verification() {
        let table: Value = serde_json::from_str(WINDOWS_JSON).expect("json");
        assert_eq!(table["verified"], "2026-10-19");
        let source = table["source"].as_str().expect("a source");
        assert!(source.contains("max_input_tokens"), "{source}");
        assert!(
            source.contains("platform.claude.com/docs/en/models/overview"),
            "{source}"
        );
    }
}
