# Flowdepth fork — заметки для сессий Claude

Личный форк Flowdepth (Rust, iced) под ордерфлоу-трейдинг пользователя. Цель — повторить функции MMT
(по шаблонам трейдера JDK, @The_JDK99) и Coinglass Footprint. Отвечать пользователю по-русски, кратко.

## Где что
- Репозиторий: `C:\Users\MSI\Flowdepth\flowdepth`, ветка `feat/delta-table` (база — `develop` от Niketion/flowdepth).
- План и статусы функций: `docs/orderflow-roadmap.md` (правила сигналов Coinglass, шаблоны JDK).
- Рабочая сборка пользователя: `C:\Users\MSI\Flowdepth\app-dev\flowsurface.exe` (рядом `assets`). Старая бета: `..\app`.
- Настройки/раскладки приложения: `%APPDATA%\flowsurface\saved-state.json`, лог: `%APPDATA%\flowsurface\logs\flowsurface-current.log`.
- Скрипты: `C:\Users\MSI\Flowdepth\tools\` (бэкапы `saved-state.before-*.json` тоже там).
- Материалы JDK (посты и скрины из X): `C:\Users\MSI\Flowdepth\jdk\`.
- Чужие терминалы и идеи для кругозора (Hyblock, 0xArchive, Hyperliquid) — `C:\Users\MSI\Flowdepth\ideas\README.md`, не план.
- Материалы @Stoiiic (AMT, пассивный спот, OB Depth/Pressure Bands, VWAP-полосы): `C:\Users\MSI\Flowdepth\stoiiic\README.md` (конспект) + картинки.

## Принцип работы — сначала очевидное (бритва Оккама)
Для любой задачи — баг, «не то показывает», новая функция, правка настроек:
1. **Уточнить, что именно**: какой объект, где, в каких единицах и часовом поясе; подтвердить у пользователя или скрином, а не по своей догадке.
2. **Проверить самые простые объяснения первыми**, от дешёвых к дорогим: внешний источник (API, файл, настройка) → вход программы (лог, кэш) → и только потом код.
3. **Одна гипотеза — одна дешёвая проверка**. Перезапуск приложения, диагностика в коде, долгие прогоны — только когда дешёвые проверки исчерпаны.
4. **Если факты не сходятся с ожиданием — остановиться и перепроверить исходное допущение**, а не копать глубже.
5. Перед новой функцией — сначала посмотреть, что уже есть в коде и в данных, и брать самый простой вариант, который решает задачу.
Пример, зачем это: 27.10 полтора часа искали в коде «потерю сделок», а это была дыра у биржи и не та свеча (МСК вместо UTC) — шаги 1–2 нашли бы за минуты.

## Сборка и тесты
- `$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"` (rustup ставился через winget, PATH сессии его не видит).
- `cargo fmt; cargo build --release` (~1–2 мин инкрементально), тесты: `cargo test --release --workspace`.
- Вывод сборки писать в файл (`*> C:\Users\MSI\Flowdepth\build.log`) и грепать `^(error|warning)`.

## Запуск новой сборки — важно
- Программа сохраняет состояние только при штатном закрытии; `CloseMainWindow` её не закрывает, `Stop-Process` теряет изменения пользователя.
- Поэтому: правка state / подмена exe — через ожидающие скрипты, которые ждут, пока пользователь сам закроет Flowdepth:
  `tools\add-indicator.mjs <layout> <Indicator> [H4]`, `tools\add-signals.mjs`, `tools\drop-npoc.mjs` (копируют exe из `target\release`, правят state, перезапускают).
  Запускать в фоне и не держать два ожидающих скрипта одновременно (иначе двойной запуск).
- Не снимать экран пока пользователь за компьютером (`tools\shot.ps1` захватывал его окна). Просить скрин; вложения иногда не доходят — просить вставить Ctrl+V.

