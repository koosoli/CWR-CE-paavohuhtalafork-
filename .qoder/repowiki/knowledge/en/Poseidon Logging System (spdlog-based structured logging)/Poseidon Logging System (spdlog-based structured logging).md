---
kind: logging_system
name: Poseidon Logging System (spdlog-based structured logging)
category: logging_system
scope:
    - '**'
source_files:
    - engine/Poseidon/Foundation/Framework/Log.hpp
    - engine/Poseidon/Foundation/Logging/Logging.hpp
    - engine/Poseidon/Foundation/Logging/Logging.cpp
    - engine/Poseidon/Foundation/Framework/DebugLog.hpp
    - apps/cwr/Server/ServerApplication.cpp
---

The CWR-CE engine uses a centralized spdlog-backed logging subsystem built around the `Foundation::LoggingSystem` class. It provides per-category loggers, configurable sinks, structured output, and strict-mode error tracking.

**Framework and core components**
- The public API is exposed via `LOG_TRACE/DEBUG/INFO/WARN/ERROR(category, ...)` macros defined in `engine/Poseidon/Foundation/Framework/Log.hpp`. Each macro forwards to an `spdlog::logger` selected from a per-category cache populated by `LoggingSystem::Initialize()`; before initialization calls fall back to spdlog's default logger.
- `LoggingSystem` (header + implementation in `engine/Poseidon/Foundation/Logging/`) owns all spdlog loggers and sinks. It creates one logger per category (Core, Config, Memory, Graphics, Audio, Input, Network, World, Script, AI, Physics, UI, Mission), all sharing the same sink pipeline.
- A custom `PoseidonFormatter` (`spdlog::custom_flag_formatter`) renders `[app-tag] [LEVEL] [CATEGORY] message`, using the logger name as the category tag so no string parsing of the message body is needed.
- An `ErrorCountingSink` increments a static error counter used by integration tests (`triErrorCount`) and by the `--strict` mode which turns any error-level log into a fatal trip.

**Initialization and configuration**
- `LoggingSystem::Initialize(logLevel, categoryFilter, logFormat, logFile)` parses level strings (trace/debug/info/warn/error/critical/off), applies a comma-separated category filter, selects text or JSONL output, and attaches sinks.
- `InitializeFromConfig(appPrefix)` reads `AppConfig` for log level, categories, format, file path, strict mode, and app tag; if no CLI tag is provided it generates `<prefix>-<pid>` (e.g. `app-1a2b`).
- Sinks attached: stdout color console (text mode) or a JSONL sink, always an error-counting sink, and an optional `basic_file_sink_mt` when `--log-file` is set. Flush policy flushes every message to files (abrupt exit safety) and only on warn+ for console.

**Output formats and sinks**
- Text mode: colored console with pattern `[%Y-%m-%d %H:%M:%S.%e] %*%v`, where `%*` is expanded by `PoseidonFormatter` to `[APP_TAG] [LEVEL] [CATEGORY] `. File logs use the same formatter.
- JSONL mode: a dedicated `JsonlSink` produces one JSON object per line, suitable for structured log ingestion.
- Dedicated server (`apps/cwr/Server/ServerApplication.cpp`) also registers its own named `"Server"` spdlog logger with both stdout and timestamped file sinks, independent of the engine's LoggingSystem.

**Categories and levels**
- Categories are an enum (`Core`, `Config`, `Memory`, `Graphics`, `Audio`, `Input`, `Network`, `World`, `Script`, `AI`, `Physics`, `UI`, `Mission`) mapped to logger names used directly by spdlog.
- Levels follow spdlog's trace/debug/info/warn/error/critical; the codebase primarily uses DEBUG, INFO, WARN, ERROR.

**Strict mode and error counting**
- `SetStrictMode(true)` enables strict mode; once enabled, any `LOG_ERROR` call sets `StrictTriipped()`, which the game main loop polls to perform a clean non-zero exit.
- `GetErrorCount()` / `ResetErrorCount()` provide thread-safe access to the cumulative error count for test assertions.

**Conventions observed across the codebase**
- All engine code uses `LOG_<LEVEL>(Category, "...", args...)` — never direct spdlog calls except in the server console bootstrap.
- Category tags appear inline in output and are used for filtering at init time via the `categoryFilter` parameter.
- Debug-only helpers in `DebugLog.hpp` (`DebugLog`, `DebF`, `Log`, `DoAssert`, `POSEIDON_LOG_CHECK`) wrap the same LOG_* macros, with release builds collapsing them to no-ops or error-level logs.
- Performance timing uses `ScopedTimer` which emits `LOG_DEBUG(<Category>, "PERF: ... took N.NNms")` on destruction.
- Legacy `#define LOG_*` flags in older modules (e.g. `AIDefs.hpp`) are compile-time switches that are disabled by default.