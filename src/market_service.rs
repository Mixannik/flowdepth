//! Application-level connectivity tracking for market-data streams.

use exchange::adapter::StreamKind;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// Avoids flashing the blocking overlay for very short network hiccups while
/// still making a real disconnection visible quickly.
const OFFLINE_GRACE: Duration = Duration::from_millis(1_500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectivityPhase {
    Connecting,
    Online,
    Offline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectivityTransition {
    None,
    WentOffline,
    Restored,
}

/// Aggregates all independently managed WebSocket subscriptions into one
/// application-level connection state.
///
/// FlowSurface commonly has multiple connections (depth, trades and klines,
/// potentially across several venues). A single `Connected` event must not
/// hide the offline overlay while the other required streams are still down.
#[derive(Debug)]
pub struct MarketConnectivity {
    expected: HashSet<StreamKind>,
    connected: HashSet<StreamKind>,
    phase: ConnectivityPhase,
    incomplete_since: Option<Instant>,
    last_reason: Option<String>,
    had_online_disconnect: bool,
}

impl MarketConnectivity {
    pub fn new() -> Self {
        Self {
            expected: HashSet::new(),
            connected: HashSet::new(),
            phase: ConnectivityPhase::Connecting,
            incomplete_since: None,
            last_reason: None,
            had_online_disconnect: false,
        }
    }

    /// Keeps the tracker aligned with the streams required by the active
    /// dashboard. Removed streams cannot keep the whole application offline.
    pub fn sync_expected(
        &mut self,
        streams: &[StreamKind],
        now: Instant,
    ) -> ConnectivityTransition {
        self.expected = streams.iter().copied().collect();
        self.connected
            .retain(|stream| self.expected.contains(stream));
        self.evaluate(now)
    }

    pub fn record_connected(
        &mut self,
        streams: &[StreamKind],
        now: Instant,
    ) -> ConnectivityTransition {
        // Events can arrive before the next dashboard/tick synchronization.
        // Preserve those streams as expected so startup state remains useful.
        self.expected.extend(streams.iter().copied());
        self.connected.extend(streams.iter().copied());
        self.evaluate(now)
    }

    pub fn record_disconnected(
        &mut self,
        streams: &[StreamKind],
        reason: String,
        now: Instant,
    ) -> ConnectivityTransition {
        if self.phase == ConnectivityPhase::Online {
            self.had_online_disconnect = true;
        }
        self.expected.extend(streams.iter().copied());
        for stream in streams {
            self.connected.remove(stream);
        }
        self.last_reason = Some(reason);
        self.evaluate(now)
    }

    /// Advances the grace timer even when no new WS event is received.
    pub fn tick(&mut self, now: Instant) -> ConnectivityTransition {
        self.evaluate(now)
    }

    pub fn is_online(&self) -> bool {
        self.phase == ConnectivityPhase::Online
    }

    pub fn phase(&self) -> ConnectivityPhase {
        self.phase
    }

    pub fn overlay_visible(&self) -> bool {
        self.phase == ConnectivityPhase::Offline
    }

    pub fn connected_count(&self) -> usize {
        self.connected
            .iter()
            .filter(|stream| self.expected.contains(stream))
            .count()
    }

    pub fn expected_count(&self) -> usize {
        self.expected.len()
    }

    pub fn last_reason(&self) -> Option<&str> {
        self.last_reason.as_deref()
    }

    fn evaluate(&mut self, now: Instant) -> ConnectivityTransition {
        let complete = self.expected.is_empty() || self.expected.is_subset(&self.connected);

        if complete {
            self.incomplete_since = None;
            let restored = self.phase == ConnectivityPhase::Offline || self.had_online_disconnect;
            self.phase = ConnectivityPhase::Online;
            if restored {
                self.had_online_disconnect = false;
                self.last_reason = None;
                ConnectivityTransition::Restored
            } else {
                ConnectivityTransition::None
            }
        } else {
            let since = *self.incomplete_since.get_or_insert(now);
            let grace_elapsed = now
                .checked_duration_since(since)
                .is_some_and(|elapsed| elapsed >= OFFLINE_GRACE);

            if self.phase != ConnectivityPhase::Offline && grace_elapsed {
                self.phase = ConnectivityPhase::Offline;
                ConnectivityTransition::WentOffline
            } else {
                if self.phase != ConnectivityPhase::Offline {
                    self.phase = ConnectivityPhase::Connecting;
                }
                ConnectivityTransition::None
            }
        }
    }
}

impl Default for MarketConnectivity {
    fn default() -> Self {
        Self::new()
    }
}

/// Depth configuration changes do not repair retained book history. Market and
/// symbol identity must also survive metadata refreshes (tick/quantity steps).
pub fn same_integrity_source(a: &StreamKind, b: &StreamKind) -> bool {
    if a.ticker_info().ticker != b.ticker_info().ticker {
        return false;
    }
    match (a, b) {
        (StreamKind::Trades { .. }, StreamKind::Trades { .. })
        | (StreamKind::Depth { .. }, StreamKind::Depth { .. }) => true,
        (StreamKind::Kline { timeframe: a, .. }, StreamKind::Kline { timeframe: b, .. }) => a == b,
        _ => false,
    }
}

/// A request is tied to both the damaged source and its delivered successor,
/// the current quantity context, and the exact consumer binding. Bounds are [from,to).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryToken {
    pub request: uuid::Uuid,
    pub pane: uuid::Uuid,
    pub binding: uuid::Uuid,
    pub stream: StreamKind,
    pub revision: u64,
    pub lost_session: exchange::adapter::WsSessionId,
    pub lost_generation: exchange::adapter::WsGeneration,
    pub successor: exchange::adapter::WsSource,
    pub from: exchange::UnixMs,
    pub to: exchange::UnixMs,
    pub context: exchange::unit::quantity_context::QuantityContext,
}

impl RecoveryToken {
    pub fn same_work(&self, other: &Self) -> bool {
        self.pane == other.pane
            && self.binding == other.binding
            && self.stream == other.stream
            && self.revision == other.revision
            && self.successor == other.successor
            && self.from == other.from
            && self.to == other.to
            && self.context == other.context
    }
}

/// A latched application-level fault. Connectivity and fetch completion cannot
/// clear it: WS-03 must supply a verified recovery protocol first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrityFault {
    pub stream: StreamKind,
    pub auxiliary: bool,
    pub session: exchange::adapter::WsSessionId,
    pub generation: exchange::adapter::WsGeneration,
    pub revision: u64,
    pub first_detected_at: exchange::UnixMs,
    pub known_trade_from: Option<exchange::UnixMs>,
    pub resumed: Option<exchange::adapter::WsRecoveryEvidence>,
    pub current_books: Vec<exchange::adapter::WsRecoveryEvidence>,
}

