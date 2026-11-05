pub mod catalog;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct VisibilityRange {
    pub enabled: bool,
    pub from: u16,
    pub to: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct ObjectVisibility {
    pub shown: bool,
    pub ranges: [VisibilityRange; 6],
}

impl Default for ObjectVisibility {
    fn default() -> Self {
        Self {
            shown: true,
            ranges: Self::CATEGORIES.map(|(_, max)| VisibilityRange {
                enabled: true,
                from: 1,
                to: max,
            }),
        }
    }
}

impl ObjectVisibility {
    pub const CATEGORIES: [(&'static str, u16); 6] = [
        ("Секунды", 59),
        ("Минуты", 59),
        ("Часы", 23),
        ("Дни", 365),
        ("Недели", 52),
        ("Месяцы", 12),
    ];

    pub fn allows_timeframe(self, basis: crate::chart::Basis) -> bool {
        let crate::chart::Basis::Time(timeframe) = basis else {
            return true;
        };
        let ms = timeframe.to_milliseconds();
        let (index, amount) = if ms < 60_000 {
            (0, (ms / 1_000).max(1))
        } else if ms < 3_600_000 {
            (1, ms / 60_000)
        } else if ms < 86_400_000 {
            (2, ms / 3_600_000)
        } else {
            (3, ms / 86_400_000)
        };
        let range = self.ranges[index];
        range.enabled && (u64::from(range.from)..=u64::from(range.to)).contains(&amount)
    }

    pub fn is_visible(self, basis: crate::chart::Basis) -> bool {
        self.shown && self.allows_timeframe(basis)
    }
}

/// New indicator registrations must not invalidate visibility maps in older profiles.
pub(crate) fn deserialize_visibility<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<enum_map::EnumMap<KlineIndicator, ObjectVisibility>, D::Error> {
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = enum_map::EnumMap<KlineIndicator, ObjectVisibility>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("indicator visibility map")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut result = Self::Value::default();
            while let Some((key, value)) = map.next_entry::<KlineIndicator, ObjectVisibility>()? {
                result[key] = value;
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(Visitor)
}

use std::fmt::{self, Debug, Display};

use enum_map::Enum;
use exchange::adapter::{Exchange, MarketKind};
use serde::{Deserialize, Serialize};

pub trait Indicator: PartialEq + Display + 'static {
    fn for_market(market: MarketKind) -> &'static [Self]
    where
        Self: Sized;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndicatorPlacement {
    Panel,
    Overlay,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize, Eq, Enum)]
pub enum KlineIndicator {
    Volume,
    BarAnalysis,
    CumulativeDelta,
    OpenInterest,
    VolumeBubbles,
    SessionVolumeProfile,
    Vwap,
    GexLevels,
    Rsi,
    Atr,
    Macd,
    Adx,
    Tpo,
    NakedLevels,
    SpotCvd,
    PeriodLevels,
    OiBars,
    SizeCvd,
    ClusterSearch1,
    ClusterSearch2,
    ClusterSearch3,
}

impl Indicator for KlineIndicator {
    fn for_market(market: MarketKind) -> &'static [Self] {
        catalog::for_market(market)
    }
}

impl KlineIndicator {
    pub fn definition(self) -> &'static catalog::Definition {
        catalog::definition(self)
    }
    pub fn placement(self) -> IndicatorPlacement {
        self.definition().placement
    }
    pub fn is_overlay(self) -> bool {
        self.placement() == IndicatorPlacement::Overlay
    }

    pub fn requires_trades(self, exchange: Exchange) -> bool {
        matches!(
            self,
            Self::VolumeBubbles
                | Self::SessionVolumeProfile
                | Self::Vwap
                | Self::ClusterSearch1
                | Self::ClusterSearch2
                | Self::ClusterSearch3
        ) || (self == Self::CumulativeDelta
            && !matches!(
                exchange,
                Exchange::BinanceLinear | Exchange::BinanceInverse | Exchange::BinanceSpot
            ))
    }
}

impl Display for KlineIndicator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.definition().name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize, Eq, Enum)]
pub enum HeatmapIndicator {
    Volume,
}

impl Indicator for HeatmapIndicator {
    fn for_market(market: MarketKind) -> &'static [Self] {
        match market {
            MarketKind::Spot => &Self::FOR_SPOT,
            MarketKind::LinearPerps | MarketKind::InversePerps => &Self::FOR_PERPS,
        }
    }
}

impl HeatmapIndicator {
    // Indicator togglers on UI menus depend on these arrays.
    // Every variant needs to be in either SPOT, PERPS or both.
    /// Indicators that can be used with spot market tickers
    const FOR_SPOT: [HeatmapIndicator; 1] = [HeatmapIndicator::Volume];
    /// Indicators that can be used with perpetual swap market tickers
    const FOR_PERPS: [HeatmapIndicator; 1] = [HeatmapIndicator::Volume];
}

impl Display for HeatmapIndicator {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            HeatmapIndicator::Volume => write!(f, "Volume"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Temporary workaround,
/// represents any indicator type in the UI
pub enum UiIndicator {
    Heatmap(HeatmapIndicator),
    Kline(KlineIndicator),
}

impl From<KlineIndicator> for UiIndicator {
    fn from(k: KlineIndicator) -> Self {
        UiIndicator::Kline(k)
    }
}

impl From<HeatmapIndicator> for UiIndicator {
    fn from(h: HeatmapIndicator) -> Self {
        UiIndicator::Heatmap(h)
    }
}

#[cfg(test)]
mod visibility_tests {
    use super::*;
    use crate::chart::Basis;
    use exchange::Timeframe;

    #[test]
    fn object_visibility_round_trips_and_respects_timeframe_boundaries() {
        let mut visibility = ObjectVisibility::default();
        visibility.ranges[1] = VisibilityRange {
            enabled: true,
            from: 3,
            to: 5,
        };
        visibility.ranges[2].enabled = false;

        let saved = serde_json::to_string(&visibility).unwrap();
        let restored: ObjectVisibility = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored, visibility);
        for tf in [
            Timeframe::MS500,
            Timeframe::M3,
            Timeframe::M5,
            Timeframe::D1,
        ] {
            assert!(restored.is_visible(Basis::Time(tf)), "{tf:?}");
        }
        for tf in [Timeframe::M1, Timeframe::M15, Timeframe::H1] {
            assert!(!restored.is_visible(Basis::Time(tf)), "{tf:?}");
        }
    }
}
