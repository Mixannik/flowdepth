//! Shared lifecycle and cache for per-market iceberg detector engines.

use data::orderflow::iceberg::{BinanceIcebergDetector, IcebergDetectorConfig, IcebergEvent};
use exchange::{TickerInfo, UnixMs, orderflow::OrderFlowEvent, unit::PriceStep};
use rustc_hash::{FxHashMap, FxHashSet};

const RELEASE_GRACE_MS: u64 = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DetectorKey {
    pub ticker_info: TickerInfo,
    pub tick_size: PriceStep,
}

struct SharedDetector {
    engine: BinanceIcebergDetector,
    consumers: usize,
    released_at: Option<UnixMs>,
}

#[derive(Default)]
pub struct IcebergDetectorRegistry {
    detectors: FxHashMap<DetectorKey, SharedDetector>,
    integrity_blocked: FxHashSet<exchange::Ticker>,
}

impl IcebergDetectorRegistry {
    /// Keep old confirmed events, but discard in-flight episodes and prevent a
    /// later Healthy/snapshot event from silently re-enabling a damaged source.
    pub fn set_integrity_blocks(&mut self, tickers: &[TickerInfo], at: UnixMs) {
        let blocked: FxHashSet<_> = tickers.iter().map(|info| info.ticker).collect();
        for (key, shared) in &mut self.detectors {
            if blocked.contains(&key.ticker_info.ticker)
                && !self.integrity_blocked.contains(&key.ticker_info.ticker)
            {
                shared.engine.ingest(OrderFlowEvent::Quality {
                    ticker_info: key.ticker_info,
                    at,
                    quality: exchange::orderflow::OrderFlowDataQuality::Gap,
                });
            }
        }
        self.integrity_blocked = blocked;
    }

    pub fn sync_requirements(
        &mut self,
        requirements: Vec<(DetectorKey, IcebergDetectorConfig)>,
        now: UnixMs,
    ) {
        let mut desired: FxHashMap<DetectorKey, (usize, IcebergDetectorConfig)> =
            FxHashMap::default();
        for (key, config) in requirements
            .into_iter()
            .filter(|(_, config)| config.enabled)
        {
            let entry = desired.entry(key).or_insert((0, config));
            entry.0 = entry.0.saturating_add(1);
        }
        for (key, shared) in &mut self.detectors {
            if let Some((count, _)) = desired.remove(key) {
                shared.consumers = count;
                shared.released_at = None;
            } else if shared.consumers > 0 {
                shared.consumers = 0;
                shared.released_at = Some(now);
            }
        }
        for (key, (count, config)) in desired {
            match BinanceIcebergDetector::new(key.ticker_info, key.tick_size, config) {
                Ok(engine) => {
                    self.detectors.insert(
                        key,
                        SharedDetector {
                            engine,
                            consumers: count,
                            released_at: None,
                        },
                    );
                    log::info!(
                        "IcebergDetectorStarted | exchange={} ticker={} consumers={count}",
                        key.ticker_info.exchange(),
                        key.ticker_info.ticker
                    );
                }
                Err(reason) => log::debug!(
                    "IcebergDetectorUnsupported | ticker={} reason={reason}",
                    key.ticker_info.ticker
                ),
            }
        }
    }

    pub fn ingest(&mut self, event: OrderFlowEvent) -> Vec<IcebergEvent> {
        let ticker_info = event.ticker_info();
        if self.integrity_blocked.contains(&ticker_info.ticker) {
            return Vec::new();
        }
        self.detectors
            .iter_mut()
            .filter(|(key, shared)| key.ticker_info == ticker_info && shared.consumers > 0)
            .flat_map(|(_, shared)| shared.engine.ingest(event.clone()))
            .collect()
    }

    pub fn collect_garbage(&mut self, now: UnixMs) {
        self.detectors.retain(|key, shared| {
            let keep = shared.consumers > 0
                || shared
                    .released_at
                    .is_some_and(|released| now.saturating_diff(released) < RELEASE_GRACE_MS);
            if !keep {
                log::info!(
                    "IcebergDetectorStopped | exchange={} ticker={}",
                    key.ticker_info.exchange(),
                    key.ticker_info.ticker
                );
            }
            keep
        });
    }

