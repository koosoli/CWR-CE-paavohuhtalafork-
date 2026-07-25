# Getting Started

<cite>
**Referenced Files in This Document**
- [CMakeLists.txt](file://CMakeLists.txt)
- [CMakePresets.json](file://CMakePresets.json)
- [vcpkg.json](file://vcpkg.json)
- [.trident.env.example](file://.trident.env.example)
- [scripts/Build.ps1](file://scripts/Build.ps1)
- [scripts/Install.ps1](file://scripts/Install.ps1)
- [scripts/Start.ps1](file://scripts/Start.ps1)
- [cmake/presets/base.json](file://cmake/presets/base.json)
- [cmake/presets/windows.json](file://cmake/presets/windows.json)
- [cmake/presets/linux.json](file://cmake/presets/linux.json)
- [apps/cwr/Game/CMakeLists.txt](file://apps/cwr/Game/CMakeLists.txt)
- [apps/cwr/Server/CMakeLists.txt](file://apps/cwr/Server/CMakeLists.txt)
- [apps/tools/Studio/CMakeLists.txt](file://apps/tools/Studio/CMakeLists.txt)
</cite>

## Table of Contents
1. [Introduction](#introduction)
2. [System Requirements](#system-requirements)
3. [Installation Guide](#installation-guide)
4. [Environment Setup](#environment-setup)
5. [Build Configuration](#build-configuration)
6. [Building the Project](#building-the-project)
7. [Running the Game](#running-the-game)
8. [Accessing the Editor](#accessing-the-editor)
9. [Running Tests](#running-tests)
10. [Troubleshooting](#troubleshooting)
11. [IDE Setup](#ide-setup)
12. [Quick Start Examples](#quick-start-examples)
13. [Platform-Specific Considerations](#platform-specific-considerations)
14. [Verification Steps](#verification-steps)

## Introduction

CWR-CE (Command & Conquer Remastered - Community Edition) is a modern reimplementation of the classic Command & Conquer game engine. This getting started guide will help you set up a complete development environment to build, run, and modify the game across Windows and Linux platforms. The project uses modern C++ with CMake as the primary build system, vcpkg for dependency management, and supports multiple toolchains including Clang and GCC.

The repository contains the core game engine, client applications, server components, development tools, and comprehensive test suites. Whether you're looking to contribute to the engine, develop mods, or simply run the game locally, this guide will get you up and running quickly.

## System Requirements

### Minimum Requirements
- **Operating System**: Windows 10/11 (64-bit) or Linux (Ubuntu 20.04+, Debian 11+)
- **Processor**: x86_64 compatible CPU with SSE4.2 support
- **Memory**: 8 GB RAM minimum, 16 GB recommended
- **Storage**: 10 GB free disk space for development environment
- **Graphics**: OpenGL 3.3+ compatible GPU with current drivers

### Development Requirements
- **CMake**: Version 3.20 or higher
- **C++ Compiler**: 
  - Windows: MSVC 2019+ or Clang 12+
  - Linux: GCC 9+ or Clang 12+
- **vcpkg**: Latest version from GitHub
- **Git**: For cloning the repository
- **Python 3.8+**: For some build scripts and tools

### Optional Dependencies
- **Visual Studio 2019+** (Windows): Full IDE experience with IntelliSense
- **CLion** (Cross-platform): Excellent CMake integration
- **VS Code**: With C/C++ extension and CMake Tools
- **Docker**: For containerized builds and testing

## Installation Guide

### Prerequisites Installation

#### Windows Setup

1. **Install Visual Studio Build Tools** (for MSVC):
   ```powershell
   winget install Microsoft.VisualStudio.2022.BuildTools --includeRecommended
   ```

2. **Install Git**:
   ```powershell
   winget install Git.Git
   ```

3. **Install Python 3.8+**:
   ```powershell
   winget install Python.Python.3.11
   ```

4. **Install vcpkg**:
   ```powershell
   git clone https://github.com/microsoft/vcpkg.git
   cd vcpkg
   .\bootstrap-vcpkg.bat
   .\vcpkg integrate install
   cd ..
   ```

#### Linux Setup

1. **Install build dependencies** (Ubuntu/Debian):
   ```bash
   sudo apt update
   sudo apt install -y build-essential cmake git python3 python3-pip
   ```

2. **Install additional dependencies**:
   ```bash
   sudo apt install -y libgl-dev libopenal-dev libsndfile1-dev
   ```

3. **Install vcpkg**:
   ```bash
   git clone https://github.com/microsoft/vcpkg.git
   cd vcpkg
   ./bootstrap.sh
   ./vcpkg integrate install
   cd ..
   ```

### Repository Cloning

Clone the CWR-CE repository and initialize submodules:

```bash
git clone https://github.com/your-repo/CWR-CE.git
cd CWR-CE
git submodule update --init --recursive
```

## Environment Setup

### Configure vcpkg Integration

Set up vcpkg as the package manager for your project:

```bash
# Set VCPKG_ROOT environment variable
export VCPKG_ROOT=/path/to/vcpkg  # Linux
set VCPKG_ROOT=C:\path\to\vcpkg    # Windows PowerShell

# Install required dependencies
./vcpkg install --triplet x64-windows
./vcpkg install --triplet x64-linux
```

### Environment Variables Configuration

Create and configure the `.trident.env` file based on the example:

```bash
# Copy the example configuration
cp .trident.env.example .trident.env

# Edit the configuration file with your settings
# Required variables:
# TRIDENT_GAME_DIR - Path to game data directory
# TRIDENT_LOG_LEVEL - Logging verbosity (debug, info, warning, error)
# TRIDENT_CONFIG_DIR - Custom configuration directory path
```

### Platform-Specific Environment Setup

#### Windows Environment Variables
```powershell
# Add to user environment variables
[System.Environment]::SetEnvironmentVariable("VCPKG_ROOT", "C:\path\to\vcpkg", "User")
[System.Environment]::SetEnvironmentVariable("TRIDENT_GAME_DIR", "C:\path\to\game\data", "User")
```

#### Linux Environment Variables
```bash
# Add to ~/.bashrc or ~/.zshrc
export VCPKG_ROOT=/path/to/vcpkg
export TRIDENT_GAME_DIR=/path/to/game/data
export TRIDENT_LOG_LEVEL=info
```

## Build Configuration

### CMake Presets Overview

The project uses CMake presets for cross-platform build configurations. Available presets include:

- `base`: Common base configuration
- `windows`: Windows-specific optimizations and settings
- `linux`: Linux-specific optimizations and settings
- `sanitizers`: Debug builds with memory sanitizers enabled

### Configure Your Build

#### Windows Configuration
```powershell
# Generate build files using preset
cmake --preset windows

# Or configure manually
cmake -B build -S . -DCMAKE_TOOLCHAIN_FILE=vcpkg/scripts/buildsystems/vcpkg.cmake
```

#### Linux Configuration
```bash
# Generate build files using preset
cmake --preset linux

# Or configure manually
cmake -B build -S . -DCMAKE_TOOLCHAIN_FILE=vcpkg/scripts/buildsystems/vcpkg.cmake
```

### Build Types

The project supports different build types:

- **Debug**: Full debugging symbols, no optimization
- **Release**: Optimized build for distribution
- **RelWithDebInfo**: Release build with debug symbols
- **MinSizeRel**: Size-optimized release build

## Building the Project

### Quick Build Commands

#### Windows (MSVC)
```powershell
# Build all targets
cmake --build build --config Debug

# Build specific target
cmake --build build --config Debug --target cwr_game

# Build with parallel jobs
cmake --build build --config Debug --parallel
```

#### Linux
```bash
# Build all targets
cmake --build build --config Debug

# Build specific target
cmake --build build --config Debug --target cwr_game

# Build with parallel jobs
cmake --build build --config Debug -j$(nproc)
```

### Using Build Scripts

The project includes PowerShell scripts for common operations:

```powershell
# Install dependencies
.\scripts\Install.ps1

# Build the project
.\scripts\Build.ps1

# Run the game
.\scripts\Start.ps1
```

### Incremental Builds

For faster development cycles, use incremental builds:

```bash
# Only rebuild changed files
cmake --build build --config Debug

# Clean build artifacts
cmake --build build --config Debug --target clean
```

## Running the Game

### Direct Execution

After building, locate and execute the game binary:

#### Windows
```cmd
# Navigate to build directory
cd build\Debug

# Run the game executable
cwr_game.exe
```

#### Linux
```bash
# Navigate to build directory
cd build/Debug

# Run the game executable
./cwr_game
```

### Running with Specific Options

```bash
# Run with custom config directory
./cwr_game --config-dir /path/to/config

# Enable debug logging
./cwr_game --log-level debug

# Specify game directory
./cwr_game --game-dir /path/to/game/data
```

### Server Execution

To run the multiplayer server:

```bash
# Build server target
cmake --build build --config Debug --target cwr_server

# Run server
./cwr_server --port 2303 --max-players 16
```

## Accessing the Editor

### Building the Studio

The development studio provides an integrated development environment:

```bash
# Build the studio target
cmake --build build --config Debug --target cwr_studio

# Run the studio
./cwr_studio
```

### Studio Features

The studio includes:
- Mission editor with real-time preview
- Asset browser and manipulation tools
- Scripting console for live code execution
- Performance profiling and debugging tools

### Editor Configuration

Configure the editor through the settings menu or command line:

```bash
# Launch with specific mission
./cwr_studio --mission /path/to/mission.sqm

# Enable developer mode
./cwr_studio --dev-mode
```

## Running Tests

### Unit Tests

Execute the unit test suite:

```bash
# Build tests
cmake --build build --config Debug --target tests

# Run all tests
ctest --test-dir build/Debug --verbose

# Run specific test category
ctest --test-dir build/Debug -R "unit" --verbose
```

### Integration Tests

Run integration tests that require full game environment:

```bash
# Build integration tests
cmake --build build --config Debug --target integration_tests

# Run integration tests
ctest --test-dir build/Debug -L integration --verbose
```

### Performance Tests

Execute performance benchmarking:

```bash
# Build performance tests
cmake --build build --config Debug --target perf_tests

# Run performance benchmarks
ctest --test-dir build/Debug -L performance --verbose
```

### Test Filtering

Filter tests by various criteria:

```bash
# By name pattern
ctest -R "audio"

# By property
ctest -L "unit"

# Exclude certain tests
ctest -E "slow"
```

## Troubleshooting

### Common Build Issues

#### vcpkg Dependency Problems
```bash
# Reset vcpkg state
rm -rf vcpkg/downloads
rm -rf vcpkg/installed

# Reinstall dependencies
./vcpkg install --triplet x64-windows
```

#### Missing Dependencies
```bash
# Check installed packages
./vcpkg list

# Install missing packages
./vcpkg install <package-name>
```

#### Compiler Errors
```bash
# Clear build cache
rm -rf build/*

# Regenerate build files
cmake --preset windows --fresh
```

### Runtime Issues

#### Graphics Driver Problems
- Update GPU drivers to latest version
- Verify OpenGL 3.3+ support
- Check for driver-specific workarounds

#### Audio Issues
- Ensure OpenAL Soft is properly installed
- Check audio device permissions
- Verify sound card compatibility

#### Network Connectivity
- Verify firewall settings allow game ports
- Check network connectivity to master servers
- Ensure proper DNS resolution

### Log Analysis

Enable verbose logging for troubleshooting:

```bash
# Set log level to debug
export TRIDENT_LOG_LEVEL=debug

# View log output
tail -f logs/trident.log
```

## IDE Setup

### Visual Studio (Windows)

1. **Open the project**:
   - File → Open → Folder → Select CWR-CE directory
   - Visual Studio will detect CMake project automatically

2. **Configure CMake settings**:
   - CMake Settings → General → Generator: Visual Studio 17 2022
   - Toolchain file: `vcpkg/scripts/buildsystems/vcpkg.cmake`

3. **Set startup project**:
   - Right-click `cwr_game` → Set as Startup Project

### CLion (Cross-platform)

1. **Import project**:
   - File → Open → Select CWR-CE directory
   - CLion will automatically detect CMake configuration

2. **Configure toolchain**:
   - Settings → Build, Execution, Deployment → Toolchains
   - Add vcpkg triplet configuration

3. **Set up debugging**:
   - Edit Configurations → Add new CMake Application
   - Select appropriate target and build type

### VS Code

1. **Install extensions**:
   - C/C++ Extension Pack
   - CMake Tools
   - CMake Language Support

2. **Configure CMake**:
   - CMake: Select Kit → Choose compiler
   - CMake: Select Variant → Choose build type

3. **Set up tasks**:
   - Create `.vscode/tasks.json` for build automation
   - Configure launch configurations for debugging

## Quick Start Examples

### Basic Game Launch

```bash
# One-liner to build and run
cmake --preset windows && cmake --build build --config Debug && ./build/Debug/cwr_game.exe
```

### Development Workflow

```bash
# Configure once
cmake --preset windows

# Build and run loop
while true; do
    cmake --build build --config Debug --target cwr_game
    ./build/Debug/cwr_game.exe
done
```

### Testing Workflow

```bash
# Build and run tests
cmake --preset windows
cmake --build build --config Debug
ctest --test-dir build/Debug --output-on-failure
```

### Server Development

```bash
# Build server only
cmake --build build --config Debug --target cwr_server

# Run with debug logging
./build/Debug/cwr_server --log-level debug --port 2303
```

## Platform-Specific Considerations

### Windows-Specific Notes

#### Visual Studio Integration
- Use Developer PowerShell for consistent environment
- Set `VCPKG_DEFAULT_TRIPLET=x64-windows` for default architecture
- Consider using `x64-windows-static` triplet for static linking

#### Path Handling
- Use forward slashes in paths when possible
- Avoid spaces in installation directories
- Consider using short paths for long directory names

### Linux-Specific Notes

#### Package Management
- Use distribution package managers for system libraries
- Consider using containers for consistent environments
- Set up proper permissions for audio and graphics devices

#### Build Optimization
- Use `-march=native` for CPU-specific optimizations
- Consider LTO (Link Time Optimization) for release builds
- Use ccache for faster incremental builds

### Cross-Platform Compatibility

#### File Paths
- Use platform-appropriate path separators
- Implement proper path normalization
- Handle case sensitivity differences

#### Line Endings
- Configure Git to handle line endings appropriately
- Use `.gitattributes` for consistent behavior
- Be aware of CRLF vs LF differences

## Verification Steps

### Build Verification

1. **Check compilation success**:
   ```bash
   cmake --build build --config Debug
   echo $?  # Should return 0
   ```

2. **Verify binaries exist**:
   ```bash
   ls -la build/Debug/cwr_game*
   ls -la build/Debug/cwr_server*
   ```

3. **Test basic functionality**:
   ```bash
   ./build/Debug/cwr_game --version
   ./build/Debug/cwr_server --help
   ```

### Runtime Verification

1. **Graphics initialization**:
   - Launch game and verify window opens
   - Check for rendering errors in logs
   - Verify frame rate is reasonable

2. **Audio initialization**:
   - Confirm sound effects play correctly
   - Test music playback
   - Verify microphone input (if applicable)

3. **Network connectivity**:
   - Test connection to local server
   - Verify master server registration
   - Check multiplayer features

### Test Suite Verification

1. **Run unit tests**:
   ```bash
   ctest --test-dir build/Debug -R "unit" --verbose
   ```

2. **Check test results**:
   - All tests should pass
   - No memory leaks detected
   - Performance within acceptable ranges

3. **Integration test validation**:
   - Multiplayer scenarios work correctly
   - Save/load functionality verified
   - Mod loading works as expected

### Environment Validation

1. **Dependency check**:
   ```bash
   ./vcpkg list
   ```

2. **Compiler verification**:
   ```bash
   gcc --version
   clang --version
   ```

3. **System requirements**:
   - OpenGL version check
   - Audio device availability
   - Network connectivity

This comprehensive getting started guide provides everything needed to set up a complete CWR-CE development environment. The modular structure allows developers to focus on specific aspects while maintaining overall system coherence. Regular updates to this documentation ensure it remains current with the evolving codebase and development practices.