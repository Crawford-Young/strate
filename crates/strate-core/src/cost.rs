//! Cost accounting: a port of the cost half of claude-config's
//! `scripts/audit-lib.mjs` (`priceFor`, `priceUsage`, `depthOf`) over a
//! byte-for-byte copy of its `prices.json` (`data/prices.json`, checked
//! against upstream in CI), plus strate's context-window table
//! (`data/context-windows.json`).
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

/// $ per million tokens for one model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelPrice {
    pub input: f64,
    pub output: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
    pub cache_read: f64,
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
        let models = json["models"]
            .as_object()?
            .iter()
            .map(|(id, p)| {
                let rate = |key: &str| p[key].as_f64();
                Some((
                    id.clone(),
                    ModelPrice {
                        input: rate("input")?,
                        output: rate("output")?,
                        cache_write_5m: rate("cacheWrite5m")?,
                        cache_write_1h: rate("cacheWrite1h")?,
                        cache_read: rate("cacheRead")?,
                    },
                ))
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
    /// (never folded in as $0). Same operation order as audit-lib, so a
    /// request's $ matches it bit for bit.
    pub fn price(&self, usage: &Usage, model: &str) -> Option<f64> {
        let p = self.model(model)?;
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
        assert_eq!(haiku.input, 1.0);
        // Not 8 digits after the base id: no match.
        assert_eq!(p.model("claude-haiku-4-5-2025"), None);
        assert_eq!(p.model("claude-haiku-4-5-preview"), None);
        // claude-opus-5-5 is its own entry, not a snapshot of claude-opus-5.
        assert_eq!(p.model("claude-opus-5-5").expect("opus 5.5").input, 4.0);
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
        assert_eq!(context_window("claude-haiku-4-5-20251001"), Some(200_000));
        assert_eq!(context_window("claude-lorem-1"), None);
        // 200006 / 1000000 = 20.0006%
        let pct = context_pct(200_006, "claude-opus-5-5").expect("known");
        assert!((pct - 20.0006).abs() < 1e-9, "{pct}");
        // 50008 / 200000 = 25.004%
        let pct = context_pct(50_008, "claude-haiku-4-5-20251001").expect("known");
        assert!((pct - 25.004).abs() < 1e-9, "{pct}");
        assert_eq!(context_pct(1000, "claude-lorem-1"), None);
        assert_eq!(context_pct(1000, SYNTHETIC), None);
    }

    #[test]
    fn the_windows_table_carries_its_verification() {
        let table: Value = serde_json::from_str(WINDOWS_JSON).expect("json");
        assert_eq!(table["verified"], "2026-10-06");
        assert!(
            table["source"]
                .as_str()
                .is_some_and(|s| s.contains("max_input_tokens"))
        );
    }
}
