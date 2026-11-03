use crate::{
    Kline, OpenInterest, Price, Qty, Ticker, TickerInfo, TickerStats, Timeframe, Trade, UnixMs,
    depth::{DeOrder, DepthPayload},
    serde_util,
    serde_util::de_string_to_number,
    unit::qty::{QtyNormalization, QuantityUnit, SizeUnit, volume_size_unit},
};

use super::{
    BinanceLimiter, HttpHub, INVERSE_PERP_DOMAIN, LINEAR_PERP_DOMAIN, MarketKind, SPOT_DOMAIN,
    THIRTY_DAYS_MS, exchange_from_market_type, raw_qty_unit_from_market_type,
};
use crate::adapter::hub::AdapterError;
use crate::adapter::{KlinePage, kline_page_end};
use csv::ReaderBuilder;
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::HashMap,
    io::BufReader,
    path::PathBuf,
    sync::{LazyLock, Mutex},
    time::UNIX_EPOCH,
    time::{Duration, Instant},
};

const METADATA_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Binance can publish a daily archive after the UTC day has already closed.
/// Pause after a 404, but recheck later: an archive may appear while we run.
const MISSING_ARCHIVE_RECHECK: Duration = Duration::from_secs(5 * 60);
static MISSING_HIST_TRADE_ARCHIVES: LazyLock<Mutex<HashMap<String, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn historical_trade_archive_is_missing(path: &str) -> bool {
    historical_trade_archive_is_missing_at(path, Instant::now())
}

fn historical_trade_archive_is_missing_at(path: &str, now: Instant) -> bool {
    MISSING_HIST_TRADE_ARCHIVES.lock().is_ok_and(|mut missing| {
        missing
            .retain(|_, checked| now.saturating_duration_since(*checked) < MISSING_ARCHIVE_RECHECK);
        missing.contains_key(path)
    })
}

fn remember_missing_historical_trade_archive(path: &str) {
    if let Ok(mut missing) = MISSING_HIST_TRADE_ARCHIVES.lock() {
        missing.insert(path.to_string(), Instant::now());
    }
}

const MAX_KLINE_BARS: u64 = 1_000;

#[derive(Deserialize, Debug, Clone)]
pub(super) struct FetchedKline(
    u64,
    #[serde(deserialize_with = "de_string_to_number")] f64,
    #[serde(deserialize_with = "de_string_to_number")] f64,
    #[serde(deserialize_with = "de_string_to_number")] f64,
    #[serde(deserialize_with = "de_string_to_number")] f64,
    #[serde(deserialize_with = "de_string_to_number")] f64,
    u64,
    #[serde(deserialize_with = "de_string_to_number")] f64,
    u32,
    #[serde(deserialize_with = "de_string_to_number")] f64,
    #[serde(deserialize_with = "de_string_to_number")] f64,
    String,
);

