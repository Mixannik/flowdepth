# Flowdepth: личный order-flow форк

Основан на [Niketion/flowdepth](https://github.com/Niketion/flowdepth). История и исходная лицензия сохранены. Основная рабочая ветка: `feat/delta-table`.

## С чего начать ревью

- [Карта проекта](docs/project-guide.md)
- [Архитектура и её этапы](docs/core-architecture.md)
- [Текущее состояние и ограничения](docs/state.md)
- [Контракт единиц и количества](docs/quantity-contract.md)
- [Пакет ядра профилей](docs/profile-core-batch.md)
- [Направление интерфейса терминала](docs/terminal-workspace.md)
- [План order-flow функций](docs/orderflow-roadmap.md)

Особое внимание: совпадение live/history/cache, полнота данных до публикации уровней в сигналы, сохранность раскладок, жизненный цикл загрузок и независимость расчётов от рисования. Архитектурная работа ещё не завершена; открытые ограничения перечислены в документации.

## Проверки

```sh
cargo test --release --workspace --locked
cargo check --workspace --all-targets --locked
cargo fmt --all -- --check
```

Последний общий прогон перед публикацией: 548 passed, 15 ignored, 0 failed. Часть ignored требует GPU или отдельного stress-прогона. Проверки запускаются для workspace: отдельные пакеты могут зависеть от объединения features корневого приложения.

Личные saved-state, рыночный кэш, логи, тестовые профили и собранные exe не являются частью этой копии. Их локальные пути в заметках разработчика — справочная информация, а не обязательная структура для клонирования и сборки. Инструкции по тестовому профилю: [docs/profiles.md](docs/profiles.md).