## Сделано в форке (коммиты на feat/delta-table)
- Кластеры **Delta Table** по правилам Coinglass (цвет по доминирующей стороне, серый <10%, POC в золотой рамке, дисбаланс — жирное цветное число); POC-рамка и в **Table**.
- **Auto row size** (шаг цены сам, строки 13–30 px) — `pick_row_size_multiplier`; без сброса request handler (иначе выбрасывался бэкфилл).
- Стартовая ширина футпринт-свечи 140 px.
- Исследование **Signals** (`src/chart/kline/signals.rs`): Absorption, Trapped longs/shorts, Exhaustion; пороги — константы в начале файла.
- Индикатор **Naked session levels** (`src/chart/kline/naked_levels.rs`): nPOC/nVAH/nVAL недельных (2) и nPOC дневных (3) сессий, по сделкам или по объёму свечей.
- Signals: **Stacked imbalances** и **Unfinished auctions** (`src/chart/kline/zones.rs`); футпринт сжимается до ~2 px, текст прячется (`MIN_TEXT_BAR_PX`).
- **Лёгкая история**: свечи старше 2 ч (`COMPACT_AFTER_MS`) сворачивают сырые сделки в суммы по ~60–600 ценам (`KlineDataPoint::compact_trades`), экстремумы точные.
- Кэш сделок (`market.redb`): старше 2 дней — поминутные суммы по ценам (`aggregate_trades_for_cache`, id с битом `AGGREGATE_ID_BIT`, пузыри их игнорируют); старый сырой кэш удаляется один раз при открытии.
- Binance REST `aggTrades` отдаёт только 2 дня (код -4166): старее — следующий дневной архив (`fetch_trades`).
- **Лента ликвидаций**: Time & Sales → Feed: Liquidations (Binance `forceOrder` только на `/market`, отдельный «тихий» сокет `WsSession::silent`); склейка частей рыночной заявки в ленте.
- **Вкладки**: все раскладки живые — подписки на объединение потоков (`Dashboard::market_subscriptions(&UniqueStreams)`), события через `route_to_layouts`, тик/бэкфилл для всех раскладок (main.rs).
- **CVD Futures + Spot** (`src/chart/indicator/kline/spot_cvd.rs`, `KlineIndicator::SpotCvd`): перп-CVD и спот-CVD Binance в одной панели, каждая линия нормирована по видимому участку (как Coinglass). Спот — по спотовым свечам (taker buy), `FetchRange::SpotKlines`, текущий бар опрашивается раз в 10 с. Заменяет обычный CVD (в JDK FP 15m старый CVD убран). Галочка **Delta bars** (`IndicatorConfigs::spot_cvd`, по умолчанию вкл.) — дельта каждого бара столбиками за линиями: фьюч слева синим, спот справа оранжевым, у каждого своя шкала (как Delta Volume в MMT).
- **Уровни прошлых периодов** PDH/PDL, PWH/PWL, PMH/PML (`src/chart/indicator/kline/period_levels.rs`, `KlineIndicator::PeriodLevels`, оверлей с собственными данными — `Runtime::PeriodLevels`): по дневным свечам, запрос `FetchRange::RefKlines { spot: false, timeframe: D1 }` (тот же путь, что спот-свечи для CVD, `spot: true`).
- **VWAP** — один индикатор (`KlineIndicator::Vwap`, `Runtime::Vwap`), в настройках галочки: Session (по сделкам, якорь из списка, рисует kline.rs), Weekly и Monthly (по часовым свечам `RefKlines { H1 }`, опрос 30 с, `src/chart/indicator/kline/htf_vwap.rs`). Отдельного `HtfVwap` больше нет. У уровней прошлых периодов и Naked levels — свои окна настроек (`IndicatorConfigs`, `src/indicators/editors/levels.rs`). Signals и так переключаются по отдельности в исследовании Signals. Оверлеи со своими данными рисуются через `KlineIndicatorImpl::draw_overlay(OverlayCtx)` (`draw_data_overlays` в kline.rs); `on_ref_klines(spot, timeframe, ..)` получают все индикаторы и фильтруют сами.
- **Fixed Range Volume Profile** под схему JDK: диапазон до 92 дней; длиннее 3 дней (`range_levels::CANDLE_PROFILE_AFTER_MS`) — по часовым свечам `RefKlines { H1 }` (хранятся в `KlineChart::range_hourly`, `build_candle_volume_profile`), короче — по сделкам. Опция `extend_levels` (вкл.) тянет POC/VAH/VAL вправо (`src/chart/kline/range_levels.rs`); раз в секунду main.rs собирает `Dashboard::range_levels` со всех вкладок и раздаёт `set_shared_range_levels` графикам того же тикера (ничего нового в state не хранится — источник остаётся рисунком 4h-графика). HTF VWAP отбрасывает часовые свечи старше прошлого месяца.
- Signals → **Only near levels** (`near_levels`, serde default false): absorption/trapped/exhaustion только в 0.3% от уровня (`KlineChart::reference_level_prices`: FRVP, period levels, naked, W/M VWAP через `KlineIndicatorImpl::reference_prices`).
- **Спот рядом с перпом**: вкладка «Spot / Perp 15m» (`tools\spot-perp-layout.mjs`). CVD Futures + Spot работает и на споте (роли меняются, `RefKlines { spot: true }` = другой рынок пары, `market_counterpart`); уровни FRVP раздаются по символу, без различия спот/перп.
- Спотовые архивы Binance отличаются от фьючерсных: время в микросекундах, сторона `True`/`False` (`archive_time_ms`, `archive_bool` в binance/fetch.rs). Без этого спот качался по 1000 сделок через REST.
- Дисбалансы (отметки и стеки): доминирующая сторона ≥ среднего объёма одной стороны строки свечи (`zones::average_row_qty`) — убирает шум тонких спотовых строк.
- **Синхронная ось времени**: скролл/зум графика двигает другие графики того же символа и таймфрейма в раскладке (спот и перп), `Dashboard::sync_time_view` → `ViewState::follow_time_view`; ось цены у каждого своя.
- **OI + delta bars** — отдельный индикатор `KlineIndicator::OiBars` (перп, нужен Open Interest); из Signals убран.
- Signals → **Failed auctions** (`src/chart/kline/failed_auction.rs`): выход за уровень и возврат закрытием, уровни из `KlineChart::auction_levels` (period levels по дням через `KlineIndicatorImpl::auction_levels`, FRVP VAH/VAL, линии); пороги — константы в начале файла.
- **CVD by order size** (`src/chart/indicator/kline/size_cvd.rs`, `KlineIndicator::SizeCvd`): линия CVD на группу размера заявки (ретейл/средние/крупные/киты), части одной заявки = сделки одной стороны в одну мс. В данных — мелкие корзины 1-3-10 (`SIZE_BUCKET_USD`, 10 шт.) в `IntrabarStats::size_delta`; группы собираются при отрисовке по рынку графика (`size_group_edges`: перп 10K/100K/1M, спот в 10 раз ниже — проверено по распределению объёма 30.10). В кэше старше 2 дней агрегаты делятся и по корзине (`AGGREGATE_SPLIT_FLAG` бит 59, корзина в битах 52–55), свеча копит `cached_size_delta`; `KlineDataPoint::size_delta()` берёт что есть. Агрегаты до 30.10 разбивки не имеют. Панель живёт только во вкладке «Who trades 15m» (перп сверху, спот снизу; `tools\who-trades-layout.mjs`).
- Ловушка iced: `Frame::with_clip` теряет трансформ графика — не использовать в chart-space.
- Шкала времени у пользователя — в UTC (лог пишется по МСК, UTC+3): сверять свечи по числу сделок в Bar Stats.
- У Binance бывают дыры в `aggTrades` (27.10 00:03–00:30 UTC: в REST нет сделок, а в klines они есть). Кэш при чтении считает долгую тишину дырой (`trade_integrity_gaps`) и докачивает; дыру на стороне биржи закроет только дневной архив, когда выйдет.

## Раскладки у пользователя
JDK 1m, JDK FP 5m (Delta Table), JDK FP 15m (Table), JDK HTF 4h (+NakedLevels на 4h), Orderflow, Test.
Единицы — BTC (`size_in_quote_ccy: Base`); режим USD в Flowdepth смешивает историю и live (баг, в плане).

## Дальше по плану
Проверить новую отрисовку уровней на 4h (и выключить там Session Volume Profile — дублирует), затем:
стековые дисбалансы, незавершённые аукционы, ленты ликвидаций/крупных сделок, второй VWAP, OB Depth, фикс USD, агрегация бирж.