impl FetchedKline {
    pub(super) fn into_candle(self) -> super::kline::BinanceCandle {
        let Self(
            time,
            open,
            high,
            low,
            close,
            native_total,
            _close_time,
            secondary_total,
            _number_of_trades,
            native_buy,
            secondary_buy,
            _ignore,
        ) = self;
        super::kline::BinanceCandle {
            time,
            open,
            high,
            low,
            close,
            native_total,
            native_buy,
            secondary_total,
            secondary_buy,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeOpenInterest {
    #[serde(rename = "timestamp")]
    pub time: u64,
    #[serde(rename = "sumOpenInterest", deserialize_with = "de_string_to_number")]
    pub sum: f64,
}

#[derive(Deserialize, Debug)]
struct DeTrade {
    #[serde(rename = "a")]
    id: u64,
    #[serde(rename = "T")]
    time: u64,
    #[serde(rename = "p", deserialize_with = "de_string_to_number")]
    price: f64,
    #[serde(rename = "q", deserialize_with = "de_string_to_number")]
    qty: f64,
    #[serde(rename = "m")]
    is_sell: bool,
}

#[derive(Deserialize, Clone)]
struct FetchedPerpDepth {
    #[serde(rename = "lastUpdateId")]
    update_id: u64,
    #[serde(rename = "T")]
    time: u64,
    #[serde(rename = "bids")]
    bids: Vec<DeOrder>,
    #[serde(rename = "asks")]
    asks: Vec<DeOrder>,
}

#[derive(Deserialize, Clone)]
struct FetchedSpotDepth {
    #[serde(rename = "lastUpdateId")]
    update_id: u64,
    #[serde(rename = "bids")]
    bids: Vec<DeOrder>,
    #[serde(rename = "asks")]
    asks: Vec<DeOrder>,
}

pub(super) async fn fetch_depth_snapshot(
    hub: &mut HttpHub<BinanceLimiter>,
    ticker: Ticker,
) -> Result<DepthPayload, AdapterError> {
    let (symbol_str, market_type) = ticker.to_full_symbol_and_type();

    let base_url = match market_type {
        MarketKind::Spot => format!("{SPOT_DOMAIN}/api/v3/depth"),
        MarketKind::LinearPerps => format!("{LINEAR_PERP_DOMAIN}/fapi/v1/depth"),
        MarketKind::InversePerps => format!("{INVERSE_PERP_DOMAIN}/dapi/v1/depth"),
    };

    let depth_limit = match market_type {
        MarketKind::Spot => 5000,
        MarketKind::LinearPerps | MarketKind::InversePerps => 1000,
    };

    let url = format!(
        "{}?symbol={}&limit={}",
        base_url,
        symbol_str.to_uppercase(),
        depth_limit
    );

    let weight = match market_type {
        MarketKind::Spot => match depth_limit {
            ..=100_i32 => 5,
            101_i32..=500_i32 => 25,
            501_i32..=1000_i32 => 50,
            1001_i32..=5000_i32 => 250,
            _ => {
                return Err(AdapterError::InvalidRequest(format!(
                    "Unsupported depth limit for spot market: {depth_limit}"
                )));
            }
        },
        MarketKind::LinearPerps | MarketKind::InversePerps => match depth_limit {
            ..100 => 2,
            100 => 5,
            500 => 10,
            1000 => 20,
            _ => {
                return Err(AdapterError::InvalidRequest(format!(
                    "Unsupported depth limit for perps market: {depth_limit}"
                )));
            }
        },
    };

    let text = hub.http_text_with_limiter(&url, weight, None, None).await?;

    match market_type {
        MarketKind::Spot => {
            let fetched_depth: FetchedSpotDepth =
                serde_json::from_str(&text).map_err(|e| AdapterError::ParseError(e.to_string()))?;

            Ok(DepthPayload {
                last_update_id: fetched_depth.update_id,
                time: (chrono::Utc::now().timestamp_millis() as u64).into(),
                bids: fetched_depth
                    .bids
                    .iter()
                    .map(|x| DeOrder {
                        price: x.price,
                        qty: x.qty,
                    })
                    .collect(),
                asks: fetched_depth
                    .asks
                    .iter()
                    .map(|x| DeOrder {
                        price: x.price,
                        qty: x.qty,
                    })
                    .collect(),
            })
        }
        MarketKind::LinearPerps | MarketKind::InversePerps => {
            let fetched_depth: FetchedPerpDepth =
                serde_json::from_str(&text).map_err(|e| AdapterError::ParseError(e.to_string()))?;

            Ok(DepthPayload {
                last_update_id: fetched_depth.update_id,
                time: fetched_depth.time.into(),
                bids: fetched_depth
                    .bids
                    .iter()
                    .map(|x| DeOrder {
                        price: x.price,
                        qty: x.qty,
                    })
                    .collect(),
                asks: fetched_depth
                    .asks
                    .iter()
                    .map(|x| DeOrder {
                        price: x.price,
                        qty: x.qty,
                    })
                    .collect(),
            })
        }
    }
}

pub(super) async fn fetch_ticker_metadata(
    hub: &mut HttpHub<BinanceLimiter>,
    market: MarketKind,
) -> Result<super::super::TickerMetadataMap, AdapterError> {
    let (url, weight) = match market {
        MarketKind::Spot => (
            format!("{SPOT_DOMAIN}/api/v3/exchangeInfo?showPermissionSets=false"),
            20,
        ),
        MarketKind::LinearPerps => (format!("{LINEAR_PERP_DOMAIN}/fapi/v1/exchangeInfo"), 1),
        MarketKind::InversePerps => (format!("{INVERSE_PERP_DOMAIN}/dapi/v1/exchangeInfo"), 1),
    };

    let response_text = hub
        .http_text_with_limiter_timeout(&url, weight, None, None, METADATA_REQUEST_TIMEOUT)
        .await?;

    let exchange_info: Value = serde_json::from_str(&response_text)
        .map_err(|e| AdapterError::ParseError(format!("Failed to parse exchange info: {e}")))?;

    let symbols = exchange_info["symbols"]
        .as_array()
        .ok_or_else(|| AdapterError::ParseError("Missing symbols array".to_string()))?;

    let exchange = exchange_from_market_type(market);
    let mut ticker_info_map = HashMap::new();

    for item in symbols {
        let symbol_str = item["symbol"]
            .as_str()
            .ok_or_else(|| AdapterError::ParseError("Missing symbol".to_string()))?;

        if !exchange.is_symbol_supported(symbol_str, true) {
            continue;
        }

        if let Some(contract_type) = item["contractType"].as_str()
            && contract_type != "PERPETUAL"
        {
            continue;
        }
        if let Some(quote_asset) = item["quoteAsset"].as_str()
            && quote_asset != "USDT"
            && quote_asset != "USDC"
            && quote_asset != "USD"
        {
            continue;
        }
        if let Some(status) = item["status"].as_str()
            && status != "TRADING"
            && status != "HALT"
        {
            continue;
        }

        let filters = item["filters"]
            .as_array()
            .ok_or_else(|| AdapterError::ParseError("Missing filters array".to_string()))?;

        let price_filter = filters
            .iter()
            .find(|x| x["filterType"].as_str().unwrap_or_default() == "PRICE_FILTER");

        let min_qty = filters
            .iter()
            .find(|x| x["filterType"].as_str().unwrap_or_default() == "LOT_SIZE")
            .ok_or_else(|| {
                AdapterError::ParseError("Missing minQty in LOT_SIZE filter".to_string())
            })
            .and_then(|x| {
                serde_util::value_as_f32(&x["minQty"])
                    .ok_or_else(|| AdapterError::ParseError("Failed to parse minQty".to_string()))
            })?;

        let contract_size = serde_util::value_as_f32(&item["contractSize"]);

        let ticker = Ticker::new(symbol_str, exchange);

        if let Some(price_filter) = price_filter {
            let min_ticksize = serde_util::value_as_f32(&price_filter["tickSize"])
                .ok_or_else(|| AdapterError::ParseError("tickSize not found".to_string()))?;

            let info = TickerInfo::new(ticker, min_ticksize, min_qty, contract_size);
            ticker_info_map.insert(ticker, Some(info));
        } else {
            ticker_info_map.insert(ticker, None);
        }
    }

    Ok(ticker_info_map)
}

pub(super) async fn fetch_ticker_stats(
    hub: &mut HttpHub<BinanceLimiter>,
    market: MarketKind,
    contract_sizes: Option<&HashMap<Ticker, crate::unit::ContractSize>>,
) -> Result<super::super::TickerStatsMap, AdapterError> {
    let (url, weight) = match market {
        MarketKind::Spot => (format!("{SPOT_DOMAIN}/api/v3/ticker/24hr"), 80),
        MarketKind::LinearPerps => (format!("{LINEAR_PERP_DOMAIN}/fapi/v1/ticker/24hr"), 40),
        MarketKind::InversePerps => (format!("{INVERSE_PERP_DOMAIN}/dapi/v1/ticker/24hr"), 40),
    };

    let parsed_response: Vec<Value> = hub.http_json_with_limiter(&url, weight, None, None).await?;

    let exchange = exchange_from_market_type(market);
    let mut ticker_price_map = HashMap::new();

    for item in parsed_response {
        let symbol = item["symbol"]
            .as_str()
            .ok_or_else(|| AdapterError::ParseError("Symbol not found".to_string()))?;

        if !exchange.is_symbol_supported(symbol, false) {
            continue;
        }

        let ticker = Ticker::new(symbol, exchange);

        let last_price = serde_util::value_as_f64(&item["lastPrice"])
            .ok_or_else(|| AdapterError::ParseError("Last price not found".to_string()))?;

        let price_change_pt =
            serde_util::value_as_f32(&item["priceChangePercent"]).ok_or_else(|| {
                AdapterError::ParseError("Price change percent not found".to_string())
            })?;

        let volume = match market {
            MarketKind::Spot | MarketKind::LinearPerps => {
                serde_util::value_as_f64(&item["quoteVolume"])
                    .or_else(|| serde_util::value_as_f64(&item["volume"]))
            }
            _ => serde_util::value_as_f64(&item["volume"]),
        }
        .ok_or_else(|| AdapterError::ParseError("Volume not found".to_string()))?;

        let daily_volume = match market {
            MarketKind::Spot | MarketKind::LinearPerps => Qty::from_f64(volume),
            MarketKind::InversePerps => {
                let contract_size = match contract_sizes
                    .and_then(|sizes| sizes.get(&ticker))
                    .copied()
                {
                    Some(size) => size.as_f64(),
                    None => {
                        log::debug!("Missing contract size for {ticker}, skipping ticker in stats");
                        continue;
                    }
                };

                Qty::from_f64(volume * contract_size)
            }
        };

        let ticker_stats = TickerStats {
            mark_price: Price::from_f64(last_price),
            daily_price_chg: price_change_pt,
            daily_volume,
        };

        ticker_price_map.insert(ticker, ticker_stats);
    }

    Ok(ticker_price_map)
}

pub(super) async fn fetch_klines(
    hub: &mut HttpHub<BinanceLimiter>,
    ticker_info: TickerInfo,
    timeframe: Timeframe,
    range: Option<(UnixMs, UnixMs)>,
) -> Result<Vec<Kline>, AdapterError> {
    let ticker = ticker_info.ticker;

    let (symbol_str, market_type) = ticker.to_full_symbol_and_type();
    let timeframe_str = timeframe.to_string();

    let base_url = match market_type {
        MarketKind::Spot => format!("{SPOT_DOMAIN}/api/v3/klines"),
        MarketKind::LinearPerps => format!("{LINEAR_PERP_DOMAIN}/fapi/v1/klines"),
        MarketKind::InversePerps => format!("{INVERSE_PERP_DOMAIN}/dapi/v1/klines"),
    };

    let mut url = format!("{base_url}?symbol={symbol_str}&interval={timeframe_str}");

    let limit_param = if let Some((start, end)) = range {
        let start = start.as_u64();
        let end = end.as_u64();
        let interval_ms = timeframe.to_milliseconds();
        let num_intervals = ((end - start) / interval_ms + 1).min(MAX_KLINE_BARS);

        if num_intervals < 3 {
            let new_start = start.saturating_sub(interval_ms * 5);
            let new_end = end.saturating_add(interval_ms * 5);
            let num_intervals = ((new_end - new_start) / interval_ms + 1).min(MAX_KLINE_BARS);

            url.push_str(&format!(
                "&startTime={new_start}&endTime={new_end}&limit={num_intervals}"
            ));
            num_intervals
        } else {
            url.push_str(&format!(
                "&startTime={start}&endTime={end}&limit={num_intervals}"
            ));
            num_intervals
        }
    } else {
        let num_intervals = 400;
        url.push_str(&format!("&limit={num_intervals}"));
        num_intervals
    };

    let weight = match market_type {
        MarketKind::Spot => 2,
        MarketKind::LinearPerps | MarketKind::InversePerps => match limit_param {
            1..=100 => 1,
            101..=500 => 2,
            501..=1000 => 5,
            1001..=1500 => 10,
            _ => {
                return Err(AdapterError::InvalidRequest(format!(
                    "Unsupported kline limit parameter for perps market: {limit_param}"
                )));
            }
        },
    };

    let fetched_klines: Vec<FetchedKline> =
        hub.http_json_with_limiter(&url, weight, None, None).await?;
    let unit = QuantityUnit::for_market(market_type, volume_size_unit());
    fetched_klines
        .into_iter()
        .map(|k| k.into_candle().into_kline(ticker_info, unit))
        .collect()
}

pub(super) async fn fetch_klines_page(
    hub: &mut HttpHub<BinanceLimiter>,
    ticker_info: TickerInfo,
    timeframe: Timeframe,
    from: UnixMs,
    to: UnixMs,
) -> Result<KlinePage, AdapterError> {
    let page_to = kline_page_end(from, to, timeframe, MAX_KLINE_BARS)?;
    let data = fetch_klines(
        hub,
        ticker_info,
        timeframe,
        Some((from, page_to.saturating_sub(1))),
    )
    .await?;
    Ok(KlinePage::new(data, from, page_to))
}

pub(super) async fn fetch_historical_oi(
    hub: &mut HttpHub<BinanceLimiter>,
    ticker_info: TickerInfo,
    range: Option<(UnixMs, UnixMs)>,
    period: Timeframe,
) -> Result<Vec<OpenInterest>, AdapterError> {
    let (ticker_str, market) = ticker_info.ticker.to_full_symbol_and_type();
    let period_str = period.to_string();

    let (base_url, pair_str, weight) = match market {
        MarketKind::LinearPerps => (
            format!("{LINEAR_PERP_DOMAIN}/futures/data/openInterestHist"),
            format!("?symbol={ticker_str}"),
            12,
        ),
        MarketKind::InversePerps => {
            let Some((pair, _)) = ticker_str.split_once('_') else {
                let err_msg =
                    format!("Unsupported inverse ticker format for open interest: {ticker_str}");
                log::error!("{err_msg}");
                return Err(AdapterError::InvalidRequest(err_msg));
            };

            (
                format!("{INVERSE_PERP_DOMAIN}/futures/data/openInterestHist"),
                format!("?pair={pair}&contractType=PERPETUAL"),
                1,
            )
        }
        _ => {
            let err_msg = format!("Unsupported market type for open interest: {market:?}");
            log::error!("{}", err_msg);
            return Err(AdapterError::InvalidRequest(err_msg));
        }
    };

    let mut url = format!("{base_url}{pair_str}&period={period_str}");

    if let Some((start, end)) = range {
        let start = start.as_u64();
        let end = end.as_u64();
        let now_ms = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|err| {
                AdapterError::ParseError(format!("System clock before UNIX_EPOCH: {err}"))
            })?
            .as_millis() as u64;
        // An exact 30-day start is already "invalid" (-1130) once the request reaches Binance
        // (and on retries); keep an hour of margin.
        let thirty_days_ago = now_ms.saturating_sub(THIRTY_DAYS_MS) + 60 * 60 * 1000;

        // Scrolling a chart back past Binance's 30 days of OI history is normal, not an
        // error worth a toast: there is simply nothing to show there.
        if end < thirty_days_ago {
            log::debug!("OI range ends at {end}, before Binance's 30-day history; skipped");
            return Ok(Vec::new());
        }

        let adjusted_start = if start < thirty_days_ago {
            log::warn!(
                "Adjusting start time from {} to {} (30 days limit)",
                start,
                thirty_days_ago
            );
            thirty_days_ago
        } else {
            start
        };

        let interval_ms = period.to_milliseconds();
        let num_intervals = oi_request_limit(adjusted_start, end, interval_ms, 500);

        url.push_str(&format!(
            "&startTime={adjusted_start}&endTime={end}&limit={num_intervals}"
        ));
    } else {
        url.push_str("&limit=400");
    }

    let binance_oi: Vec<DeOpenInterest> =
        hub.http_json_with_limiter(&url, weight, None, None).await?;

    let contract_size = ticker_info.contract_size;
    let open_interest = binance_oi
        .iter()
        .map(|x| OpenInterest {
            time: x.time.into(),
            value: contract_size.map_or(x.sum, |size| x.sum * size.as_f64()),
        })
        .collect::<Vec<OpenInterest>>();

    Ok(open_interest)
}

fn oi_request_limit(start: u64, end: u64, interval_ms: u64, maximum: u64) -> u64 {
    // Both API bounds are inclusive; count grid timestamps, not intervals.
    (end / interval_ms)
        .saturating_sub(start.div_ceil(interval_ms))
        .saturating_add(1)
        .clamp(1, maximum)
}

async fn fetch_intraday_trades(
    hub: &mut HttpHub<BinanceLimiter>,
    ticker_info: TickerInfo,
    from: UnixMs,
) -> Result<Vec<Trade>, AdapterError> {
    complete_intraday_trade_page(
        hub,
        from,
        |hub, cursor| Box::pin(fetch_recovery_trade_page(hub, ticker_info, cursor)),
        |trade| (trade.id, trade.time),
    )
    .await
}

async fn complete_intraday_trade_page<C, T>(
    context: &mut C,
    from: UnixMs,
    mut fetch: impl for<'a> FnMut(
        &'a mut C,
        crate::adapter::TradePageCursor,
    ) -> std::pin::Pin<
        Box<dyn Future<Output = Result<Vec<T>, AdapterError>> + Send + 'a>,
    >,
    key: impl Fn(&T) -> (Option<u64>, UnixMs),
) -> Result<Vec<T>, AdapterError> {
    use crate::adapter::TradePageCursor;

    let mut trades = fetch(context, TradePageCursor::Time(from)).await?;
    if trades.len() < AGG_TRADE_PAGE_LIMIT {
        return Ok(trades);
    }

    // The caller marks the last millisecond covered. Finish that millisecond
    // before returning the batch, even if it spans several REST pages.
    for _ in 0..128 {
        let (last_id, last_time) = key(trades.last().expect("saturated trade page"));
        let next = last_id.and_then(|id| id.checked_add(1)).ok_or_else(|| {
            AdapterError::ParseError("Invalid Binance aggregate trade cursor".into())
        })?;
        let page = fetch(context, TradePageCursor::AggregateId(next)).await?;
        let page_len = page.len();
        let mut expected = next;
        for trade in page {
            let (id, time) = key(&trade);
            if id != Some(expected) || time < last_time {
                return Err(AdapterError::ParseError(
                    "Binance aggregate trade continuation is discontinuous".into(),
                ));
            }
            if time > last_time {
                return Ok(trades);
            }
            trades.push(trade);
            expected = expected.checked_add(1).ok_or_else(|| {
                AdapterError::ParseError("Binance aggregate trade cursor overflow".into())
            })?;
        }
        if page_len < AGG_TRADE_PAGE_LIMIT {
            return Err(AdapterError::ParseError(
                "Binance aggregate trade continuation has no later timestamp".into(),
            ));
        }
    }
    Err(AdapterError::ParseError(
        "Binance aggregate trade continuation exceeded page limit".into(),
    ))
}

const AGG_TRADE_PAGE_LIMIT: usize = 1000;

/// Recovery uses an inclusive ID cursor after the first time-based page. Never
/// advance a timestamp past a saturated millisecond. See Binance aggTrades API.
pub(super) async fn fetch_recovery_trade_page(
    hub: &mut HttpHub<BinanceLimiter>,
    ticker_info: TickerInfo,
    cursor: crate::adapter::TradePageCursor,
) -> Result<Vec<Trade>, AdapterError> {
    let ticker = ticker_info.ticker;
    let (symbol_str, market_type) = ticker.to_full_symbol_and_type();

    let (base_url, weight) = match market_type {
        MarketKind::Spot => (format!("{SPOT_DOMAIN}/api/v3/aggTrades"), 4),
        MarketKind::LinearPerps => (format!("{LINEAR_PERP_DOMAIN}/fapi/v1/aggTrades"), 20),
        MarketKind::InversePerps => (format!("{INVERSE_PERP_DOMAIN}/dapi/v1/aggTrades"), 20),
    };

    let mut url = format!("{base_url}?symbol={symbol_str}&limit={AGG_TRADE_PAGE_LIMIT}");
    match cursor {
        crate::adapter::TradePageCursor::Time(from) => {
            url.push_str(&format!("&startTime={}", from.as_u64()))
        }
        crate::adapter::TradePageCursor::AggregateId(id) => url.push_str(&format!("&fromId={id}")),
    }

    let de_trades: Vec<DeTrade> = hub.http_json_with_limiter(&url, weight, None, None).await?;

    let qty_norm = QtyNormalization::with_raw_qty_unit(
        volume_size_unit() == SizeUnit::Quote,
        ticker_info,
        raw_qty_unit_from_market_type(market_type),
    );

    let trades = de_trades
        .into_iter()
        .map(|de_trade| Trade {
            id: Some(de_trade.id),
            time: de_trade.time.into(),
            is_sell: de_trade.is_sell,
            price: Price::from_f64(de_trade.price).round_to_min_tick(ticker_info.min_ticksize),
            qty: qty_norm.normalize_qty(de_trade.qty, de_trade.price),
        })
        .collect();

    Ok(trades)
}

/// Spot archives since 2025 store microseconds, futures archives milliseconds.
fn archive_time_ms(raw: u64) -> u64 {
    // Milliseconds stay below 1e14 until the year 5138.
    if raw >= 100_000_000_000_000 {
        raw / 1_000
    } else {
        raw
    }
}

/// Spot archives write "True"/"False", futures archives "true"/"false".
fn archive_bool(raw: &str) -> Option<bool> {
    if raw.eq_ignore_ascii_case("true") {
        Some(true)
    } else if raw.eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        None
    }
}