#[derive(Default)]
pub struct MarketIntegrity {
    active: HashSet<StreamKind>,
    faults: Vec<IntegrityFault>,
    revision: u64,
    trade_watermarks: HashMap<StreamKind, exchange::UnixMs>,
    context: Option<exchange::unit::quantity_context::QuantityContext>,
    books: Vec<exchange::adapter::WsRecoveryEvidence>,
}

impl MarketIntegrity {
    pub fn sync_subscriptions(&mut self, streams: &[StreamKind]) {
        let context = exchange::unit::quantity_context::QuantityContext::capture();
        if self.context.is_some_and(|old| old != context) {
            self.trade_watermarks.clear();
            self.books.clear();
            for fault in &mut self.faults {
                fault.resumed = None;
                fault.current_books.clear();
            }
        }
        self.context = Some(context);
        self.active = streams.iter().copied().collect();
        self.books
            .retain(|proof| self.active.contains(&proof.stream));
        for fault in &mut self.faults {
            fault
                .current_books
                .retain(|proof| self.active.contains(&proof.stream));
        }
        // Removing a subscription is not evidence that its retained data healed.
        // Keep its fault so switching away and back cannot bypass quarantine.
    }

    pub fn record_loss(&mut self, loss: &exchange::adapter::WsIntegrityLoss) -> bool {
        let mut changed = false;
        let mut invalidated_books = Vec::new();
        for &stream in loss.streams.iter().filter(|s| {
            self.active
                .iter()
                .any(|active| same_integrity_source(s, active))
        }) {
            let known_from = self
                .trade_watermarks
                .iter()
                .filter(|(s, _)| same_integrity_source(s, &stream))
                .map(|(_, t)| *t)
                .min()
                .map(|last| {
                    loss.lost_events
                        .iter()
                        .filter(|e| e.streams.iter().any(|s| same_integrity_source(s, &stream)))
                        .filter_map(|e| e.first_known_utc)
                        .fold(last, |a, b| a.min(b))
                });
            let previous = self.faults.iter_mut().find(|fault| {
                same_integrity_source(&fault.stream, &stream)
                    && fault.auxiliary == loss.auxiliary
                    && (!matches!(stream, StreamKind::Depth { .. }) || fault.stream == stream)
            });
            if let Some(previous) = previous {
                if loss.session.0 < previous.session.0
                    || (loss.session == previous.session
                        && loss.generation.0 <= previous.generation.0)
                {
                    continue;
                }
                self.revision += 1;
                previous.session = loss.session;
                previous.generation = loss.generation;
                previous.revision = self.revision;
                previous.known_trade_from = previous
                    .known_trade_from
                    .zip(known_from)
                    .map(|(a, b)| a.min(b));
                previous.resumed = None;
                previous.current_books.clear();
                previous.first_detected_at = previous.first_detected_at.min(loss.detected_at);
            } else {
                self.revision += 1;
                self.faults.push(IntegrityFault {
                    stream,
                    auxiliary: loss.auxiliary,
                    session: loss.session,
                    generation: loss.generation,
                    revision: self.revision,
                    first_detected_at: loss.detected_at,
                    known_trade_from: known_from,
                    resumed: None,
                    current_books: Vec::new(),
                });
            }
            invalidated_books.push(stream);
            changed = true;
        }
        if changed {
            self.books
                .retain(|book| !invalidated_books.contains(&book.stream));
            for fault in &mut self.faults {
                if matches!(fault.stream, StreamKind::Depth { .. }) {
                    fault.current_books = self
                        .books
                        .iter()
                        .filter(|book| same_integrity_source(&book.stream, &fault.stream))
                        .cloned()
                        .collect();
                }
            }
        }
        changed
    }

