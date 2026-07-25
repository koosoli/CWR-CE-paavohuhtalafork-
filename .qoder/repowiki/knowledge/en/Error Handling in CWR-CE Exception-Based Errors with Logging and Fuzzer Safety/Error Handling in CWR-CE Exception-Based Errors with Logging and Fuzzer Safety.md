---
kind: error_handling
name: 'Error Handling in CWR-CE: Exception-Based Errors with Logging and Fuzzer Safety'
category: error_handling
scope:
    - '**'
source_files:
    - engine/Poseidon/Asset/Formats/BISBinaryStream.hpp
    - engine/Poseidon/Foundation/Framework/Log.hpp
    - apps/cwr/Server/ServerApplication.cpp
    - apps/cwr/Game/GameApplication.cpp
    - apps/fuzzers/Fuzzer/fuzz_p3d.cpp
---

## What system/approach is used
The CWR-CE codebase uses **C++ exceptions** (`std::runtime_error`, `std::exception`) as the primary error propagation mechanism, combined with **spdlog-based structured logging** for error reporting. There is no centralized error type hierarchy or result-returning pattern (no `std::expected` or custom Result types). The approach is exception-driven with defensive catch blocks at application boundaries.

## Key files and packages
- **engine/Poseidon/Asset/Formats/BISBinaryStream.hpp**: Core format parsing that throws `std::runtime_error` on malformed input (string length validation, array bounds checking, LZSS decompression failures)
- **engine/Poseidon/Foundation/Framework/Log.hpp**: Unified logging framework with per-category spdlog loggers (Core, Config, Memory, Graphics, Audio, Input, Network, World, Script, AI, Physics, UI, Mission)
- **apps/cwr/Server/ServerApplication.cpp**: Top-level exception handling catching `spdlog::spdlog_ex`, `std::exception`, and unknown exceptions during server initialization
- **apps/cwr/Game/GameApplication.cpp**: Uses `std::error_code` for filesystem operations while relying on exceptions for other error paths
- **apps/fuzzers/Fuzzer/*.cpp**: Consistent `catch (...)` patterns to prevent fuzzer crashes from malformed input

## Architecture and conventions
- **Exception throwing**: Low-level parsers and validators throw `std::runtime_error` with descriptive messages about what went wrong (e.g., "Invalid string length", "Array count exceeds remaining input", "LZSS decompression failed")
- **Boundary catching**: Application entry points catch exceptions and log them via LOG_ERROR macros before returning failure codes
- **Logging integration**: All errors are logged through the categorized spdlog system rather than being returned as values
- **Fuzzer safety**: Fuzzing harnesses wrap all parser calls in `catch (...)` blocks to ensure malformed input doesn't crash the fuzzer process
- **Filesystem operations**: Use `std::error_code` for non-fatal filesystem operations, allowing graceful fallback behavior

## Conventions and constraints
- Format parsers validate input aggressively and throw exceptions for any malformed data - this is enforced by the consistent use of runtime checks followed by `throw std::runtime_error(...)` throughout the asset loading pipeline
- Application startup code catches both typed exceptions (`std::exception`, `spdlog::spdlog_ex`) and unknown exceptions (`catch (...)`) to prevent crashes during initialization
- No custom exception classes exist - the codebase relies entirely on standard library exception types
- Error information flows through logging rather than return values, making debugging dependent on log analysis
- The absence of a unified error type means error handling is inconsistent between different subsystems (exceptions vs error codes)