//! Prepared profile results. Drawing and signal readers never rebuild distributions.
use super::*;

#[derive(Default)]
pub(super) struct ProfileCalculations {
    pub dirty: bool,
    inputs: Vec<(UnixMs, UnixMs, FixedRangeVolumeProfileConfig)>,
    tick: i64,
    hour: u64,
    naked_config: Option<(bool, data::chart::kline::NakedLevelsConfig)>,
    session_key: Option<(u64, u64, SessionVolumeProfileConfig)>,
    fixed: Vec<FixedSnapshot>,
    pub session: Vec<SessionProfile>,
    pub naked: Vec<naked_levels::Level>,
    fixed_warnings: Vec<String>,
    naked_warning: Option<String>,
    session_warning: Option<String>,
    pub fixed_builds: u64,
    pub naked_builds: u64,
    pub session_builds: u64,
    pub builds: u64,
}

struct FixedSnapshot {
    from: UnixMs,
    to: UnixMs,
    config: FixedRangeVolumeProfileConfig,
    profile: Option<Arc<SessionProfile>>,
    ready: bool,
    approximate: bool,
}

impl KlineChart {
    pub(super) fn prepare_profile_calculations(&mut self) {
        let inputs = self.fixed_volume_profiles();
        let naked_config = (
            self.indicator_enabled(KlineIndicator::NakedLevels),
            self.visual_config.indicator_configs.naked_levels,
        );
        let session_key = self
            .indicator_enabled(KlineIndicator::SessionVolumeProfile)
            .then(|| {
                self.visible_timerange().map(|(a, b)| {
                    let cfg = self.visual_config.session_volume_profile;
                    let interval = match &self.data_source {
                        PlotData::TimeBased(s) => s.interval.to_milliseconds(),
                        _ => 1,
                    };
                    (
                        align_session_start(a, cfg.interval.milliseconds()),
                        b / interval * interval,
                        cfg,
                    )
                })
            })
            .flatten();
        let hour = UnixMs::now().as_u64() / range_levels::HOUR_MS;
        let old = &self.profile_calculations;
        let fixed_changed = old.dirty
            || old.inputs != inputs
            || old.tick != self.chart.tick_size.units
            || old.hour != hour;
        let naked_changed = old.dirty || old.naked_config != Some(naked_config);
        let session_changed =
            old.dirty || old.tick != self.chart.tick_size.units || old.session_key != session_key;
        if !fixed_changed && !naked_changed && !session_changed {
            return;
        }
        if fixed_changed {
            // Retain recent H1 for reuse, plus active long drawings only.
            let since = data::analysis::htf_vwap::retained_since(UnixMs::now().as_u64());
            self.range_hourly.retain(|&time, _| {
                time >= since
                    || inputs.iter().any(|&(from, to, _)| {
                        range_levels::uses_candles(from.as_u64(), to.as_u64())
                            && time >= from.as_u64() / range_levels::HOUR_MS * range_levels::HOUR_MS
                            && time < to.as_u64()
                    })
            });
            self.range_hourly_confirmed
                .retain(|time| self.range_hourly.contains_key(time));
            let mut warnings = Vec::new();
            let fixed=inputs.iter().map(|&(from,to,config)| {
                let approximate=range_levels::uses_candles(from.as_u64(),to.as_u64());
                let ready=self.profile_publication_ready(from,to);
                let profile=self.compute_fixed_volume_profile(from,to,&config).map(Arc::new);
                if !ready {
                    let date=|time:UnixMs| chrono::DateTime::from_timestamp_millis(time.as_u64() as i64)
                        .map(|t| t.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default();
                    warnings.push(format!("Fixed profile {} to {} UTC: incomplete or developing {}; extended levels and signals are withheld.",
                        date(from),date(to),if approximate {"hourly history"} else {"trade history"}));
                }
                FixedSnapshot {from,to,config,profile,ready,approximate}
            }).collect();
            self.profile_calculations.fixed = fixed;
            self.profile_calculations.fixed_warnings = warnings;
            self.profile_calculations.fixed_builds += 1;
        }
        if naked_changed {
            let (naked, warning) = if naked_config.0 {
                let result =
                    naked_levels::snapshot(&self.data_source, align_session_start, &naked_config.1);
                let warning=(result.incomplete_sessions>0).then(||format!("Naked levels: {} recent sessions have incomplete candles or later price history; untested levels are withheld.",result.incomplete_sessions));
                (result.levels, warning)
            } else {
                (Vec::new(), None)
            };
            self.profile_calculations.naked = naked;
            self.profile_calculations.naked_warning = warning;
            self.profile_calculations.naked_builds += 1;
        }
        if session_changed {
            let mut warning = None;
            let session=session_key.map(|(from,to,config)| {
                let start=UnixMs::new(from);
                if let PlotData::TimeBased(series)=&self.data_source && !series.datapoints.is_empty() {
                    let (_,last)=series.timerange();
                    let end=UnixMs::new(to).min(historical_trade_target_to(last,series.interval.to_milliseconds(),UnixMs::now()));
                    if end>start && !self.is_trade_range_covered(start,end) {
                        warning=Some("Session profile is developing from incomplete trade history; its distribution and local POC/VA may change during loading.".to_owned());
                    }
                }
                build_session_profiles(&self.data_source,from,to,self.chart.tick_size,&config)
            }).unwrap_or_default();
            self.profile_calculations.session = session;
            self.profile_calculations.session_warning = warning;
            self.profile_calculations.session_builds += 1;
        }
        let cache = &mut self.profile_calculations;
        cache.dirty = false;
        cache.inputs = inputs;
        cache.tick = self.chart.tick_size.units;
        cache.hour = hour;
        cache.naked_config = Some(naked_config);
        cache.session_key = session_key;
        cache.builds += 1;
    }

    /// Missing keys, invalid candles, or a previously forming snapshot not yet
    /// refreshed after close all require another source request.
    pub(super) fn missing_profile_hourly(
        &self,
        from: u64,
        to: u64,
        now: u64,
    ) -> Option<(u64, u64)> {
        if let Some(missing) = range_levels::missing_hourly(&self.range_hourly, from, to, now) {
            return Some(missing);
        }
        let end = to.min(now);
        if from >= end {
            return None;
        }
        let first = from / range_levels::HOUR_MS * range_levels::HOUR_MS;
        self.range_hourly
            .range(first..end)
            .find(|(time, bar)| {
                !self.range_hourly_confirmed.contains(*time) || !valid_profile_candle(bar)
            })
            .map(|(&time, _)| (time, end))
    }

    fn profile_publication_ready(&self, from: UnixMs, to: UnixMs) -> bool {
        let now = UnixMs::now();
        if from >= to || to > now {
            return false;
        }
        if range_levels::uses_candles(from.as_u64(), to.as_u64()) {
            return self
                .missing_profile_hourly(from.as_u64(), to.as_u64(), now.as_u64())
                .is_none();
        }
        if !self.is_trade_range_covered(from, to) {
            return false;
        }
        let PlotData::TimeBased(series) = &self.data_source else {
            return false;
        };
        let interval = series.interval.to_milliseconds();
        let mut expected = from.floor_to(series.interval);
        for (&time, bar) in series.datapoints.range(expected..to) {
            if time != expected
                || bar.trade_coverage != data::chart::kline::TradeCoverage::Complete
                || !valid_profile_candle(&bar.kline)
            {
                return false;
            }
            expected = expected.saturating_add(interval);
        }
        expected >= to
    }

    pub(super) fn fixed_volume_profile(
        &self,
        from: UnixMs,
        to: UnixMs,
        config: &FixedRangeVolumeProfileConfig,
    ) -> Option<Arc<SessionProfile>> {
        self.profile_calculations
            .fixed
            .iter()
            .find(|s| s.from == from && s.to == to && s.config == *config)?
            .profile
            .clone()
    }

    pub(super) fn owned_range_levels(
        &self,
    ) -> impl DoubleEndedIterator<Item = (u64, range_levels::RangeLevels)> + '_ {
        self.visible_fixed_profile_owners()
            .filter_map(|(id, from, to, config)| {
                if self.ws_integrity_revision.is_some() || !config.extend_levels {
                    return None;
                }
                let snapshot = self
                    .profile_calculations
                    .fixed
                    .iter()
                    .find(|s| s.from == from && s.to == to && s.config == config && s.ready)?;
                let profile = snapshot.profile.as_ref()?;
                Some((
                    id,
                    range_levels::RangeLevels {
                        to: to.as_u64(),
                        poc: profile.poc.to_f64(),
                        vah: profile.vah.to_f64(),
                        val: profile.val.to_f64(),
                        approximate: snapshot.approximate,
                    },
                ))
            })
    }