    pub fn observe_trades(&mut self, stream: StreamKind, trades: &[exchange::Trade]) {
        if let Some(last) = trades.iter().map(|t| t.time).max() {
            let value = self.trade_watermarks.entry(stream).or_insert(last);
            *value = (*value).max(last);
        }
    }

    pub fn watermark(&self, stream: &StreamKind) -> Option<exchange::UnixMs> {
        self.trade_watermarks
            .iter()
            .filter(|(s, _)| same_integrity_source(s, stream))
            .map(|(_, t)| *t)
            .max()
    }

    pub fn record_evidence(&mut self, evidence: &exchange::adapter::WsRecoveryEvidence) -> bool {
        use exchange::adapter::RecoveryProof;
        if evidence.source.auxiliary || !self.active.contains(&evidence.stream) {
            return false;
        }
        if let RecoveryProof::BookSynchronized { at, sequence } = evidence.proof {
            if at.as_u64() == 0
                || sequence == 0
                || !matches!(evidence.stream, StreamKind::Depth { .. })
            {
                return false;
            }
            if self.faults.iter().any(|fault| {
                !fault.auxiliary
                    && fault.stream == evidence.stream
                    && (evidence.source.session.0, evidence.source.generation.0)
                        <= (fault.session.0, fault.generation.0)
            }) {
                return false;
            }
            let slot = self
                .books
                .iter_mut()
                .find(|old| old.stream == evidence.stream);
            if let Some(old) = slot {
                if (old.source.session.0, old.source.generation.0)
                    >= (evidence.source.session.0, evidence.source.generation.0)
                {
                    return false;
                }
                *old = evidence.clone();
            } else {
                self.books.push(evidence.clone());
            }
            for fault in self
                .faults
                .iter_mut()
                .filter(|f| !f.auxiliary && same_integrity_source(&f.stream, &evidence.stream))
            {
                fault
                    .current_books
                    .retain(|old| old.stream != evidence.stream);
                fault.current_books.push(evidence.clone());
            }
            return true;
        }
        let RecoveryProof::TradesResumed { first, last } = evidence.proof else {
            return false;
        };
        if first > last || !matches!(evidence.stream, StreamKind::Trades { .. }) {
            return false;
        }
        let Some(fault) = self
            .faults
            .iter_mut()
            .find(|f| !f.auxiliary && same_integrity_source(&f.stream, &evidence.stream))
        else {
            return false;
        };
        if (evidence.source.session.0, evidence.source.generation.0)
            <= (fault.session.0, fault.generation.0)
            || fault.resumed.as_ref().is_some_and(|old| {
                (old.source.session.0, old.source.generation.0)
                    >= (evidence.source.session.0, evidence.source.generation.0)
            })
        {
            return false;
        }
        fault.resumed = Some(evidence.clone());
        true
    }

