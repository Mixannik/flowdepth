use crate::adapter::{
    AdapterError, Event, LostEventKind, LostWsEvent, RecoveryProof, StreamKind, WsGeneration,
    WsIntegrityLoss, WsRecoveryEvidence, WsSessionId, WsSource,
};
use crate::proxy::{Proxy, ProxyStream};
use crate::unit::qty::QtyNormalization;
use crate::{Ticker, TickerInfo, Trade, UnixMs};

use bytes::Bytes;
use fastwebsockets::{
    FragmentCollectorRead, Frame, OpCode, Payload, WebSocket, WebSocketError, WebSocketWrite,
};
use http_body_util::Empty;
use hyper::{
    Request,
    header::{CONNECTION, UPGRADE},
    upgrade::Upgraded,
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustc_hash::FxHashMap;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::Instant;
use tokio_rustls::{
    TlsConnector,
    rustls::{ClientConfig, OwnedTrustAnchor},
};
use url::Url;

use futures::StreamExt;
#[cfg(not(feature = "unbounded-channel"))]
use futures::channel::mpsc::Sender;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender};
use std::sync::{
    Arc, LazyLock,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

const HEARTBEAT_SEND_FAILED_REASON: &str = "Failed to send heartbeat ping";
const HEARTBEAT_PONG_FAILED_REASON: &str = "Failed to reply pong";
const HEARTBEAT_TIMEOUT_REASON: &str = "Heartbeat timeout (no websocket activity)";

const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const WS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

pub(super) const TCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

const MAX_DRAIN_PER_TICK: usize = 256;

pub(super) static TLS_CONNECTOR: LazyLock<TlsConnector> = LazyLock::new(|| {
    let mut root_store = tokio_rustls::rustls::RootCertStore::empty();

    root_store.add_trust_anchors(webpki_roots::TLS_SERVER_ROOTS.0.iter().map(|ta| {
        OwnedTrustAnchor::from_subject_spki_name_constraints(
            ta.subject,
            ta.spki,
            ta.name_constraints,
        )
    }));

    let config = ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    TlsConnector::from(Arc::new(config))
});

enum AnySender<T> {
    #[cfg(not(feature = "unbounded-channel"))]
    Bounded(Sender<T>),
    Unbounded(UnboundedSender<T>),
}

impl<T> AnySender<T> {
    fn send(&mut self, item: T) -> Result<(), futures::channel::mpsc::TrySendError<T>> {
        match self {
            #[cfg(not(feature = "unbounded-channel"))]
            AnySender::Bounded(tx) => tx.try_send(item),
            AnySender::Unbounded(tx) => tx.unbounded_send(item),
        }
    }
}

enum AnyReceiver<T> {
    #[cfg(not(feature = "unbounded-channel"))]
    Bounded(futures::channel::mpsc::Receiver<T>),
    Unbounded(UnboundedReceiver<T>),
}

impl<T> futures::Stream for AnyReceiver<T> {
    type Item = T;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match self.get_mut() {
            #[cfg(not(feature = "unbounded-channel"))]
            AnyReceiver::Bounded(rx) => std::pin::Pin::new(rx).poll_next(cx),
            AnyReceiver::Unbounded(rx) => std::pin::Pin::new(rx).poll_next(cx),
        }
    }
}

impl<T> AnyReceiver<T> {
    fn try_recv(&mut self) -> Option<T> {
        match self {
            #[cfg(not(feature = "unbounded-channel"))]
            AnyReceiver::Bounded(rx) => rx.try_recv().ok(),
            AnyReceiver::Unbounded(rx) => rx.try_recv().ok(),
        }
    }
}

fn channel<T>(_capacity: usize) -> (AnySender<T>, AnyReceiver<T>) {
    #[cfg(not(feature = "unbounded-channel"))]
    {
        let (tx, rx) = futures::channel::mpsc::channel(_capacity);
        (AnySender::Bounded(tx), AnyReceiver::Bounded(rx))
    }
    #[cfg(feature = "unbounded-channel")]
    {
        let (tx, rx) = futures::channel::mpsc::unbounded();
        (AnySender::Unbounded(tx), AnyReceiver::Unbounded(rx))
    }
}

pub(super) struct ChannelStream<T> {
    receiver: AnyReceiver<T>,
    task: tokio::task::JoinHandle<()>,
}

impl<T> futures::Stream for ChannelStream<T> {
    type Item = T;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::pin::Pin::new(&mut self.get_mut().receiver).poll_next(cx)
    }
}

impl<T> Drop for ChannelStream<T> {
    fn drop(&mut self) {
        self.task.abort();
    }
}

static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

/// Aborting the parent stream must also cancel its independently spawned reader.
#[derive(Default)]
struct SessionReader(Option<tokio::task::JoinHandle<()>>);

impl SessionReader {
    async fn stop(&mut self) {
        if let Some(handle) = self.0.take() {
            if !handle.is_finished() {
                handle.abort();
            }
            if let Err(error) = handle.await
                && !error.is_cancelled()
            {
                log::error!("WebSocket I/O task failed: {error}");
            }
        }
    }
}

impl Drop for SessionReader {
    fn drop(&mut self) {
        if let Some(handle) = &self.0 {
            handle.abort();
        }
    }
}

#[derive(Debug)]
enum DeliveryFailure {
    Closed,
    Full(Vec<LostWsEvent>),
}

/// The only ordinary event delivery path. On Full, preserve the failed event
/// and the unattempted suffix and unwind the generation immediately.
fn send_events(
    tx: &mut AnySender<Event>,
    events: impl IntoIterator<Item = Event>,
) -> Result<(), DeliveryFailure> {
    let mut events = events.into_iter();
    while let Some(event) = events.next() {
        if let Err(error) = tx.send(event) {
            if error.is_disconnected() {
                return Err(DeliveryFailure::Closed);
            }
            let lost = std::iter::once(error.into_inner())
                .chain(events)
                .map(lost_event)
                .collect();
            return Err(DeliveryFailure::Full(lost));
        }
    }
    Ok(())
}

/// Evidence follows the actual delivered payload through the SAME checked FIFO.
/// If either payload or proof overflows, WS-01 tears down this generation.
fn send_generation_events(
    tx: &mut AnySender<Event>,
    events: Vec<Event>,
    source: WsSource,
    book: Option<(StreamKind, UnixMs, u64)>,
    published: &mut Vec<StreamKind>,
) -> Result<(), DeliveryFailure> {
    let mut proofs = Vec::new();
    for event in &events {
        let candidate = match event {
            Event::TradesReceived(stream, _, trades) if !trades.is_empty() => {
                let first = trades.iter().map(|t| t.time).min().unwrap();
                let last = trades.iter().map(|t| t.time).max().unwrap();
                Some((*stream, RecoveryProof::TradesResumed { first, last }))
            }
            Event::DepthReceived(stream, _, _) => book
                .filter(|(s, _, _)| s == stream)
                .map(|(s, at, sequence)| (s, RecoveryProof::BookSynchronized { at, sequence })),
            _ => None,
        };
        if let Some((stream, proof)) = candidate
            && !published.contains(&stream)
            && !proofs
                .iter()
                .any(|p: &WsRecoveryEvidence| p.stream == stream)
        {
            proofs.push(WsRecoveryEvidence {
                source,
                stream,
                proof,
            });
        }
    }
    send_events(tx, events)?;
    for proof in proofs {
        let stream = proof.stream;
        send_events(tx, [Event::RecoveryEvidence(proof)])?;
        published.push(stream);
    }
    Ok(())
}