    #[cfg(test)]
    fn detector_count(&self) -> usize {
        self.detectors.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use exchange::{Ticker, adapter::Exchange};

    fn key() -> DetectorKey {
        let ticker_info = TickerInfo::new(
            Ticker::new("BTCUSDT", Exchange::BinanceLinear),
            0.1,
            0.001,
            None,
        );
        DetectorKey {
            ticker_info,
            tick_size: ticker_info.min_ticksize.into(),
        }
    }

    #[test]
    fn ws_integrity_blocks_healthy_and_new_detector_instances_but_not_other_markets() {
        use exchange::orderflow::{AggressorSide, NormalizedTradeEvent, OrderFlowDataQuality};
        let btc = key();
        let mut eth = key();
        eth.ticker_info.ticker = Ticker::new("ETHUSDT", Exchange::BinanceLinear);
        let config = IcebergDetectorConfig {
            enabled: true,
            ..Default::default()
        };
        let mut registry = IcebergDetectorRegistry::default();
        registry.sync_requirements(vec![(btc, config), (eth, config)], UnixMs::new(100));
        let trade = |key: DetectorKey, id: u64| {
            OrderFlowEvent::Trade(NormalizedTradeEvent {
                ticker_info: key.ticker_info,
                event_time: UnixMs::new(100),
                trade_time: UnixMs::new(100),
                receive_time: UnixMs::new(100),
                trade_id: id,
                price: exchange::unit::Price::from_f64(100.0),
                quantity: exchange::unit::Qty::from_f64(1.0),
                aggressor: AggressorSide::Buy,
            })
        };
        registry.ingest(trade(btc, 1));
        registry.ingest(trade(eth, 1));
        assert_eq!(registry.detectors[&btc].engine.buffered_count(), 1);
        let retained = registry.detectors[&btc].engine.retained_events().len();
        registry.set_integrity_blocks(&[btc.ticker_info], UnixMs::new(110));
        assert_eq!(registry.detectors[&btc].engine.buffered_count(), 0);
        assert_eq!(registry.detectors[&eth].engine.buffered_count(), 1);
        assert_eq!(
            registry.detectors[&btc].engine.retained_events().len(),
            retained
        );
        assert!(
            registry
                .ingest(OrderFlowEvent::Quality {
                    ticker_info: btc.ticker_info,
                    at: UnixMs::new(120),
                    quality: OrderFlowDataQuality::Healthy
                })
                .is_empty()
        );
        assert!(registry.ingest(trade(btc, 2)).is_empty());
        assert_eq!(registry.detectors[&btc].engine.buffered_count(), 0);
        registry.sync_requirements(vec![(eth, config)], UnixMs::new(200));
        registry.collect_garbage(UnixMs::new(6000));
        registry.sync_requirements(vec![(btc, config), (eth, config)], UnixMs::new(6001));
        assert!(registry.ingest(trade(btc, 3)).is_empty());
        assert_eq!(registry.detectors[&btc].engine.buffered_count(), 0);
    }

    #[test]
    fn two_consumers_share_one_engine_and_last_release_expires_it() {
        let mut registry = IcebergDetectorRegistry::default();
        let config = IcebergDetectorConfig {
            enabled: true,
            ..Default::default()
        };
        registry.sync_requirements(vec![(key(), config), (key(), config)], UnixMs::new(1_000));
        assert_eq!(registry.detector_count(), 1);
        registry.sync_requirements(vec![(key(), config)], UnixMs::new(1_000));
        registry.collect_garbage(UnixMs::new(7_000));
        assert_eq!(registry.detector_count(), 1);
        registry.sync_requirements(Vec::new(), UnixMs::new(7_000));
        registry.collect_garbage(UnixMs::new(12_001));
        assert_eq!(registry.detector_count(), 0);
    }
}