    pub(super) fn profile_warnings(&self) -> impl Iterator<Item = String> + '_ {
        self.profile_calculations
            .fixed_warnings
            .iter()
            .chain(self.profile_calculations.naked_warning.iter())
            .chain(self.profile_calculations.session_warning.iter())
            .cloned()
    }

    pub fn profile_source_info(&self) -> Option<String> {
        let candles = self
            .profile_calculations
            .fixed
            .iter()
            .any(|s| s.approximate);
        let naked = self
            .profile_calculations
            .naked
            .iter()
            .any(|l| l.approximate);
        if !candles && !naked {
            return None;
        }
        Some(format!(
            "{}{}Candle volume is distributed over price rows; these profiles are approximations, not exact trade distributions.",
            if candles {
                "Fixed profile levels marked ~ use hourly candles.\n"
            } else {
                ""
            },
            if naked {
                "Some naked levels use candle-volume fallback.\n"
            } else {
                ""
            }
        ))
    }
}

pub(super) fn valid_profile_candle(bar: &Kline) -> bool {
    bar.low.units > 0
        && bar.high >= bar.low
        && bar.open >= bar.low
        && bar.open <= bar.high
        && bar.close >= bar.low
        && bar.close <= bar.high
        && match bar.volume {
            exchange::Volume::TotalOnly(q) => q.units >= 0,
            exchange::Volume::BuySell(b, s) => b.units >= 0 && s.units >= 0,
        }
}