fn lost_event(event: Event) -> LostWsEvent {
    use LostEventKind as K;
    let ticker_info = match &event {
        Event::OrderFlow(event) => Some(event.ticker_info()),
        _ => None,
    };
    let (kind, streams, records, times): (_, Arc<[StreamKind]>, _, Vec<UnixMs>) = match event {
        Event::Connected(streams) => (K::Connected, streams, 0, vec![]),
        Event::Disconnected(streams, _) => (K::Disconnected, streams, 0, vec![]),
        Event::DepthReceived(stream, at, _) => (K::Depth, Arc::from([stream]), 1, vec![at]),
        Event::KlineReceived(stream, kline) => (K::Kline, Arc::from([stream]), 1, vec![kline.time]),
        Event::TradesReceived(stream, _, trades) => (
            K::Trades,
            Arc::from([stream]),
            trades.len(),
            trades.iter().map(|t| t.time).collect(),
        ),
        Event::LiquidationsReceived(stream, trades) => (
            K::Liquidations,
            Arc::from([stream]),
            trades.len(),
            trades.iter().map(|t| t.time).collect(),
        ),
        Event::OrderFlow(event) => {
            use crate::orderflow::OrderFlowEvent as O;
            let at = match event {
                O::Trade(t) => t.trade_time,
                O::BookDelta(d) => d.exchange_time,
                O::Quality { at, .. } | O::Reconnect { at, .. } => at,
            };
            (K::OrderFlow, Arc::from([]), 1, vec![at])
        }
        Event::OrderFlowTrades(trades) => {
            let mut streams = Vec::new();
            for trade in &trades {
                let stream = StreamKind::Trades {
                    ticker_info: trade.ticker_info,
                };
                if !streams.contains(&stream) {
                    streams.push(stream);
                }
            }
            (
                K::OrderFlowTrades,
                streams.into(),
                trades.len(),
                trades.iter().map(|t| t.trade_time).collect(),
            )
        }
        Event::RecoveryEvidence(evidence) => {
            (K::RecoveryEvidence, Arc::from([evidence.stream]), 0, vec![])
        }
        Event::IntegrityLost(_) => unreachable!("integrity markers use reliable delivery"),
    };
    LostWsEvent {
        kind,
        streams,
        ticker_info,
        records,
        first_known_utc: times.iter().copied().min(),
        last_known_utc: times.iter().copied().max(),
    }
}

/// Reader termination precedes any wait on the bounded output. SinkExt::send
/// registers the receiver wakeup, including closure; dropping ChannelStream
/// aborts the parent future and cancels this wait.
async fn report_integrity_loss(
    tx: &mut AnySender<Event>,
    reader: &mut SessionReader,
    loss: WsIntegrityLoss,
) -> Result<(), DeliveryFailure> {
    reader.stop().await;
    let event = Event::IntegrityLost(loss);
    match tx {
        #[cfg(not(feature = "unbounded-channel"))]
        AnySender::Bounded(tx) => futures::SinkExt::send(tx, event)
            .await
            .map_err(|_| DeliveryFailure::Closed),
        AnySender::Unbounded(tx) => tx
            .unbounded_send(event)
            .map_err(|_| DeliveryFailure::Closed),
    }
}

#[derive(Clone, Debug)]
pub(super) struct WsSession {
    streams: Arc<[StreamKind]>,
    /// Auxiliary sockets (e.g. liquidations) must not report Connected/Disconnected for
    /// the streams they piggyback on, or connectivity status would flap with them.
    silent: bool,
}

/// How often [`on_tick`](WsAdapter::on_tick) fires. Also serves as the time-bucket
/// granularity for trade aggregation — trades within one interval are collapsed
/// into a single [`Event::TradesReceived`].
const ADAPTER_TICK_INTERVAL: Duration = Duration::from_micros(33_333);

pub(super) trait WsAdapter {
    /// Selects the transport heartbeat policy for this adapter.
    ///
    /// The session reads this policy after each successful connection. The
    /// policy owns all venue-specific timing and wire-format decisions, while
    /// the shared transport handles frame activity and control replies.
    fn heartbeat_policy(&self) -> HeartbeatPolicy;

    /// Only adapters with a proven snapshot/diff boundary opt in. The session
    /// stamps identity and publishes proof only after a matching DepthReceived.
    fn book_sync_proof(&self) -> Option<(StreamKind, UnixMs, u64)> {
        None
    }

    /// Connects to the WebSocket and returns a transport for it.
    /// This will be retried indefinitely until it succeeds, with an exponential backoff
    /// between attempts (base ~500ms, doubling, capped at 30s, with jitter).
    fn connect(&mut self) -> impl std::future::Future<Output = Result<WsTransport, String>> + Send;

    /// Tick interval controlling how often [`on_tick`](WsAdapter::on_tick) is called
    /// and the time-bucket granularity for trade aggregation.
    fn tick_interval(&self) -> Duration {
        ADAPTER_TICK_INTERVAL
    }

    /// Called when a connection is established.
    /// This is called on every successful connection, including after reconnects.
    fn on_connected(&mut self) -> impl std::future::Future<Output = Vec<Event>> + Send;

    /// Called periodically while connected, at the cadence returned by
    /// [`tick_interval`].
    ///
    /// This is where **trade adapters** flush their [`TradeBuffer`] — trades are
    /// batched across one tick interval and emerge as a single
    /// [`Event::TradesReceived`]. Non-trade adapters leave this as the default
    /// no-op because they push events directly in [`on_text`](Self::on_text).
    fn on_tick(&mut self) -> impl std::future::Future<Output = Vec<Event>> + Send {
        async { Vec::new() }
    }

    /// Called when a text message is received.
    ///
    /// Adapters parse incoming data and return resulting `Event`s.
    /// The session loop sends them to the output channel.
    /// Queue overflow terminates the generation and produces an IntegrityLost marker.
    ///
    /// **Flush model**: non-trade adapters return events here directly.
    /// Trade adapters only buffer into [`TradeBuffer`] here and return
    /// events later in [`on_tick`](Self::on_tick).
    fn on_text(
        &mut self,
        payload: &[u8],
    ) -> impl std::future::Future<Output = Result<Vec<Event>, String>> + Send;

    /// Called when the connection is closed or a fatal error occurs.
    fn on_disconnected(
        &mut self,
        reason: &str,
    ) -> impl std::future::Future<Output = Vec<Event>> + Send;
}

impl WsSession {
    pub(super) fn new(streams: Arc<[StreamKind]>) -> Self {
        Self {
            streams,
            silent: false,
        }
    }

    pub(super) fn silent(mut self) -> Self {
        self.silent = true;
        self
    }

    pub(super) fn run<A: WsAdapter + Send + 'static>(self, mut adapter: A) -> ChannelStream<Event> {
        let (mut event_tx, event_rx) = channel(512);

        let streams = Arc::clone(&self.streams);
        let silent = self.silent;

