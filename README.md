# ByteForge 2000

Десктопный редактор и анализатор бинарных/аудиофайлов
выделять аудио-дорожки, запускать плагины на C/C++ и делать
локальный эвристический "AI"-анализ данных — без интернета.

## Возможности

- Открытие / сохранение любого файла, редактирование байта по offset.
- Вкладки: HEX, ASCII, PREVIEW, AUDIO, CONSOLE, ANALYSIS, LIBRARY.
- **Аудио.** Разбор RIFF/WAVE + PCM 8/16 бит, автоматическая
  сегментация на дорожки (Attack / Sustain / Release / Silence /
  Noise) по RMS-энергии и zero-crossing rate. Для файлов без
  WAVE-заголовка работает эвристический фоллбэк на сырых байтах.
  Дорожки можно переименовывать и менять их тип в редакторе (вкладка
  AUDIO).
- **AI-анализ.** Энтропия Шеннона, printable ratio, поиск известных
  сигнатур форматов (PNG/JPEG/ZIP/PDF/ELF/MZ/RIFF/GZIP/RAR/7z/SQLite,
  в т.ч. встроенных на произвольном смещении), эвристический вердикт
  по типу содержимого.
- **Плагины на C/C++.** Загружаются как shared library (.dll / .so /
  .dylib) через `libloading`, см. `plugins/plugin_api.h`. Библиотека
  LIBRARY показывает имя, тип, описание, версию и статус каждого
  плагина; кнопка RUN вызывает `plugin_process()` и печатает результат
  в CONSOLE с тегом `[PLUGIN]`.
- Консоль с тегами `[SYSTEM]`, `[CHANGE]`, `[ERROR]`, `[PLUGIN]`,
  `[AI]`, `[AUDIO]`.

## Структура проекта

```
byteforge2000/
├── Cargo.toml
├── src/
│   └── main.rs              # GUI (egui) + core-анализ + аудио + загрузчик плагинов
├── plugins/
│   ├── plugin_api.h         # C ABI для плагинов
│   ├── example_entropy_plugin.c
│   └── (сюда кладутся скомпилированные .dll/.so/.dylib)
├── README.md
└── LICENSE
```

## Сборка

```bash
cargo build --release
cargo run --release
```

## Сборка примера плагина

```bash
# Linux / macOS
cd plugins
gcc -shared -fPIC example_entropy_plugin.c -o entropy_plugin.so -lm
# положить .so обратно в plugins/, нажать RELOAD PLUGINS в приложении

# Windows (MSVC)
cl /LD example_entropy_plugin.c /Fe:entropy_plugin.dll
```

## Roadmap

- **MVP (готово):** открытие/сохранение, HEX/ASCII/PREVIEW,
  редактирование байтов, базовый анализ, библиотека плагинов, старый GUI.
- **V2 (готово в этой версии):** плагины на C/C++ через libloading,
  локальный AI-анализ (энтропия + сигнатуры + вердикт), автоматическое
  выделение аудио-дорожек.
- **V3 (следующие шаги):** более гибкий редактор аудио-дорожек
  (перетаскивание границ, прослушивание, экспорт сегментов),
  расширенный pattern-matching, экспорт отчётов анализа, вынос
  тяжёлой части эвристик (entropy/feature extraction) в отдельный
  C/C++ core-модуль, который UI будет вызывать так же, как плагины.

## Лицензия

Apache License 2.0 — см. `LICENSE`.