async fn get_hist_trades_with_client(
    client: &reqwest::Client,
    ticker_info: TickerInfo,
    date: chrono::NaiveDate,
    base_path: PathBuf,
) -> Result<Vec<Trade>, AdapterError> {
    let ticker = ticker_info.ticker;
    let (symbol, market_type) = ticker.to_full_symbol_and_type();

    let market_subpath = match market_type {
        MarketKind::Spot => format!("data/spot/daily/aggTrades/{symbol}"),
        MarketKind::LinearPerps => format!("data/futures/um/daily/aggTrades/{symbol}"),
        MarketKind::InversePerps => format!("data/futures/cm/daily/aggTrades/{symbol}"),
    };

    let zip_file_name = format!(
        "{}-aggTrades-{}.zip",
        symbol.to_uppercase(),
        date.format("%Y-%m-%d"),
    );

    let base_path = base_path.join(&market_subpath);

    std::fs::create_dir_all(&base_path)
        .map_err(|e| AdapterError::ParseError(format!("Failed to create directories: {e}")))?;

    let zip_path = format!("{market_subpath}/{zip_file_name}");
    let base_zip_path = base_path.join(&zip_file_name);

    if std::fs::metadata(&base_zip_path).is_ok() {
        log::info!("Using cached {}", zip_path);
    } else {
        if historical_trade_archive_is_missing(&zip_path) {
            return Err(AdapterError::InvalidRequest(format!(
                "Historical archive is not available yet: {zip_path}"
            )));
        }

        let url = format!("https://data.binance.vision/{zip_path}");

        log::info!("Downloading from {}", url);

        let resp = client.get(&url).send().await.map_err(AdapterError::from)?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            remember_missing_historical_trade_archive(&zip_path);
        }
        if !resp.status().is_success() {
            return Err(AdapterError::InvalidRequest(format!(
                "Failed to fetch from {}: {}",
                url,
                resp.status()
            )));
        }

        let body = resp.bytes().await.map_err(AdapterError::from)?;

        std::fs::write(&base_zip_path, &body).map_err(|e| {
            AdapterError::ParseError(format!("Failed to write zip file: {e}, {base_zip_path:?}"))
        })?;
    }

    let file = std::fs::File::open(&base_zip_path)
        .map_err(|e| AdapterError::ParseError(format!("Failed to open compressed file: {e}")))?;

    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| AdapterError::ParseError(format!("Failed to unzip file: {e}")))?;

    let qty_norm = QtyNormalization::with_raw_qty_unit(
        volume_size_unit() == SizeUnit::Quote,
        ticker_info,
        raw_qty_unit_from_market_type(market_type),
    );

    let mut trades = Vec::new();
    for i in 0..archive.len() {
        let csv_file = archive
            .by_index(i)
            .map_err(|e| AdapterError::ParseError(format!("Failed to read csv: {e}")))?;

        let mut csv_reader = ReaderBuilder::new()
            .has_headers(false)
            .from_reader(BufReader::new(csv_file));

        trades.extend(csv_reader.records().filter_map(|record| {
            record.ok().and_then(|record| {
                let id = record.get(0)?.parse::<u64>().ok()?;
                let time = archive_time_ms(record.get(5)?.parse::<u64>().ok()?);
                let is_sell = archive_bool(record.get(6)?)?;
                let price_f64 = record.get(1)?.parse::<f64>().ok()?;

                let price = Price::from_f64(price_f64).round_to_min_tick(ticker_info.min_ticksize);
                let qty = qty_norm.normalize_qty(record.get(2)?.parse::<f64>().ok()?, price_f64);

                Some(Trade {
                    id: Some(id),
                    time: time.into(),
                    is_sell,
                    price,
                    qty,
                })
            })
        }));
    }

    Ok(trades)
}