        let session = WsSessionId(NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed));
        let task = tokio::spawn(async move {
            if streams.is_empty() {
                // A fresh queue has room for this single event; Closed ends the task.
                if send_events(
                    &mut event_tx,
                    [Event::Disconnected(
                        streams,
                        "Empty stream payload".to_string(),
                    )],
                )
                .is_err()
                {
                    return;
                }
                return;
            }

            let mut backoff = ReconnectBackoff::new();
            let mut disconnect_count: u32 = 0;
            let mut last_disconnect_instant: Option<std::time::Instant> = None;
            let mut attempt: u64 = 0;

            loop {
                attempt += 1;
                log::info!(
                    "WS ReconnectAttempt | attempt={attempt} backoff_next={:?}",
                    backoff.peek_delay()
                );

                let source = WsSource {
                    session,
                    generation: WsGeneration(attempt),
                    auxiliary: silent,
                };
                let mut published = Vec::new();
                let mut reader = SessionReader::default();
                let mut disconnected_callback_called = false;
                let mut reconnect_delay = None;
                let outcome: Result<(), DeliveryFailure> = async {
                    let transport = match adapter.connect().await {
                        Ok(t) => t,
                        Err(reason) => {
                            disconnect_count += 1;
                            log::warn!(
                                "WS ConnectFailed | reason={reason} count={disconnect_count} backoff_next={:?}",
                                backoff.peek_delay()
                            );
                            if !silent {
                                send_events(&mut event_tx, [Event::Disconnected(Arc::clone(&streams), reason)])?;
                            }
                            reconnect_delay = Some(backoff.delay());
                            backoff.record_failure();
                            return Ok(());
                        }
                    };

                    let heartbeat_policy = adapter.heartbeat_policy();

                    let (frame_tx, mut frame_rx) = {
                        let (tx, rx) = futures::channel::mpsc::unbounded();
                        (AnySender::Unbounded(tx), AnyReceiver::Unbounded(rx))
                    };
                    reader.0 = Some(tokio::spawn(transport.read_frame(heartbeat_policy, frame_tx)));

                    let events = adapter.on_connected().await;
                    send_generation_events(&mut event_tx, events, source, adapter.book_sync_proof(), &mut published)?;
                    if !silent {
                        send_events(&mut event_tx, [Event::Connected(Arc::clone(&streams))])?;
                    }

                    let tick_interval = adapter.tick_interval();
                    let tick_sleep = tokio::time::sleep(tick_interval);
                    tokio::pin!(tick_sleep);

                    // Track connection stability for backoff reset.
                    let connected_at = std::time::Instant::now();
                    let stability_timer = tokio::time::sleep(ReconnectBackoff::STABLE_THRESHOLD);
                    tokio::pin!(stability_timer);
                    let mut is_stable = false;
                    let mut data_received = false;

                    let disconnect_reason = loop {
                        tokio::select! {
                            biased;
                            // Raw trade streams can remain continuously ready on active symbols.
                            // Service the batching timer first so detector traffic cannot starve
                            // legacy TradesReceived events used by bubbles, footprint and CVD.
                            _ = &mut tick_sleep => {
                                let events = adapter.on_tick().await;
                    send_generation_events(&mut event_tx, events, source, adapter.book_sync_proof(), &mut published)?;
                                tick_sleep
                                    .as_mut()
                                    .reset(tokio::time::Instant::now() + tick_interval);
                            }
                            frame = frame_rx.next() => {
                                let mut disconnect_reason: Option<String> = None;

                                match frame {
                                    Some(Ok(payload)) => {
                                        if !payload.is_empty() {
                                            match adapter.on_text(&payload).await {
                                                Ok(events) => {
                                                    let had_events = !events.is_empty();
                                                    send_generation_events(&mut event_tx, events, source, adapter.book_sync_proof(), &mut published)?;
                                                    if had_events {
                                                        data_received = true;
                                                        backoff.record_success();
                                                    }
                                                }
                                                Err(reason) => {
                                                    disconnect_reason = Some(reason);
                                                }
                                            }
                                        }

                                        if disconnect_reason.is_none() {
                                            let mut drained = 0;
                                            while drained < MAX_DRAIN_PER_TICK {
                                                let Some(drain) = frame_rx.try_recv() else { break };
                                                drained += 1;
                                                match drain {
                                                    Ok(payload) => {
                                                        match adapter.on_text(&payload).await {
                                                            Ok(events) => {
                                                                let had_events = !events.is_empty();
                                                                send_generation_events(&mut event_tx, events, source, adapter.book_sync_proof(), &mut published)?;
                                                                if had_events {
                                                                    data_received = true;
                                                                    backoff.record_success();
                                                                }
                                                            }
                                                            Err(reason) => {
                                                                disconnect_reason = Some(reason);
                                                                break;
                                                            }
                                                        }
                                                    }
                                                    Err(reason) => {
                                                        disconnect_reason = Some(reason);
                                                        break;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    Some(Err(reason)) => {
                                        disconnect_reason = Some(reason);
                                    }
                                    None => {
                                        disconnect_reason = Some(
                                            "I/O task exited".to_string(),
                                        );
                                    }
                                }

                                if let Some(reason) = disconnect_reason {
                                    break Some(reason);
                                }
                            }
                            _ = &mut stability_timer, if !is_stable => {
                                is_stable = true;
                                let connected_for = connected_at.elapsed();
                                log::info!(
                                    "WS Stable | connected_for={connected_for:?}"
                                );
                                if backoff.is_inflated() {
                                    log::info!(
                                        "WS BackoffReset | reason=connected_stable connected_for={connected_for:?}"
                                    );
                                    backoff.record_success();
                                }
                            }
                        }
                    };

                    reader.stop().await;

                    if let Some(reason) = disconnect_reason {
                        disconnect_count += 1;

                        // Only inflate backoff for genuine failures (no data received).
                        // Stable connections that received data before dropping should
                        // not increase the reconnection delay.
                        if !data_received {
                            backoff.record_failure();
                        }

                        let since_last_str = match last_disconnect_instant {
                            Some(t) => format!("{:?}", t.elapsed()),
                            None => "none".to_string(),
                        };
                        last_disconnect_instant = Some(std::time::Instant::now());

                        log::warn!(
                            "WS ReconnectDiag | reason={reason} count={disconnect_count} since_last={since_last_str} backoff_next={:?}",
                            backoff.peek_delay()
                        );

                        disconnected_callback_called = true;
                        send_events(&mut event_tx, adapter.on_disconnected(&reason).await)?;
                        if !silent {
                            send_events(&mut event_tx, [Event::Disconnected(Arc::clone(&streams), reason)])?;
                        }
                    }

                    Ok(())
                }.await;
                match outcome {
                    Ok(()) => {}
                    Err(DeliveryFailure::Closed) => {
                        reader.stop().await;
                        return;
                    }
                    Err(DeliveryFailure::Full(mut lost_events)) => {
                        let detected_at = UnixMs::now();
                        reader.stop().await;
                        // Flush/reset adapter-owned buffers without publishing any more
                        // ordinary events from the failed generation.
                        if !disconnected_callback_called {
                            lost_events.extend(
                                adapter
                                    .on_disconnected("WS output queue overflow")
                                    .await
                                    .into_iter()
                                    .map(lost_event),
                            );
                        }
                        let loss = WsIntegrityLoss {
                            session,
                            generation: WsGeneration(attempt),
                            streams: Arc::clone(&streams),
                            auxiliary: silent,
                            detected_at,
                            lost_events,
                            complete_loss_extent_known: false,
                        };
                        if report_integrity_loss(&mut event_tx, &mut reader, loss)
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                }

                let delay = reconnect_delay.unwrap_or_else(|| backoff.delay());
                log::info!("WS ReconnectScheduled | attempt={attempt} delay={delay:?}");
                tokio::time::sleep(delay).await;
            }
        });

        ChannelStream {
            receiver: event_rx,
            task,
        }
    }
}

/// Wire representation for an outbound heartbeat payload.
#[derive(Clone, Copy, Debug)]
pub(crate) enum PingPayload {
    /// An application-level text message.
    Text(&'static [u8]),
    /// A WebSocket control Ping frame.
    OpCode(&'static [u8]),
}

/// Transport-level keepalive strategy for a WebSocket adapter.
///
/// Activity is recorded for every complete inbound frame, including control
/// frames. The policy determines when the connection sends a heartbeat and
/// when a lack of inbound activity is treated as a dead connection.
#[derive(Clone, Copy, Debug)]
pub(super) enum HeartbeatPolicy {
    /// Send an application or control ping at a fixed cadence.
    ///
    /// The next ping is scheduled after the previous write succeeds. The
    /// connection is closed when no complete inbound frame arrives within
    /// `silence_timeout`.
    PeriodicClientPing {
        ping: PingPayload,
        every: Duration,
        silence_timeout: Duration,
    },
    /// Send one application-level ping after inbound traffic has been idle.
    ///
    /// A complete inbound frame returns the policy to idle monitoring. After
    /// a ping is written, the connection waits `response_timeout` for any
    /// complete inbound frame before closing.
    PingAfterIdle {
        ping: PingPayload,
        idle_for: Duration,
        response_timeout: Duration,
    },
    /// Rely on protocol-level pings sent by the server.
    ///
    /// The transport still replies to incoming WebSocket Ping frames, but it
    /// never sends a proactive heartbeat.
    ServerDriven { silence_timeout: Duration },
}

impl HeartbeatPolicy {
    /// Creates a [`PeriodicClientPing`](HeartbeatPolicy::PeriodicClientPing) policy
    /// that sends an application-level text ping.
    pub(super) const fn periodic_text(
        payload: &'static [u8],
        every: Duration,
        silence_timeout: Duration,
    ) -> Self {
        Self::PeriodicClientPing {
            ping: PingPayload::Text(payload),
            every,
            silence_timeout,
        }
    }

    /// Creates a [`PeriodicClientPing`](HeartbeatPolicy::PeriodicClientPing) policy
    /// that sends a WebSocket control Ping frame.
    #[allow(dead_code)]
    pub(crate) const fn periodic_opcode(
        payload: &'static [u8],
        every: Duration,
        silence_timeout: Duration,
    ) -> Self {
        Self::PeriodicClientPing {
            ping: PingPayload::OpCode(payload),
            every,
            silence_timeout,
        }
    }

    /// Creates a [`PingAfterIdle`](HeartbeatPolicy::PingAfterIdle) policy
    /// that sends an application-level text ping.
    pub(super) const fn ping_after_idle_text(
        payload: &'static [u8],
        idle_for: Duration,
        response_timeout: Duration,
    ) -> Self {
        Self::PingAfterIdle {
            ping: PingPayload::Text(payload),
            idle_for,
            response_timeout,
        }
    }

    /// Creates a [`ServerDriven`](HeartbeatPolicy::ServerDriven) policy
    /// that only observes server-driven activity.
    pub(super) const fn server_driven(silence_timeout: Duration) -> Self {
        Self::ServerDriven { silence_timeout }
    }
}

enum HeartbeatState {
    Periodic { next_ping: Instant },
    IdleMonitoring,
    IdleAwaitingResponse { deadline: Instant },
    ServerDriven,
}

enum HeartbeatAction {
    SendPing(PingPayload),
    Timeout,
    Wait,
}

struct WsHeartbeat {
    policy: HeartbeatPolicy,
    last_activity: Instant,
    state: HeartbeatState,
}

impl WsHeartbeat {
    fn new(policy: HeartbeatPolicy, now: Instant) -> Self {
        let state = match policy {
            HeartbeatPolicy::PeriodicClientPing { every, .. } => HeartbeatState::Periodic {
                next_ping: now + every,
            },
            HeartbeatPolicy::PingAfterIdle { .. } => HeartbeatState::IdleMonitoring,
            HeartbeatPolicy::ServerDriven { .. } => HeartbeatState::ServerDriven,
        };

        Self {
            policy,
            last_activity: now,
            state,
        }
    }

    fn next_deadline(&self) -> Instant {
        match (self.policy, &self.state) {
            (
                HeartbeatPolicy::PeriodicClientPing {
                    silence_timeout, ..
                },
                HeartbeatState::Periodic { next_ping },
            ) => (self.last_activity + silence_timeout).min(*next_ping),
            (HeartbeatPolicy::PingAfterIdle { idle_for, .. }, HeartbeatState::IdleMonitoring) => {
                self.last_activity + idle_for
            }
            (
                HeartbeatPolicy::PingAfterIdle { .. },
                HeartbeatState::IdleAwaitingResponse { deadline },
            ) => *deadline,
            (HeartbeatPolicy::ServerDriven { silence_timeout }, HeartbeatState::ServerDriven) => {
                self.last_activity + silence_timeout
            }
            (
                HeartbeatPolicy::PeriodicClientPing {
                    silence_timeout, ..
                },
                _,
            ) => self.last_activity + silence_timeout,
            (HeartbeatPolicy::PingAfterIdle { idle_for, .. }, _) => self.last_activity + idle_for,
            (HeartbeatPolicy::ServerDriven { silence_timeout }, _) => {
                self.last_activity + silence_timeout
            }
        }
    }

    fn deadline_action(&self, now: Instant) -> HeartbeatAction {
        match (self.policy, &self.state) {
            (
                HeartbeatPolicy::PeriodicClientPing {
                    ping,
                    silence_timeout,
                    ..
                },
                HeartbeatState::Periodic { next_ping },
            ) => {
                if now >= self.last_activity + silence_timeout {
                    HeartbeatAction::Timeout
                } else if now >= *next_ping {
                    HeartbeatAction::SendPing(ping)
                } else {
                    HeartbeatAction::Wait
                }
            }
            (
                HeartbeatPolicy::PingAfterIdle { ping, idle_for, .. },
                HeartbeatState::IdleMonitoring,
            ) => {
                if now >= self.last_activity + idle_for {
                    HeartbeatAction::SendPing(ping)
                } else {
                    HeartbeatAction::Wait
                }
            }
            (
                HeartbeatPolicy::PingAfterIdle { .. },
                HeartbeatState::IdleAwaitingResponse { deadline },
            ) => {
                if now >= *deadline {
                    HeartbeatAction::Timeout
                } else {
                    HeartbeatAction::Wait
                }
            }
            (HeartbeatPolicy::ServerDriven { silence_timeout }, HeartbeatState::ServerDriven) => {
                if now >= self.last_activity + silence_timeout {
                    HeartbeatAction::Timeout
                } else {
                    HeartbeatAction::Wait
                }
            }
            _ => HeartbeatAction::Wait,
        }
    }

    fn mark_activity(&mut self, now: Instant) {
        self.last_activity = now;
        if matches!(self.state, HeartbeatState::IdleAwaitingResponse { .. }) {
            self.state = HeartbeatState::IdleMonitoring;
        }
    }

    fn record_ping_sent(&mut self, now: Instant) {
        match (self.policy, &self.state) {
            (
                HeartbeatPolicy::PeriodicClientPing { every, .. },
                HeartbeatState::Periodic { .. },
            ) => {
                self.state = HeartbeatState::Periodic {
                    next_ping: now + every,
                };
            }
            (
                HeartbeatPolicy::PingAfterIdle {
                    response_timeout, ..
                },
                HeartbeatState::IdleMonitoring,
            ) => {
                self.state = HeartbeatState::IdleAwaitingResponse {
                    deadline: now + response_timeout,
                };
            }
            _ => {}
        }
    }
}

struct WsConnection<R, W> {
    reader: FragmentCollectorRead<R>,
    writer: WebSocketWrite<W>,
    heartbeat: WsHeartbeat,
    frame_tx: AnySender<Result<Vec<u8>, String>>,
}

impl<R, W> WsConnection<R, W>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    fn new(
        reader: FragmentCollectorRead<R>,
        writer: WebSocketWrite<W>,
        heartbeat_policy: HeartbeatPolicy,
        frame_tx: AnySender<Result<Vec<u8>, String>>,
    ) -> Self {
        Self {
            reader,
            writer,
            heartbeat: WsHeartbeat::new(heartbeat_policy, Instant::now()),
            frame_tx,
        }
    }

    async fn run(self) {
        let Self {
            mut reader,
            mut writer,
            mut heartbeat,
            mut frame_tx,
        } = self;

        loop {
            let mut no_control_send =
                |_frame: Frame<'_>| std::future::ready(Ok::<(), std::io::Error>(()));
            let mut read_future = Box::pin(reader.read_frame(&mut no_control_send));

            let outcome = loop {
                let deadline = heartbeat.next_deadline();

                tokio::select! {
                    biased;
                    result = &mut read_future => break Ok(result),
                    _ = tokio::time::sleep_until(deadline) => {
                        if let Err(reason) = apply_heartbeat_action(&mut heartbeat, &mut writer).await {
                            break Err(reason);
                        }
                    }
                }
            };

            drop(read_future);

            let message = match outcome {
                Ok(Ok(message)) => message,
                Ok(Err(error)) => {
                    let _ = frame_tx.send(Err(format!("Error reading frame: {error}")));
                    break;
                }
                Err(reason) => {
                    let _ = frame_tx.send(Err(reason.to_string()));
                    break;
                }
            };

            heartbeat.mark_activity(Instant::now());

            let keep_connection = match message.opcode {
                OpCode::Text => {
                    let payload = Vec::from(&message.payload[..]);
                    frame_tx.send(Ok(payload)).is_ok()
                }
                OpCode::Ping => {
                    let payload = Vec::from(message.payload);
                    if writer
                        .write_frame(Frame::pong(Payload::Owned(payload)))
                        .await
                        .is_err()
                    {
                        let _ = frame_tx.send(Err(HEARTBEAT_PONG_FAILED_REASON.into()));
                        false
                    } else {
                        frame_tx.send(Ok(Vec::new())).is_ok()
                    }
                }
                OpCode::Close => {
                    let payload = Vec::from(message.payload);
                    let close_reason = format_close_frame_reason(&payload);
                    let _ = writer
                        .write_frame(Frame::close_raw(Payload::Owned(payload)))
                        .await;
                    let _ = frame_tx.send(Err(close_reason));
                    false
                }
                _ => true,
            };

            if !keep_connection {
                break;
            }

            if let Err(reason) = apply_heartbeat_action(&mut heartbeat, &mut writer).await {
                let _ = frame_tx.send(Err(reason.to_string()));
                break;
            }
        }
    }
}

async fn apply_heartbeat_action<W>(
    heartbeat: &mut WsHeartbeat,
    writer: &mut WebSocketWrite<W>,
) -> Result<(), &'static str>
where
    W: AsyncWrite + Unpin,
{
    match heartbeat.deadline_action(Instant::now()) {
        HeartbeatAction::SendPing(ping) => {
            write_heartbeat_ping(writer, ping)
                .await
                .map_err(|_| HEARTBEAT_SEND_FAILED_REASON)?;
            heartbeat.record_ping_sent(Instant::now());
            Ok(())
        }
        HeartbeatAction::Timeout => Err(HEARTBEAT_TIMEOUT_REASON),
        HeartbeatAction::Wait => Ok(()),
    }
}

async fn write_heartbeat_ping<W>(
    writer: &mut WebSocketWrite<W>,
    ping_payload: PingPayload,
) -> Result<(), WebSocketError>
where
    W: AsyncWrite + Unpin,
{
    let frame = match ping_payload {
        PingPayload::Text(payload) => Frame::text(Payload::Borrowed(payload)),
        PingPayload::OpCode(payload) => {
            Frame::new(true, OpCode::Ping, None, Payload::Borrowed(payload))
        }
    };

    writer.write_frame(frame).await
}

fn format_close_frame_reason(payload: &[u8]) -> String {
    const MAX_CLOSE_REASON_CHARS: usize = 512;
    match payload {
        [] => "Connection closed by peer: no status code or reason".to_string(),
        [_] => "Connection closed by peer: invalid close payload (one byte)".to_string(),
        [code_high, code_low, reason @ ..] => {
            let code = u16::from_be_bytes([*code_high, *code_low]);
            let close_code = fastwebsockets::CloseCode::from(code);
            let reason = match std::str::from_utf8(reason) {
                Ok("") => "no reason".to_string(),
                Ok(reason) => {
                    let mut chars = reason.chars();
                    let preview: String = chars.by_ref().take(MAX_CLOSE_REASON_CHARS).collect();
                    let suffix = if chars.next().is_some() {
                        " (truncated)"
                    } else {
                        ""
                    };
                    format!("reason={preview:?}{suffix}")
                }
                Err(_) => format!("reason=<invalid UTF-8, {} bytes>", reason.len()),
            };

            format!("Connection closed by peer: code={code} ({close_code:?}), {reason}")
        }
    }
}

pub(super) struct WsTransport(WebSocket<TokioIo<Upgraded>>);

impl WsTransport {
    /// Reads frames, handles heartbeat and Ping/Pong at transport level,
    /// forwards text frames to the processor task.
    async fn read_frame(
        self,
        heartbeat_policy: HeartbeatPolicy,
        frame_tx: AnySender<Result<Vec<u8>, String>>,
    ) {
        let (mut reader, writer) = self.0.split(tokio::io::split);
        reader.set_auto_pong(false);
        reader.set_auto_close(false);

        let reader = FragmentCollectorRead::new(reader);
        WsConnection::new(reader, writer, heartbeat_policy, frame_tx)
            .run()
            .await;
    }

    pub(super) async fn write_frame(&mut self, frame: Frame<'_>) -> Result<(), WebSocketError> {
        self.0.write_frame(frame).await
    }

    pub(super) async fn establish(
        domain: &str,
        url: &str,
        proxy_cfg: Option<&Proxy>,
    ) -> Result<Self, AdapterError> {
        let parsed = Url::parse(url).map_err(|e| AdapterError::InvalidRequest(e.to_string()))?;

        let url_host = parsed.host_str().ok_or_else(|| {
            AdapterError::InvalidRequest("Missing host in websocket URL".to_string())
        })?;

        if !url_host.eq_ignore_ascii_case(domain) {
            return Err(AdapterError::InvalidRequest(format!(
                "WebSocket URL host mismatch: url_host={url_host}, domain_arg={domain}"
            )));
        }

        let target_port = parsed.port_or_known_default().ok_or_else(|| {
            AdapterError::InvalidRequest("Missing port for websocket URL".to_string())
        })?;

        let tcp_stream = ProxyStream::connect_tcp(domain, target_port, proxy_cfg).await?;

        match parsed.scheme() {
            "wss" => {
                let tls_stream = tokio::time::timeout(
                    TLS_HANDSHAKE_TIMEOUT,
                    Self::upgrade_to_tls(tcp_stream, domain),
                )
                .await
                .map_err(|_| {
                    AdapterError::WebsocketError("TLS handshake to target timed out".to_string())
                })??;

                tokio::time::timeout(
                    WS_HANDSHAKE_TIMEOUT,
                    Self::handshake_tls(tls_stream, domain, &parsed),
                )
                .await
                .map_err(|_| {
                    AdapterError::WebsocketError("WebSocket handshake timed out".to_string())
                })?
            }
            "ws" => tokio::time::timeout(
                WS_HANDSHAKE_TIMEOUT,
                Self::handshake_tcp(tcp_stream, domain, &parsed),
            )
            .await
            .map_err(|_| {
                AdapterError::WebsocketError("WebSocket handshake timed out".to_string())
            })?,
            _ => Err(AdapterError::InvalidRequest(
                "Invalid scheme for websocket URL".to_string(),
            )),
        }
    }

    async fn upgrade_to_tls(
        stream: ProxyStream,
        domain: &str,
    ) -> Result<Box<tokio_rustls::client::TlsStream<ProxyStream>>, AdapterError> {
        let server_name = tokio_rustls::rustls::ServerName::try_from(domain)
            .map_err(|_| AdapterError::ParseError("invalid dnsname".to_string()))?;

        let tls_stream = TLS_CONNECTOR
            .connect(server_name, stream)
            .await
            .map_err(|e| AdapterError::WebsocketError(e.to_string()))?;

        Ok(Box::new(tls_stream))
    }

    async fn handshake_tcp(
        stream: ProxyStream,
        domain: &str,
        parsed: &Url,
    ) -> Result<Self, AdapterError> {
        let req = Self::build_ws_request(domain, parsed)?;
        let exec = TokioExecutor::new();
        let (ws, _http_resp) = fastwebsockets::handshake::client(&exec, req, stream)
            .await
            .map_err(|e| AdapterError::WebsocketError(e.to_string()))?;
        Ok(Self(ws))
    }

    async fn handshake_tls(
        tls: Box<tokio_rustls::client::TlsStream<ProxyStream>>,
        domain: &str,
        parsed: &Url,
    ) -> Result<Self, AdapterError> {
        let req = Self::build_ws_request(domain, parsed)?;
        let exec = TokioExecutor::new();
        let (ws, _http_resp) = fastwebsockets::handshake::client(&exec, req, tls)
            .await
            .map_err(|e| AdapterError::WebsocketError(e.to_string()))?;
        Ok(Self(ws))
    }

    fn build_ws_request(domain: &str, parsed: &Url) -> Result<Request<Empty<Bytes>>, AdapterError> {
        let mut path_and_query = parsed.path().to_string();
        if let Some(q) = parsed.query() {
            path_and_query.push('?');
            path_and_query.push_str(q);
        }
        if path_and_query.is_empty() {
            path_and_query.push('/');
        }

        let host_header = match parsed.port() {
            Some(explicit_port) => {
                let default_port = parsed.port_or_known_default().unwrap_or(explicit_port);
                if explicit_port != default_port {
                    format!("{domain}:{explicit_port}")
                } else {
                    domain.to_string()
                }
            }
            None => domain.to_string(),
        };

        Request::builder()
            .method("GET")
            .uri(path_and_query)
            .header("Host", host_header)
            .header(UPGRADE, "websocket")
            .header(CONNECTION, "upgrade")
            .header(
                "Sec-WebSocket-Key",
                fastwebsockets::handshake::generate_key(),
            )
            .header("Sec-WebSocket-Version", "13")
            .body(Empty::<Bytes>::new())
            .map_err(|e| AdapterError::WebsocketError(e.to_string()))
    }
}

/// Exponential backoff for WebSocket reconnection attempts.
///
/// Delay doubles on each failure, resets to the initial 500ms on success.
/// Capped at 30s with ±25% multiplicative jitter to spread reconnections
/// across streams when multiple disconnect at once.
struct ReconnectBackoff {
    current: Duration,
}

impl ReconnectBackoff {
    const INITIAL: Duration = Duration::from_millis(500);
    const MAX: Duration = Duration::from_secs(30);
    const JITTER: f32 = 0.25;
    /// How long a connection must remain alive before backoff is reset.
    const STABLE_THRESHOLD: Duration = Duration::from_secs(30);

    fn new() -> Self {
        Self {
            current: Self::INITIAL,
        }
    }

    /// Returns the delay before the next reconnect attempt, with ±jitter applied.
    fn delay(&self) -> Duration {
        let factor = 1.0 + (rand::random::<f32>() * 2.0 - 1.0) * Self::JITTER;
        let secs = self.current.as_secs_f32() * factor;
        Duration::from_secs_f32(secs.max(0.0)).min(Self::MAX)
    }

    /// Doubles the delay (capped) after a failed attempt.
    fn record_failure(&mut self) {
        self.current = (self.current.mul_f32(2.0)).min(Self::MAX);
    }

    /// Resets the delay to the initial value after genuine success
    /// (real market-data events were produced by the connection).
    fn record_success(&mut self) {
        self.current = Self::INITIAL;
    }

    /// Returns the current delay without jitter, for diagnostic logging.
    fn peek_delay(&self) -> Duration {
        self.current.min(Self::MAX)
    }

    /// Returns true if the backoff has been inflated above the initial value.
    fn is_inflated(&self) -> bool {
        self.current > Self::INITIAL
    }
}

pub(super) struct TradeBuffer {
    buffer_map: FxHashMap<Ticker, Vec<Trade>>,
    ticker_info_map: FxHashMap<Ticker, (TickerInfo, QtyNormalization)>,
}

impl TradeBuffer {
    pub(super) fn new(ticker_info_map: FxHashMap<Ticker, (TickerInfo, QtyNormalization)>) -> Self {
        Self {
            buffer_map: FxHashMap::default(),
            ticker_info_map,
        }
    }

    pub(super) fn ticker_info(&self, ticker: &Ticker) -> Option<&(TickerInfo, QtyNormalization)> {
        self.ticker_info_map.get(ticker)
    }

    pub(super) fn push(&mut self, ticker: Ticker, trade: Trade) {
        self.buffer_map.entry(ticker).or_default().push(trade);
    }

    pub(super) fn ticker_infos(&self) -> impl Iterator<Item = TickerInfo> + '_ {
        self.ticker_info_map.values().map(|(info, _)| *info)
    }

    /// Drain all buffered trades, clearing internal buffers.
    ///
    /// Each ticker's trades are collapsed into a single [`Event::TradesReceived`]
    /// keyed by the most recent trade's time rounded down to the nearest
    /// [`ADAPTER_TICK_INTERVAL`] bucket.
    pub(super) fn flush(&mut self) -> Vec<Event> {
        let interval_ms = ADAPTER_TICK_INTERVAL.as_millis() as u64;
        let mut events = Vec::new();

        for (ticker, trades_buffer) in self.buffer_map.iter_mut() {
            if trades_buffer.is_empty() {
                continue;
            }

            let bucket_update_t = trades_buffer
                .iter()
                .map(|t| t.time.as_u64())
                .max()
                .map(|t| UnixMs::new((t / interval_ms) * interval_ms));

            if let Some((ticker_info, _)) = self.ticker_info_map.get(ticker)
                && let Some(update_t) = bucket_update_t
            {
                events.push(Event::TradesReceived(
                    StreamKind::Trades {
                        ticker_info: *ticker_info,
                    },
                    update_t,
                    std::mem::take(trades_buffer).into_boxed_slice(),
                ));
            }
        }

        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::channel::oneshot;

    fn scope() -> Arc<[StreamKind]> {
        Arc::from([StreamKind::Trades {
            ticker_info: TickerInfo::new(
                Ticker::new("BTCUSDT", crate::adapter::Exchange::BinanceLinear),
                0.1,
                0.001,
                None,
            ),
        }])
    }

    #[cfg(not(feature = "unbounded-channel"))]
    fn loss(lost_events: Vec<LostWsEvent>) -> WsIntegrityLoss {
        WsIntegrityLoss {
            session: WsSessionId(7),
            generation: WsGeneration(3),
            streams: scope(),
            auxiliary: true,
            detected_at: UnixMs::new(100),
            lost_events,
            complete_loss_extent_known: false,
        }
    }

    struct OnDrop(Option<oneshot::Sender<()>>);
    impl Drop for OnDrop {
        fn drop(&mut self) {
            if let Some(tx) = self.0.take() {
                let _ = tx.send(());
            }
        }
    }

    async fn reader() -> (SessionReader, oneshot::Receiver<()>) {
        let (started_tx, started_rx) = oneshot::channel();
        let (stopped_tx, stopped_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _guard = OnDrop(Some(stopped_tx));
            started_tx.send(()).unwrap();
            futures::future::pending::<()>().await;
        });
        started_rx.await.unwrap();
        (SessionReader(Some(task)), stopped_rx)
    }

    // A watchdog only; scheduling/order is controlled by polls and oneshot barriers.
    async fn completes<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(5), future)
            .await
            .expect("task hung")
    }

    #[cfg(not(feature = "unbounded-channel"))]
    fn full(tx: &mut AnySender<Event>) -> Vec<LostWsEvent> {
        // futures mpsc grants a reserved slot to the sender in addition to capacity.
        match send_events(tx, (0..16).map(|_| Event::Connected(scope()))) {
            Err(DeliveryFailure::Full(lost)) => lost,
            other => panic!("expected Full: {other:?}"),
        }
    }

    #[cfg(not(feature = "unbounded-channel"))]
    #[tokio::test]
    async fn overflow_stops_reader_before_wait_and_marker_precedes_new_generation() {
        let (mut tx, mut rx) = channel(1);
        let lost = full(&mut tx);
        assert_eq!(lost.len(), 14);
        assert!(lost.iter().all(|e| e.kind == LostEventKind::Connected));
        let (mut reader, stopped) = reader().await;
        let (waiting_tx, waiting_rx) = oneshot::channel();
        let producer = tokio::spawn(async move {
            let mut report = Box::pin(report_integrity_loss(&mut tx, &mut reader, loss(lost)));
            assert!(futures::poll!(&mut report).is_pending());
            waiting_tx.send(()).unwrap();
            report.await.unwrap();
            // Reliable send may wait for a flush; consumer controls progress.
            send_events(
                &mut tx,
                [Event::Disconnected(scope(), "new generation".into())],
            )
            .unwrap();
        });
        completes(waiting_rx).await.unwrap();
        completes(stopped).await.unwrap();
        assert!(matches!(rx.next().await, Some(Event::Connected(_))));
        assert!(matches!(rx.next().await, Some(Event::Connected(_))));
        let Some(Event::IntegrityLost(marker)) = rx.next().await else {
            panic!("missing marker")
        };
        assert_eq!(marker.session, WsSessionId(7));
        assert_eq!(marker.generation, WsGeneration(3));
        assert_eq!(marker.lost_events.len(), 14);
        assert!(marker.auxiliary);
        assert!(!marker.complete_loss_extent_known);
        assert!(matches!(
            completes(rx.next()).await,
            Some(Event::Disconnected(_, _))
        ));
        completes(producer).await.unwrap();
        assert!(rx.next().await.is_none());
    }

    #[cfg(not(feature = "unbounded-channel"))]
    #[tokio::test]
    async fn receiver_closure_wakes_pending_marker_send() {
        let (mut tx, rx) = channel(1);
        let lost = full(&mut tx);
        let mut reader = SessionReader::default();
        let mut report = Box::pin(report_integrity_loss(&mut tx, &mut reader, loss(lost)));
        assert!(futures::poll!(&mut report).is_pending());
        drop(rx);
        assert!(matches!(
            completes(report).await,
            Err(DeliveryFailure::Closed)
        ));
    }

    #[tokio::test]
    async fn closed_is_distinct_from_full() {
        let (mut tx, rx) = channel(1);
        drop(rx);
        assert!(matches!(
            send_events(&mut tx, [Event::Connected(scope())]),
            Err(DeliveryFailure::Closed)
        ));
    }

    #[cfg(not(feature = "unbounded-channel"))]
    #[tokio::test]
    async fn dropping_subscription_cancels_pending_marker() {
        let (mut tx, rx) = channel(1);
        let lost = full(&mut tx);
        let (waiting_tx, waiting_rx) = oneshot::channel();
        let (cancelled_tx, cancelled_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _guard = OnDrop(Some(cancelled_tx));
            let mut reader = SessionReader::default();
            let mut report = Box::pin(report_integrity_loss(&mut tx, &mut reader, loss(lost)));
            assert!(futures::poll!(&mut report).is_pending());
            waiting_tx.send(()).unwrap();
            report.await.unwrap();
        });
        let stream = ChannelStream { receiver: rx, task };
        completes(waiting_rx).await.unwrap();
        drop(stream);
        completes(cancelled_rx).await.unwrap();
    }

    #[tokio::test]
    async fn dropping_subscription_also_cancels_running_reader() {
        let (reader, stopped) = reader().await;
        let (tx, rx) = channel::<Event>(1);
        let (started_tx, started_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _reader = reader;
            let _tx = tx;
            started_tx.send(()).unwrap();
            futures::future::pending::<()>().await;
        });
        let stream = ChannelStream { receiver: rx, task };
        completes(started_rx).await.unwrap();
        drop(stream);
        completes(stopped).await.unwrap();
    }

    #[test]
    fn lost_trade_bounds_use_payload_min_max_not_bucket_time() {
        let scopes = scope();
        let trade = |at| Trade {
            id: Some(at),
            time: UnixMs::new(at),
            is_sell: false,
            price: crate::Price::from_f64(100.0),
            qty: crate::unit::Qty::from_f64(1.0),
        };
        let summary = lost_event(Event::TradesReceived(
            scopes[0],
            UnixMs::new(99),
            vec![trade(30), trade(10), trade(20)].into_boxed_slice(),
        ));
        assert_eq!(summary.kind, LostEventKind::Trades);
        assert_eq!(summary.records, 3);
        assert_eq!(summary.first_known_utc, Some(UnixMs::new(10)));
        assert_eq!(summary.last_known_utc, Some(UnixMs::new(30)));
        assert_eq!(&*summary.streams, &*scopes);
    }

    #[cfg(not(feature = "unbounded-channel"))]
    #[tokio::test]
    async fn overflow_keeps_unsent_suffix_and_other_subscription_is_independent() {
        let (mut tx, mut rx) = channel(1);
        let (mut other_tx, mut other_rx) = channel(1);
        let _ = full(&mut tx);
        let second = StreamKind::Trades {
            ticker_info: TickerInfo::new(
                Ticker::new("ETHUSDT", crate::adapter::Exchange::BinanceLinear),
                0.01,
                0.001,
                None,
            ),
        };
        let events = [
            Event::OrderFlow(crate::orderflow::OrderFlowEvent::Quality {
                ticker_info: scope()[0].ticker_info(),
                at: UnixMs::new(30),
                quality: crate::orderflow::OrderFlowDataQuality::Gap,
            }),
            Event::LiquidationsReceived(second, Box::new([])),
            Event::Disconnected(Arc::from([second]), "closed".into()),
        ];
        let Err(DeliveryFailure::Full(lost)) = send_events(&mut tx, events) else {
            panic!("not full")
        };
        assert_eq!(lost.len(), 3);
        assert_eq!(lost[0].ticker_info, Some(scope()[0].ticker_info()));
        assert_eq!(lost[0].first_known_utc, Some(UnixMs::new(30)));
        assert_eq!(lost[1].kind, LostEventKind::Liquidations);
        assert_eq!(&*lost[1].streams, &[second]);
        assert_eq!(lost[1].first_known_utc, None);
        assert_eq!(lost[2].kind, LostEventKind::Disconnected);
        send_events(&mut other_tx, [Event::Connected(Arc::from([second]))]).unwrap();
        assert!(matches!(other_rx.next().await, Some(Event::Connected(_))));
        // Failed events did not enter the queue, including the unattempted suffix.
        assert!(matches!(rx.next().await, Some(Event::Connected(_))));
        assert!(matches!(rx.next().await, Some(Event::Connected(_))));
        assert!(rx.try_recv().is_none());
    }

    #[tokio::test]
    async fn ws03_proof_follows_payload_with_exact_source() {
        let (mut tx, mut rx) = channel(8);
        let source = WsSource {
            session: WsSessionId(8),
            generation: WsGeneration(4),
            auxiliary: false,
        };
        let trade = Trade {
            id: Some(1),
            time: UnixMs::new(100),
            is_sell: false,
            price: crate::Price::from_f64(100.0),
            qty: crate::unit::Qty::from_f64(1.0),
        };
        let mut published = Vec::new();
        send_generation_events(
            &mut tx,
            vec![Event::TradesReceived(
                scope()[0],
                UnixMs::new(99),
                Box::new([trade]),
            )],
            source,
            None,
            &mut published,
        )
        .unwrap();
        assert!(matches!(rx.next().await, Some(Event::TradesReceived(..))));
        let Some(Event::RecoveryEvidence(proof)) = rx.next().await else {
            panic!("missing proof")
        };
        assert_eq!(proof.source, source);
        assert_eq!(
            proof.proof,
            RecoveryProof::TradesResumed {
                first: UnixMs::new(100),
                last: UnixMs::new(100)
            }
        );
        send_generation_events(
            &mut tx,
            vec![Event::TradesReceived(
                scope()[0],
                UnixMs::new(99),
                Box::new([trade]),
            )],
            source,
            None,
            &mut published,
        )
        .unwrap();
        assert!(matches!(rx.next().await, Some(Event::TradesReceived(..))));
        assert!(rx.try_recv().is_none());
    }

    #[cfg(not(feature = "unbounded-channel"))]
    #[tokio::test]
    async fn ws03_proof_overflow_is_reported_as_loss_not_a_successful_boundary() {
        let (mut tx, mut rx) = channel(0);
        let source = WsSource {
            session: WsSessionId(8),
            generation: WsGeneration(4),
            auxiliary: false,
        };
        let trade = Trade {
            id: Some(1),
            time: UnixMs::new(100),
            is_sell: false,
            price: crate::Price::from_f64(100.0),
            qty: crate::unit::Qty::from_f64(1.0),
        };
        let mut published = Vec::new();
        let Err(DeliveryFailure::Full(lost)) = send_generation_events(
            &mut tx,
            vec![Event::TradesReceived(
                scope()[0],
                UnixMs::new(99),
                Box::new([trade]),
            )],
            source,
            None,
            &mut published,
        ) else {
            panic!("expected proof overflow")
        };
        assert!(published.is_empty());
        assert_eq!(lost.len(), 1);
        assert_eq!(lost[0].kind, LostEventKind::RecoveryEvidence);
        assert!(matches!(rx.next().await, Some(Event::TradesReceived(..))));
        assert!(rx.try_recv().is_none());
    }

    #[tokio::test]
    async fn ws03_book_proof_requires_matching_delivered_snapshot() {
        let (mut tx, mut rx) = channel(8);
        let source = WsSource {
            session: WsSessionId(8),
            generation: WsGeneration(4),
            auxiliary: false,
        };
        let stream = StreamKind::Depth {
            ticker_info: scope()[0].ticker_info(),
            depth_aggr: Default::default(),
            push_freq: Default::default(),
        };
        let book = Some((stream, UnixMs::new(1000), 9));
        let mut published = Vec::new();
        send_generation_events(&mut tx, vec![], source, book, &mut published).unwrap();
        assert!(rx.try_recv().is_none());
        send_generation_events(
            &mut tx,
            vec![Event::DepthReceived(
                stream,
                UnixMs::new(1000),
                Arc::new(crate::depth::Depth::default()),
            )],
            source,
            book,
            &mut published,
        )
        .unwrap();
        assert!(matches!(rx.next().await, Some(Event::DepthReceived(..))));
        let Some(Event::RecoveryEvidence(proof)) = rx.next().await else {
            panic!()
        };
        assert_eq!(proof.source, source);
        assert_eq!(
            proof.proof,
            RecoveryProof::BookSynchronized {
                at: UnixMs::new(1000),
                sequence: 9
            }
        );
    }

    #[cfg(feature = "unbounded-channel")]
    #[tokio::test]
    async fn existing_unbounded_feature_keeps_order_and_reports_closed() {
        let (mut tx, mut rx) = channel(1);
        send_events(&mut tx, (0..1024).map(|_| Event::Connected(scope()))).unwrap();
        for _ in 0..1024 {
            assert!(matches!(rx.next().await, Some(Event::Connected(_))));
        }
        drop(rx);
        assert!(matches!(
            send_events(&mut tx, [Event::Connected(scope())]),
            Err(DeliveryFailure::Closed)
        ));
    }
}
