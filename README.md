# Flowdepth — личный order-flow форк

Форк [Niketion/flowdepth](https://github.com/Niketion/flowdepth) под рабочие раскладки и анализ ордерфлоу. Приложение написано на Rust и iced; исполняемый файл пока называется `flowsurface`.

Репозиторий опубликован для хранения истории и внешнего ревью. Основная рабочая ветка — **feat/delta-table**. Архитектура и функции продолжают дорабатываться.

## Рабочее направление

- Footprint: Table и Delta Table, статистика бара, автоматический шаг строк.
- Сравнение Binance spot/perpetual, CVD Futures + Spot, CVD по размеру заявок и Open Interest.
- Session/Weekly/Monthly VWAP, уровни прошлых периодов, Fixed Range Volume Profile и naked-уровни.
- Исследование order-flow сигналов: absorption, trapped traders, exhaustion, imbalances и failed auctions. Эти правила требуют дальнейшей проверки на данных.
- Рабочие раскладки с графиками, стаканом и heatmap.

В текущей серии изменений укрепляется ядро: сохранность настроек и кэша, отдельный тестовый профиль, полнота истории, единицы количества, общие загрузки и расчёты вне отрисовки. Неполные уровни исключаются из соответствующих сигнальных фильтров, свечные приближения обозначаются отдельно.

Наличие унаследованного модуля в исходниках не означает, что он проверен или используется в этом форке. Скриншот проверенной сборки пока не добавлен.

## Для ревью

Начните с [REVIEW.md](REVIEW.md). Там перечислены приоритеты проверки и основные документы.

- [Карта проекта](docs/project-guide.md)
- [Архитектура](docs/core-architecture.md)
- [Текущее состояние и ограничения](docs/state.md)
- [План order-flow функций](docs/orderflow-roadmap.md)

## Сборка

Нужны Git, Rust из [rust-toolchain.toml](rust-toolchain.toml) и платформенные зависимости iced. На Windows используется Rust MSVC toolchain.

```sh
git clone --branch feat/delta-table https://github.com/Mixannik/flowdepth.git
cd flowdepth
cargo build --release --locked
cargo run --release --locked
```

Проверки:

```sh
cargo test --release --workspace --locked
cargo check --workspace --all-targets --locked
cargo fmt --all -- --check
```

Готовые релизы именно этого личного форка пока не опубликованы. Релизы исходного проекта не содержат автоматически все изменения этой ветки.

Личные раскладки, рыночный кэш, логи, тестовые профили и собранные exe не загружены в репозиторий. [Изолированный тестовый профиль](docs/profiles.md) позволяет проверять сборку отдельно от рабочих настроек.

## Происхождение и лицензия

Основа — [Niketion/flowdepth](https://github.com/Niketion/flowdepth), происходящий от [Flowsurface](https://github.com/flowsurface-rs/flowsurface). История и авторство исходных изменений сохранены. Лицензия — [GNU GPL v3 или более поздняя](LICENSE).
