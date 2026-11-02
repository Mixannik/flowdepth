# Жизненный цикл профилей объёма — аудит 02.11.2026

Только чтение production-кода; cargo не запускался. Линии относятся к HEAD 84f310a до параллельной реализации большого пакета.

## Где расчёт попадает в draw/read

- `src/chart/kline.rs:5069 build_session_profiles` создаёт bins/POC/VA/VWAP. `draw_session_volume_profiles:5401` вызывает его внутри рисования (`:5413`); сам draw вызывается из canvas main (`:3320`). Результат зависит от `earliest/latest`, потому что выборка строится от начала первой видимой сессии до последнего видимого бара.
- `fixed_volume_profile:2540` вызывает `build_candle_volume_profile` либо trade builders. Он используется в canvas draw (`:3333`) и в публичном `range_levels:2415`. Последний вызывается из reference (`:2436`), auction (`:2492`) и экспортируется всем вкладкам. То есть одинаковая FRVP-математика выполняется повторно и вне draw при чтении.
- `src/chart/kline/naked_levels.rs:277 shown_levels` каждый раз `collect_bars` + `naked_levels` для дневных/недельных сессий. `draw:302` вызывает его (`:317`); Kline `reference_level_prices` отдельно вызывает тот же `shown_levels` (`kline.rs:2463`). Отдельного calc-cache нет.
- `chart.cache.main` (`kline.rs:2996`) — кэш готовой геометрии canvas. Он чистится и при pan/zoom/layout; это не кэш результатов модели. `gex_render_cache` — отдельный механизм GEX, к данным профилей отношения не имеет. `range_hourly` — исходные H1, не вычисленные профили.

## Доказуемые проблемы missing_hourly

`src/chart/kline/range_levels.rs:49` проверяет только первый/последний timestamp. При полном `[10H,20H)` и отсутствии 15H вернёт `None`: внутренняя дыра не обнаруживается.

При `from=10H+30m` выборка `hourly.range(from..end)` исключает перекрывающую H1 10H. Первый бар 11H ещё меньше `from+H`, поэтому начало считается приемлемым. Сам `build_candle_volume_profile` (`kline.rs:5299`) также начинает с `range(from..to)` и теряет перекрывающий час. Для выбранной политики H1-аппроксимации нормализовать request/coverage/build последовательно, не только helper.

`drawing.rs:266` считает профиль готовым, если `missing_hourly` вернул хвост с `missing_from > from`; это специально допускает частичную картинку, но не является доказательством полной исходной истории для reference/shared/auction.

Существующие тесты (`range_levels.rs:146,167`) покрывают пустой диапазон, позднее начало, отсутствующий хвост и polling forming bar. Внутренней дыры, невыравненных границ, неправильного timeframe/невалидных свечей и stale после закрытия часа нет.

## Минимальный безопасный cache FRVP+naked

Владельцем готовых результатов должен стать KlineChart/отдельная модель; draw/reference/shared export читают один снимок. FRVP ключ — bounds, математическая config, tick size, revisions данных и coverage. Naked ключ — revision данных/coverage, config и эффективная сессия. Не включать viewport: naked и FRVP от pan/zoom не меняются. SVP пока сохранить отдельно либо построить все нужные сессии и фильтровать видимость после расчёта.

Закрытый набор хуков dirty:

1. Конструктор/init, `update_latest_kline:583` (сам не вызывает invalidate), `insert_trades:1976`, непустой `insert_raw_trades:2072`, `insert_hist_klines:2234`.
2. `on_ref_klines:2385` при изменении H1 `range_hourly`, независимо от наличия видимого FRVP.
3. `mark_trade_range_covered:1292`, `reset_request_handler:1191`, `invalidate_market_data_cache:1215`, `mark_visible_range_trades_complete:1604`. Покрытие меняется и без нового объёма. `complete_trade_fetch/complete_backfill` сначала меняют диапазоны, потом `mark_trade_buckets_complete:1589`: refresh нельзя делать посередине этой последовательности.
4. `set_visual_config:1791`, `change_tick_size:1891`, `set_basis:1914`, `toggle_indicator:2769` (особенно NakedLevels).
5. `drawing.rs:293 commit`, принятый общий хвост `handle_drawing:325` (включая move/resize/style/delete/visibility), `set_drawings:1172`, изменение geometry в `normalize_fixed_range_drawings:183`.
6. `drawing_canvas_update:858` — `&self`, только публикует сообщения; отдельной мутации geometry там нет. PointerMoved/MoveFinished проходят через `handle_drawing`.

Обновление dirty-снимка в `invalidate():2599` перед `fetch_missing_data` подходит и не выполняет расчёт в draw. Нельзя безусловно выставлять dirty на каждом invalidate: это вернёт расчёт при навигации. Обработчики drawing сейчас чистят только canvas и сдвигают last_tick; после mutation нужно либо сразу refresh, либо гарантированно refresh до следующего draw/экспорта уровней. `set_shared_range_levels` меняет уже готовые чужие линии и не должен dirty собственного calc-cache.

Текущий naked algorithm выбирает current_session по последнему бару, а не wall clock. Для сохранения поведения cache-key достаточно эффективной сессии последнего бара. Если переводить на явный now, отдельно определить, как работать при отсутствии нового бара после UTC-границы: часы сами по себе не доказывают полноту сессии/отсутствие касаний.

Проверки перед сдачей: одинаковый snapshot для draw/reference/export; обновления после каждого из dirty-хуков; pan/zoom не увеличивает число расчётов; internal H1 hole ремонтируется; после восстановления coverage уровень появляется без повторной настройки; смена tick/config/drawing bounds не оставляет старый POC.
