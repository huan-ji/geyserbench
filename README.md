# GeyserBench

A Yellowstone Geyser gRPC endpoint benchmarking tool.

## Overview

GeyserBench is a performance testing tool that connects to multiple Solana gRPC endpoints simultaneously and measures their speed and reliability in detecting transactions.

## Features

- Connect to multiple gRPC endpoints simultaneously
- Measure endpoint performance metrics:
    - First detection rate
    - Latency percentiles (p50, p95, p99)
- Simple summary output for quick comparison
- Detailed metrics for in-depth analysis

## Installation

### Download Binary

Download the latest release from the [releases page](https://github.com/solstackapp/geyserbench/releases).

## Configuration

When first run, GeyserBench will create a default `config.toml` file. Edit this file to customize your benchmark:

```toml
[config]
transactions = 1000
account = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"
commitment = "processed"

[[endpoint]]
name = "Yellowstone GRPC"
url = "https://api.mainnet-beta.solana.com:10000"
x_token = "YOUR_TOKEN_HERE"
kind = "yellowstone"

[[endpoint]]
name = "Corvus ARPC"
url = "http://0.0.0.0:20202"
x_token = ""
kind = "arpc"

[[endpoint]]
name = "Thor Streamer"
url = "http://0.0.0.0:50051"
x_token = "YOUR_TOKEN_HERE"
kind = "thor"

[[endpoint]]
name = "Shreder.xyz"
url = "http://0.0.0.0:10000"
x_token = "YOUR_TOKEN_HERE"
kind = "shreder"

[[endpoint]]
name = "Shredstream"
url = "http://0.0.0.0:10000"
kind = "shredstream"
```

### Configuration Options

- `transactions`: Number of transactions to measure
- `account`: Account address to monitor for transactions
- `commitment`: Transaction commitment level (processed, confirmed, finalized)
- `endpoint`: Array of gRPC endpoint configurations:
    - `name`: Name for the endpoint
    - `url`: gRPC endpoint URL
    - `x_token`: Authentication token (if required)
    - `kind`: Geyser provider type (yellowstone, arpc, thor, shreder, etc.)

## Usage

1. Run GeyserBench to generate the default config:
   ```bash
   ./geyserbench
   ```

2. Edit the generated `config.toml` file with your endpoint details

3. Run the benchmark:
   ```bash
   ./geyserbench
   ```

## Output

GeyserBench provides both simplified and detailed output:

### Simple Summary
```
Finished test results
endpoint 1: Win rate 85.23%, p50 0.00ms (fastest)
endpoint 2: Win rate 10.45%, p50 42.31ms
endpoint 3: Win rate 4.32%, p50 78.56ms
```

### Detailed Metrics
```
Detailed test results
----------------------------------

Fastest Endpoint: endpoint 1
  First detections: 82 out of 97 valid transactions (84.54%)

Delays relative to fastest endpoint:
endpoint 2:
  P50 latency: 38.75 ms
  P95 latency: 62.18 ms
  P99 latency: 84.03 ms
  Valid transactions: 97

endpoint 3:
  P50 latency: 71.22 ms
  P95 latency: 121.57 ms
  P99 latency: 189.45 ms
  Valid transactions: 97
```
