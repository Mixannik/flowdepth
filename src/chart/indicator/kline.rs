use crate::chart::{Basis, Message, ViewState};
use crate::connector::fetcher::FetchRange;

use data::chart::indicator::KlineIndicator;
use data::chart::kline::KlineDataPoint;
use data::chart::{BasisSeries, PlotData};
use exchange::adapter::Exchange;
use exchange::{Kline, Timeframe, Trade, UnixMs};

use super::plot::AnySeries;

pub mod bar_analysis;
pub mod htf_vwap;
pub mod open_interest;
pub mod period_levels;
pub mod scripted;
pub mod spot_cvd;
pub mod volume;

/// UI adapter methods for converting domain `BasisSeries` into plot-ready series.
trait BasisSeriesExt<T> {
    fn as_plot_series(&self) -> AnySeries<'_, T>;
}

impl<T> BasisSeriesExt<T> for BasisSeries<T> {
    fn as_plot_series(&self) -> AnySeries<'_, T> {
        match self {
            BasisSeries::Time(data) => AnySeries::forward_unix_ms(data),
            BasisSeries::Tick(data) => AnySeries::reversed_u64(data),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub enum IndicatorAvailability {
    /// Indicator can be rendered normally.
    #[default]
    Available,
    /// Availability cannot be determined yet (e.g. no datapoints loaded).
    Unknown,
    /// Indicator cannot be rendered for the current source/context.
    Unavailable(AvailabilityCause),
}

#[derive(Debug, Clone, PartialEq)]
pub enum AvailabilityCause {
    Exchange(Exchange),
    Timeframe(Timeframe),
    Basis(Basis),
    TradeData,
}

impl IndicatorAvailability {
    pub fn unavailable_message(&self, indicator: &str) -> Option<String> {
        match self {
            IndicatorAvailability::Available | IndicatorAvailability::Unknown => None,
            IndicatorAvailability::Unavailable(cause) => Some(match cause {
                AvailabilityCause::Exchange(exchange) => {
                    format!("{indicator} is not available for {exchange}.")
                }
                AvailabilityCause::Timeframe(timeframe) => {
                    format!("{indicator} is not available on {timeframe} timeframe.")
                }
                AvailabilityCause::Basis(Basis::Tick(_)) => {
                    format!("{indicator} is not available for tick charts.")
                }
                AvailabilityCause::Basis(basis) => {
                    format!("{indicator} is not available on {basis} basis.")
                }
                AvailabilityCause::TradeData => {
                    format!("{indicator} requires directional trade-volume data.")
                }
            }),
        }
    }
}

pub trait KlineIndicatorImpl {
    /// Clear all caches for a full redraw
    fn clear_all_caches(&mut self);

    /// Clear caches related to crosshair only
    /// e.g. tooltips and scale labels for a partial redraw
    fn clear_crosshair_caches(&mut self);

    fn element<'a>(
        &'a self,
        chart: &'a ViewState,
        // Whether to show last value labels on top right/left when not hovering
        data_labels_always_visible: bool,
        visible_range: std::ops::RangeInclusive<u64>,
    ) -> iced::Element<'a, Message>;

    fn availability(&self, _chart: &ViewState) -> IndicatorAvailability {
        IndicatorAvailability::Available
    }

    fn unavailable_message(&self, chart: &ViewState, indicator: &str) -> Option<String> {
        self.availability(chart).unavailable_message(indicator)
    }

    /// If the indicator needs data fetching, return the required range
    fn fetch_range(&mut self, _ctx: &FetchCtx) -> Option<FetchRange> {
        None
    }

    /// Rebuild data using kline(OHLCV) source
    fn rebuild_from_source(&mut self, _source: &PlotData<KlineDataPoint>) {}

    fn on_insert_klines(&mut self, _klines: &[Kline], _source: &PlotData<KlineDataPoint>) {}

    fn on_insert_trades(
        &mut self,
        _trades: &[Trade],
        _old_dp_len: usize,
        _source: &PlotData<KlineDataPoint>,
    ) {
    }

    fn on_ticksize_change(&mut self, _source: &PlotData<KlineDataPoint>) {}

    /// Apply persisted indicator-specific visual settings.
    fn on_config_changed(&mut self, _config: &data::chart::kline::Config) {}

    /// Timeframe/tick interval has changed
    fn on_basis_change(&mut self, _source: &PlotData<KlineDataPoint>) {}

    fn on_open_interest(&mut self, _pairs: &[exchange::OpenInterest]) {}

    /// Reference klines requested through `FetchRange::RefKlines`; every indicator sees
    /// every batch and keeps only its own (by market and timeframe).
    fn on_ref_klines(&mut self, _spot: bool, _timeframe: Timeframe, _klines: &[Kline]) {}

    /// Draws on the price chart, for overlays that own fetched data.
    fn draw_overlay(&self, _frame: &mut iced::widget::canvas::Frame, _ctx: &OverlayCtx) {}

    /// Prices of the levels the overlay currently shows, for "near a level" filters.
    fn reference_prices(&self) -> Vec<f64> {
        Vec::new()
    }

    /// Open interest samples by time, for indicators that hold them.
    fn open_interest(&self) -> Option<&std::collections::BTreeMap<UnixMs, f64>> {
        None
    }
}

/// Chart-space mapping for `KlineIndicatorImpl::draw_overlay`.
pub struct OverlayCtx<'a> {
    pub x: &'a dyn Fn(u64) -> f32,
    pub y: &'a dyn Fn(exchange::unit::Price) -> f32,
    pub left_x: f32,
    pub right_x: f32,
    pub scaling: f32,
    pub text_color: iced::Color,
}

pub struct FetchCtx<'a> {
    pub main_chart: &'a ViewState,
    pub timeframe: Timeframe,
    pub visible_earliest: UnixMs,
    pub kline_latest: UnixMs,
    pub prefetch_earliest: UnixMs,
}

pub fn make_empty(which: KlineIndicator) -> Box<dyn KlineIndicatorImpl> {
    use data::chart::indicator::catalog::Runtime;
    match which.definition().runtime {
        Runtime::Volume => Box::new(volume::VolumeIndicator::new()),
        Runtime::BarAnalysis => Box::new(bar_analysis::BarAnalysisIndicator::new()),
        Runtime::OpenInterest => Box::new(open_interest::OpenInterestIndicator::new()),
        Runtime::SpotCvd => Box::new(spot_cvd::SpotCvdIndicator::new()),
        Runtime::PeriodLevels => Box::new(period_levels::PeriodLevelsIndicator::new()),
        Runtime::Vwap => Box::new(htf_vwap::HtfVwapIndicator::new()),
        Runtime::Scripted(kind) => Box::new(scripted::ScriptedIndicator::new(kind)),
        Runtime::Overlay => Box::new(OverlayIndicator),
    }
}

/// Runtime marker for indicators rendered on the main price canvas. Overlay
/// drawing is orchestrated by `KlineChart`, but lifecycle, persistence and UI
/// selection use the same indicator registry as panel indicators.
struct OverlayIndicator;

impl KlineIndicatorImpl for OverlayIndicator {
    fn clear_all_caches(&mut self) {}
    fn clear_crosshair_caches(&mut self) {}

    fn element<'a>(
        &'a self,
        _chart: &'a ViewState,
        _data_labels_always_visible: bool,
        _visible_range: std::ops::RangeInclusive<u64>,
    ) -> iced::Element<'a, Message> {
        unreachable!("overlay indicators render on the main chart canvas")
    }
}