    pub fn token_is_current(&self, token: &RecoveryToken) -> bool {
        token.context.is_current() && token.from < token.to
            && self.active.iter().any(|s| same_integrity_source(s, &token.stream))
            && self.watermark(&token.stream).is_some_and(|last| last >= token.to)
            && self.faults.iter().any(|f| !f.auxiliary && f.revision == token.revision
                && same_integrity_source(&f.stream, &token.stream)
                && f.session == token.lost_session && f.generation == token.lost_generation
                && f.known_trade_from.is_some_and(|from| token.from <= from && from < token.to)
                && f.resumed.as_ref().is_some_and(|e| e.source == token.successor
                    && matches!(e.proof, exchange::adapter::RecoveryProof::TradesResumed { last, .. } if last < token.to)))
    }

    pub fn faults(&self) -> &[IntegrityFault] {
        &self.faults
    }

    pub fn blocked_detector_tickers(&self) -> Vec<exchange::TickerInfo> {
        let mut tickers = Vec::new();
        for fault in &self.faults {
            if !fault.auxiliary
                && !matches!(fault.stream, StreamKind::Kline { .. })
                && !tickers.contains(&fault.stream.ticker_info())
            {
                tickers.push(fault.stream.ticker_info());
            }
        }
        tickers
    }
}

#[cfg(test)]
mod ws_integrity_tests {
    use super::*;
    use exchange::{
        Ticker, TickerInfo, UnixMs,
        adapter::{Exchange, WsGeneration, WsIntegrityLoss, WsSessionId},
    };
    use std::sync::Arc;

    fn stream(symbol: &str) -> StreamKind {
        StreamKind::Trades {
            ticker_info: TickerInfo::new(
                Ticker::new(symbol, Exchange::BinanceLinear),
                0.1,
                0.001,
                None,
            ),
        }
    }
    fn loss(
        streams: &[StreamKind],
        session: u64,
        generation: u64,
        auxiliary: bool,
    ) -> WsIntegrityLoss {
        WsIntegrityLoss {
            session: WsSessionId(session),
            generation: WsGeneration(generation),
            streams: Arc::from(streams),
            auxiliary,
            detected_at: UnixMs::new(100),
            lost_events: Vec::new(),
            complete_loss_extent_known: false,
        }
    }

    #[test]
    fn depth_settings_and_metadata_refresh_do_not_create_a_trusted_source_and_candle_loss_does_not_block_iceberg()
     {
        let mut info = stream("BTCUSDT").ticker_info();
        let depth = StreamKind::Depth {
            ticker_info: info,
            depth_aggr: exchange::adapter::StreamTicksize::Client,
            push_freq: exchange::PushFrequency::ServerDefault,
        };
        info.min_qty = exchange::unit::MinQtySize::from(0.01);
        let changed = StreamKind::Depth {
            ticker_info: info,
            depth_aggr: exchange::adapter::StreamTicksize::Client,
            push_freq: exchange::PushFrequency::Custom(exchange::Timeframe::MS100),
        };
        let mut integrity = MarketIntegrity::default();
        integrity.sync_subscriptions(&[changed]);
        assert!(integrity.record_loss(&loss(&[depth], 1, 1, false)));
        assert!(same_integrity_source(
            &integrity.faults()[0].stream,
            &changed
        ));
        let kline = StreamKind::Kline {
            ticker_info: info,
            timeframe: exchange::Timeframe::M1,
        };
        let mut candles = MarketIntegrity::default();
        candles.sync_subscriptions(&[kline]);
        assert!(candles.record_loss(&loss(&[kline], 2, 1, false)));
        assert!(candles.blocked_detector_tickers().is_empty());
        assert!(!same_integrity_source(&depth, &kline));
    }