pub(super) async fn fetch_trades(
    hub: &mut HttpHub<BinanceLimiter>,
    ticker_info: TickerInfo,
    from_time: UnixMs,
    data_path: Option<PathBuf>,
) -> Result<Vec<Trade>, AdapterError> {
    let Some(data_path) = data_path else {
        return Err(AdapterError::InvalidRequest(
            "Binance trades fetch requires data_path".to_string(),
        ));
    };

    let today_midnight = chrono::Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| {
            AdapterError::ParseError("Failed to construct UTC midnight timestamp".to_string())
        })?
        .and_utc();

    let from_time_ms = i64::try_from(from_time.as_u64())
        .map_err(|_| AdapterError::InvalidRequest("Timestamp exceeds i64 range".to_string()))?;

    if from_time_ms >= today_midnight.timestamp_millis() {
        return fetch_intraday_trades(hub, ticker_info, from_time).await;
    }

    let mut date = chrono::DateTime::from_timestamp_millis(from_time_ms)
        .ok_or_else(|| AdapterError::ParseError("Invalid timestamp".into()))?
        .date_naive();
    let today = today_midnight.date_naive();
    let now_ms = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or_default();

    let client = hub.client().clone();

    // A request starting at a day's last trade gets nothing new from that day's archive, so
    // move on to the next day's archive instead of re-reading the same one forever.
    loop {
        match get_hist_trades_with_client(&client, ticker_info, date, data_path.clone()).await {
            Ok(trades) => {
                let mut trades: Vec<Trade> =
                    trades.into_iter().filter(|t| t.time >= from_time).collect();
                let Some(latest_trade) = trades.last().copied() else {
                    date = date.succ_opt().unwrap_or(today);
                    if date < today {
                        continue;
                    }
                    ensure_rest_history_available(
                        ticker_info,
                        from_time,
                        now_ms,
                        date,
                        "No later archived trades",
                    )?;
                    return fetch_intraday_trades(hub, ticker_info, from_time).await;
                };
                // Binance REST only serves the last two days; older gaps come from the next
                // archive on the following request.
                if intraday_window_covers(latest_trade.time.as_u64(), now_ms) {
                    match fetch_intraday_trades(hub, ticker_info, latest_trade.time).await {
                        Ok(intraday_trades) => trades.extend(intraday_trades),
                        Err(e) => log::error!("Failed to fetch intraday trades: {}", e),
                    }
                }
                return Ok(trades);
            }
            Err(e) => {
                ensure_rest_history_available(
                    ticker_info,
                    from_time,
                    now_ms,
                    date,
                    &e.to_string(),
                )?;
                log::warn!(
                    "Historical trades fetch failed: {}, falling back to intraday fetch",
                    e
                );
                return fetch_intraday_trades(hub, ticker_info, from_time).await;
            }
        }
    }
}

