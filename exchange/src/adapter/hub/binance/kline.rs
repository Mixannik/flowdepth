//! Shared, deterministic conversion of REST and WebSocket candle payloads.
use super::AdapterError;
use crate::{
    Kline, Qty, TickerInfo, Volume,
    adapter::MarketKind,
    unit::qty::{QtyNormalization, QuantityUnit, RawQtyUnit},
};

pub(super) struct BinanceCandle {
    pub time: u64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    // Spot/linear: base quantity. Inverse: contract count.
    pub native_total: f64,
    pub native_buy: f64,
    // Spot/linear: quote turnover. Inverse: base quantity (NOT USD).
    pub secondary_total: f64,
    pub secondary_buy: f64,
}

impl BinanceCandle {
    pub fn into_kline(self, ticker: TickerInfo, unit: QuantityUnit) -> Result<Kline, AdapterError> {
        let (total, buy) = match ticker.market_type() {
            MarketKind::InversePerps => {
                let normalizer =
                    QtyNormalization::with_raw_qty_unit(true, ticker, RawQtyUnit::Contracts);
                (
                    normalizer.normalize(self.native_total, self.close),
                    normalizer.normalize(self.native_buy, self.close),
                )
            }
            MarketKind::Spot | MarketKind::LinearPerps => match unit {
                QuantityUnit::Base => (self.native_total, self.native_buy),
                QuantityUnit::Quote => (self.secondary_total, self.secondary_buy),
            },
        };
        let max = i64::MAX as f64 / 10f64.powi(Qty::QTY_SCALE);
        if !total.is_finite()
            || !buy.is_finite()
            || total < 0.0
            || buy < 0.0
            || buy > total
            || total > max
        {
            return Err(AdapterError::ParseError(
                "Invalid Binance candle side volumes".into(),
            ));
        }
        let total = Qty::from_f64(total);
        let buy = Qty::from_f64(buy);
        // Subtract after rounding so buy + sell equals the stored total exactly.
        Ok(Kline::new(
            self.time,
            self.open,
            self.high,
            self.low,
            self.close,
            Volume::BuySell(buy, total - buy),
            ticker.min_ticksize,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::super::{fetch::FetchedKline, stream::SonicKline};
    use super::*;
    use crate::{Ticker, adapter::Exchange};

    fn rest() -> FetchedKline {
        serde_json::from_value(serde_json::json!([
            60_000, "100", "120", "100", "110", "3.5", 119_999, "395", 3, "1.5", "155", "0"
        ]))
        .unwrap()
    }

    fn ws() -> SonicKline {
        serde_json::from_value(serde_json::json!({
            "t":60_000,"o":"100","h":"120","l":"100","c":"110","v":"3.5","V":"1.5","q":"395","Q":"155","i":"1m"
        })).unwrap()
    }

    #[test]
    fn rest_and_ws_match_executions_at_different_prices_in_both_units() {
        // 1 buy @100, 2 sell @120, 0.5 buy @110; close=110.
        for exchange in [Exchange::BinanceSpot, Exchange::BinanceLinear] {
            let ticker = TickerInfo::new(Ticker::new("BTCUSDT", exchange), 0.01, 0.001, None);
            for unit in [QuantityUnit::Base, QuantityUnit::Quote] {
                let rest = rest().into_candle().into_kline(ticker, unit).unwrap();
                let ws = ws().into_candle().into_kline(ticker, unit).unwrap();
                let normalizer = QtyNormalization::with_raw_qty_unit(
                    unit == QuantityUnit::Quote,
                    ticker,
                    RawQtyUnit::Base,
                );
                let expected_buy =
                    normalizer.normalize_qty(1.0, 100.0) + normalizer.normalize_qty(0.5, 110.0);
                let expected_sell = normalizer.normalize_qty(2.0, 120.0);
                assert_eq!(rest.volume, Volume::BuySell(expected_buy, expected_sell));
                assert_eq!(
                    serde_json::to_value(rest).unwrap(),
                    serde_json::to_value(ws).unwrap()
                );
                if unit == QuantityUnit::Quote {
                    assert_eq!(rest.volume.total(), Qty::from_f64(395.0));
                    assert_ne!(rest.volume.total(), Qty::from_f64(3.5 * 110.0));
                }
            }
        }
    }

    #[test]
    fn inverse_contracts_are_quote_valued_without_using_secondary_base_volume() {
        let ticker = TickerInfo::new(
            Ticker::new("BTCUSD_PERP", Exchange::BinanceInverse),
            0.1,
            1.0,
            Some(100.0),
        );
        for unit in [QuantityUnit::Base, QuantityUnit::Quote] {
            let rest: FetchedKline = serde_json::from_value(serde_json::json!([
                60_000,
                "9640.7",
                "9642.4",
                "9640.6",
                "9642.0",
                "206",
                119_999,
                "2.13660389",
                48,
                "119",
                "1.23424865",
                "0"
            ]))
            .unwrap();
            let ws: SonicKline = serde_json::from_value(serde_json::json!({
                "t":60_000,"o":"9640.7","h":"9642.4","l":"9640.6","c":"9642.0","v":"206","V":"119","q":"2.13660389","Q":"1.23424865","i":"1m"
            })).unwrap();
            let rest = rest.into_candle().into_kline(ticker, unit).unwrap();
            let ws = ws.into_candle().into_kline(ticker, unit).unwrap();
            assert_eq!(
                rest.volume,
                Volume::BuySell(Qty::from_f64(11900.0), Qty::from_f64(8700.0))
            );
            assert_eq!(rest.volume, ws.volume);
        }
    }

    #[test]
    fn invalid_side_volumes_are_rejected_and_rounding_preserves_total() {
        let ticker = TickerInfo::new(
            Ticker::new("BTCUSDT", Exchange::BinanceSpot),
            0.01,
            0.001,
            None,
        );
        for (total, buy) in [
            (1.0, 2.0),
            (-1.0, 0.0),
            (1.0, -1.0),
            (f64::NAN, 0.0),
            (f64::INFINITY, 0.0),
            (1.0, f64::NAN),
            (1e20, 0.0),
        ] {
            let mut candle = rest().into_candle();
            candle.secondary_total = total;
            candle.secondary_buy = buy;
            assert!(candle.into_kline(ticker, QuantityUnit::Quote).is_err());
        }
        for (total, buy) in [(0.0, 0.0), (1e-8, 5e-9)] {
            let mut candle = rest().into_candle();
            candle.secondary_total = total;
            candle.secondary_buy = buy;
            let volume = candle
                .into_kline(ticker, QuantityUnit::Quote)
                .unwrap()
                .volume;
            assert_eq!(volume.total(), Qty::from_f64(total));
            assert!(volume.sell_qty().unwrap() >= Qty::ZERO);
        }
    }

    #[test]
    fn missing_quote_fields_are_not_deserialized_as_zero() {
        let payload = serde_json::json!({"t":60_000,"o":"100","h":"120","l":"100","c":"110","v":"3.5","V":"1.5","i":"1m"});
        assert!(serde_json::from_value::<SonicKline>(payload).is_err());
    }
}