    #[test]
    fn symbols_and_auxiliary_are_isolated_and_removal_does_not_heal_data() {
        let btc = stream("BTCUSDT");
        let eth = stream("ETHUSDT");
        let mut integrity = MarketIntegrity::default();
        integrity.sync_subscriptions(&[btc, eth]);
        assert!(integrity.record_loss(&loss(&[btc], 1, 1, true)));
        assert!(integrity.blocked_detector_tickers().is_empty());
        assert!(integrity.record_loss(&loss(&[eth], 2, 1, false)));
        assert_eq!(
            integrity.blocked_detector_tickers(),
            vec![eth.ticker_info()]
        );
        integrity.sync_subscriptions(&[eth]);
        assert!(!integrity.record_loss(&loss(&[btc], 3, 1, false)));
        integrity.sync_subscriptions(&[btc, eth]);
        assert_eq!(integrity.faults().len(), 2);
        assert!(
            integrity
                .faults()
                .iter()
                .any(|f| f.stream == btc && f.auxiliary)
        );
    }

    #[test]
    fn repeated_overflow_advances_fence_but_old_sessions_generations_do_not() {
        let btc = stream("BTCUSDT");
        let mut integrity = MarketIntegrity::default();
        integrity.sync_subscriptions(&[btc]);
        assert!(integrity.record_loss(&loss(&[btc], 7, 2, false)));
        let old_fence = integrity.faults()[0].revision;
        assert!(!integrity.record_loss(&loss(&[btc], 7, 1, false)));
        assert!(!integrity.record_loss(&loss(&[btc], 6, 9, false)));
        assert!(!integrity.record_loss(&loss(&[btc], 7, 2, false)));
        assert_eq!(integrity.faults()[0].revision, old_fence);
        assert!(integrity.record_loss(&loss(&[btc], 7, 3, false)));
        assert!(integrity.faults()[0].revision > old_fence);
        assert!(integrity.record_loss(&loss(&[btc], 8, 1, false)));
        assert_eq!(integrity.faults()[0].session, WsSessionId(8));
    }
}