fn ensure_rest_history_available(
    ticker: TickerInfo,
    from: UnixMs,
    now_ms: u64,
    archive_date: chrono::NaiveDate,
    detail: &str,
) -> Result<(), AdapterError> {
    if ticker.market_type() != MarketKind::Spot && !intraday_window_covers(from.as_u64(), now_ms) {
        return Err(AdapterError::HistoryUnavailable {
            symbol: ticker.ticker.to_full_symbol_and_type().0.to_string(),
            date: archive_date.to_string(),
            detail: detail.to_owned(),
        });
    }
    Ok(())
}

/// Binance futures `aggTrades` rejects start times older than two days (code -4166). Keep an
/// hour of margin for the request's own latency.
const INTRADAY_WINDOW_MS: u64 = 47 * 60 * 60 * 1000;

fn intraday_window_covers(start_ms: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(start_ms) < INTRADAY_WINDOW_MS
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn intraday_saturated_millisecond_is_completed_by_id() {
        use super::{Price, Trade, UnixMs, complete_intraday_trade_page};
        use crate::adapter::TradePageCursor;

        let from = UnixMs::new(1000);
        let trade = |id: u64, time| Trade {
            id: Some(id),
            time,
            is_sell: id % 2 == 1,
            price: Price::from_f64(100.0),
            qty: crate::unit::Qty::from_f64(1.0),
        };
        let first = (0..1000).map(|id| trade(id, from)).collect::<Vec<_>>();
        let continuation = vec![
            trade(1000, from),
            trade(1001, from),
            trade(1002, UnixMs::new(1001)),
        ];
        let run = async |repeat_page| {
            complete_intraday_trade_page(
                &mut (),
                from,
                |_, cursor| {
                    Box::pin(std::future::ready(Ok(match cursor {
                        TradePageCursor::Time(time) if time == from => first.clone(),
                        TradePageCursor::AggregateId(1000) if repeat_page => first.clone(),
                        TradePageCursor::AggregateId(1000) => continuation.clone(),
                        _ => panic!("unexpected cursor"),
                    })))
                },
                |trade| (trade.id, trade.time),
            )
            .await
        };

        assert!(run(true).await.is_err());
        let trades = run(false).await.unwrap();
        assert_eq!(
            trades
                .iter()
                .map(|trade| trade.id.unwrap())
                .collect::<Vec<_>>(),
            (0..1002).collect::<Vec<_>>()
        );
        assert!(trades.iter().all(|trade| trade.time == from));
        for is_sell in [false, true] {
            assert_eq!(
                trades
                    .iter()
                    .filter(|trade| trade.is_sell == is_sell)
                    .map(|trade| trade.qty.to_f64())
                    .sum::<f64>(),
                501.0
            );
        }
    }

    use super::{
        archive_bool, archive_time_ms, historical_trade_archive_is_missing, intraday_window_covers,
        oi_request_limit, remember_missing_historical_trade_archive,
    };

    #[test]
    fn missing_old_archive_never_falls_back_to_expired_futures_rest() {
        use crate::{Ticker, TickerInfo, UnixMs, adapter::Exchange};
        let now = 1_000 * 60 * 60 * 1000;
        let old = UnixMs::new(now - 72 * 60 * 60 * 1000);
        let recent = UnixMs::new(now - 12 * 60 * 60 * 1000);
        let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 28).unwrap();
        for exchange in [
            Exchange::BinanceLinear,
            Exchange::BinanceInverse,
            Exchange::BinanceSpot,
        ] {
            let ticker = TickerInfo::new(Ticker::new("BTCUSDT", exchange), 0.1, 0.001, None);
            let result =
                super::ensure_rest_history_available(ticker, old, now, date, "archive HTTP 404");
            if exchange == Exchange::BinanceSpot {
                assert!(result.is_ok());
            } else {
                assert!(matches!(
                    result,
                    Err(super::AdapterError::HistoryUnavailable { .. })
                ));
            }
            assert!(
                super::ensure_rest_history_available(ticker, recent, now, date, "archive HTTP 404")
                    .is_ok()
            );
        }
    }

    #[test]
    fn missing_archive_is_rechecked_after_cooldown() {
        let path = "test/archive-expiry.zip";
        let checked = std::time::Instant::now();
        super::MISSING_HIST_TRADE_ARCHIVES
            .lock()
            .unwrap()
            .insert(path.into(), checked);
        assert!(!super::historical_trade_archive_is_missing_at(
            path,
            checked + super::MISSING_ARCHIVE_RECHECK + std::time::Duration::from_secs(1)
        ));
        remember_missing_historical_trade_archive(path);
        assert!(historical_trade_archive_is_missing(path));
    }

    #[test]
    fn spot_archive_microseconds_become_milliseconds() {
        assert_eq!(archive_time_ms(1_793_059_200_482_489), 1_793_059_200_482);
        assert_eq!(archive_time_ms(1_793_059_200_482), 1_793_059_200_482);
    }

    #[test]
    fn archive_sides_parse_in_both_spellings() {
        assert_eq!(archive_bool("True"), Some(true));
        assert_eq!(archive_bool("false"), Some(false));
        assert_eq!(archive_bool("1"), None);
    }

    #[test]
    fn rest_trades_are_only_requested_within_two_days() {
        const HOUR: u64 = 60 * 60 * 1000;
        let now = 1_000 * HOUR;
        assert!(intraday_window_covers(now - 30 * HOUR, now));
        // Binance answers -4166 "restricted to recent 2 days" for these.
        assert!(!intraday_window_covers(now - 48 * HOUR, now));
        assert!(!intraday_window_covers(now - 70 * HOUR, now));
    }

    #[test]
    fn open_interest_short_range_never_uses_zero_limit() {
        assert_eq!(oi_request_limit(1_000, 1_001, 300_000, 500), 1);
        assert_eq!(oi_request_limit(1_000, 301_000, 300_000, 500), 1);
        assert_eq!(oi_request_limit(0, 300_000, 300_000, 500), 2);
        assert_eq!(oi_request_limit(0, 499 * 300_000, 300_000, 500), 500);
    }

    #[test]
    fn missing_historical_trade_archive_is_not_retried() {
        let path = "test/futures/um/daily/aggTrades/BTCUSDT/never-published.zip";
        assert!(!historical_trade_archive_is_missing(path));
        remember_missing_historical_trade_archive(path);
        assert!(historical_trade_archive_is_missing(path));
    }
}