#[cfg(test)]
mod ws03_tests {
    use super::*;
    use exchange::{
        Ticker, TickerInfo, Trade, UnixMs,
        adapter::{
            Exchange, RecoveryProof, WsGeneration, WsIntegrityLoss, WsRecoveryEvidence,
            WsSessionId, WsSource,
        },
    };
    fn stream() -> StreamKind {
        StreamKind::Trades {
            ticker_info: TickerInfo::new(
                Ticker::new("BTCUSDT", Exchange::BinanceSpot),
                0.1,
                0.001,
                None,
            ),
        }
    }
    fn trade(at: u64) -> Trade {
        Trade {
            id: Some(at),
            time: UnixMs::new(at),
            is_sell: false,
            price: exchange::unit::Price::from_f64(100.0),
            qty: exchange::unit::Qty::from_f64(1.0),
        }
    }
    fn loss(stream: StreamKind, generation: u64) -> WsIntegrityLoss {
        WsIntegrityLoss {
            session: WsSessionId(7),
            generation: WsGeneration(generation),
            streams: std::sync::Arc::from([stream]),
            auxiliary: false,
            detected_at: UnixMs::new(1000),
            lost_events: vec![],
            complete_loss_extent_known: false,
        }
    }
    fn evidence(stream: StreamKind, generation: u64) -> WsRecoveryEvidence {
        WsRecoveryEvidence {
            source: WsSource {
                session: WsSessionId(7),
                generation: WsGeneration(generation),
                auxiliary: false,
            },
            stream,
            proof: RecoveryProof::TradesResumed {
                first: UnixMs::new(1500),
                last: UnixMs::new(2000),
            },
        }
    }
    #[test]
    fn ws03_token_rejects_repeat_overflow_stale_generation_and_source_change() {
        let stream = stream();
        let mut integrity = MarketIntegrity::default();
        integrity.sync_subscriptions(&[stream]);
        integrity.observe_trades(stream, &[trade(1000)]);
        assert!(integrity.record_loss(&loss(stream, 2)));
        assert!(!integrity.record_evidence(&evidence(stream, 2)));
        assert!(integrity.record_evidence(&evidence(stream, 3)));
        integrity.observe_trades(stream, &[trade(300000)]);
        let token = RecoveryToken {
            request: uuid::Uuid::new_v4(),
            pane: uuid::Uuid::new_v4(),
            binding: uuid::Uuid::new_v4(),
            stream,
            revision: integrity.faults()[0].revision,
            lost_session: WsSessionId(7),
            lost_generation: WsGeneration(2),
            successor: evidence(stream, 3).source,
            from: UnixMs::ZERO,
            to: UnixMs::new(300000),
            context: exchange::unit::quantity_context::QuantityContext::capture(),
        };
        assert!(integrity.token_is_current(&token));
        assert!(integrity.record_evidence(&evidence(stream, 4)));
        assert!(!integrity.token_is_current(&token));
        let mut token = token;
        token.successor = evidence(stream, 4).source;
        assert!(integrity.token_is_current(&token));
        assert!(integrity.record_loss(&loss(stream, 4)));
        assert!(!integrity.token_is_current(&token));
        assert!(integrity.faults()[0].resumed.is_none());
        integrity.sync_subscriptions(&[]);
        assert!(!integrity.token_is_current(&token));
    }
    #[test]
    fn ws03_independent_depth_scopes_never_share_current_book_confirmation() {
        let a = StreamKind::Depth {
            ticker_info: stream().ticker_info(),
            depth_aggr: Default::default(),
            push_freq: Default::default(),
        };
        let b = StreamKind::Depth {
            ticker_info: stream().ticker_info(),
            depth_aggr: Default::default(),
            push_freq: exchange::PushFrequency::Custom(exchange::Timeframe::MS100),
        };
        let mut integrity = MarketIntegrity::default();
        integrity.sync_subscriptions(&[a, b]);
        let mut pb = evidence(b, 1);
        pb.source.session = WsSessionId(5);
        pb.proof = RecoveryProof::BookSynchronized {
            at: UnixMs::new(1000),
            sequence: 1,
        };
        assert!(integrity.record_evidence(&pb));
        integrity.record_loss(&loss(a, 2));
        assert_eq!(integrity.faults()[0].current_books[0].stream, b);
        let mut pa = evidence(a, 3);
        pa.proof = RecoveryProof::BookSynchronized {
            at: UnixMs::new(2000),
            sequence: 2,
        };
        assert!(integrity.record_evidence(&pa));
        assert_eq!(integrity.faults()[0].current_books.len(), 2);
        integrity.record_loss(&loss(a, 3));
        assert_eq!(integrity.faults()[0].current_books.len(), 1);
        assert_eq!(integrity.faults()[0].current_books[0].stream, b);
        let mut lost_b = loss(b, 1);
        lost_b.session = WsSessionId(5);
        assert!(integrity.record_loss(&lost_b));
        assert_eq!(integrity.faults().len(), 2);
        assert!(
            integrity
                .faults()
                .iter()
                .all(|f| f.current_books.is_empty())
        );
        pa.source.generation = WsGeneration(4);
        assert!(integrity.record_evidence(&pa));
        assert!(
            integrity
                .faults()
                .iter()
                .all(|f| f.current_books.len() == 1 && f.current_books[0].stream == a)
        );
        integrity.sync_subscriptions(&[b]);
        integrity.sync_subscriptions(&[a, b]);
        assert!(
            integrity
                .faults()
                .iter()
                .all(|f| f.current_books.is_empty())
        );
    }

    #[test]
    fn ws03_current_book_proof_does_not_heal_depth_history_raw_or_auxiliary() {
        let stream = StreamKind::Depth {
            ticker_info: stream().ticker_info(),
            depth_aggr: Default::default(),
            push_freq: Default::default(),
        };
        let mut integrity = MarketIntegrity::default();
        integrity.sync_subscriptions(&[stream]);
        integrity.record_loss(&loss(stream, 2));
        let mut proof = evidence(stream, 3);
        proof.proof = RecoveryProof::BookSynchronized {
            at: UnixMs::new(2000),
            sequence: 5,
        };
        proof.source.auxiliary = true;
        assert!(!integrity.record_evidence(&proof));
        proof.source.auxiliary = false;
        assert!(integrity.record_evidence(&proof));
        assert!(!integrity.faults()[0].current_books.is_empty());
        assert!(!integrity.blocked_detector_tickers().is_empty());
        assert!(integrity.faults()[0].known_trade_from.is_none());
        assert!(integrity.record_loss(&loss(stream, 3)));
        assert!(integrity.faults()[0].current_books.is_empty());
        assert!(!integrity.record_evidence(&proof));
    }
}
