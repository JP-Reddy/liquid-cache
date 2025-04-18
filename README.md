<p align="center"> <img src="https://raw.githubusercontent.com/XiangpengHao/liquid-cache/main/dev/doc/logo.png" alt="liquid_cache_logo" width="450"/> </p>


[![Crates.io Version](https://img.shields.io/crates/v/liquid-cache-client?label=liquid-cache-client)](https://crates.io/crates/liquid-cache-client)
[![Crates.io Version](https://img.shields.io/crates/v/liquid-cache-server?label=liquid-cache-server)](https://crates.io/crates/liquid-cache-server)
[![docs.rs](https://img.shields.io/docsrs/liquid-cache-client?style=flat&label=client-doc)](https://docs.rs/liquid-cache-client/latest/liquid_cache_client/)
[![docs.rs](https://img.shields.io/docsrs/liquid-cache-server?style=flat&label=server-doc)](https://docs.rs/liquid-cache-server/latest/liquid_cache_server/)
[![Rust CI](https://github.com/XiangpengHao/liquid-cache/actions/workflows/ci.yml/badge.svg)](https://github.com/XiangpengHao/liquid-cache/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/XiangpengHao/liquid-cache/graph/badge.svg?token=yTeQR2lVnd)](https://codecov.io/gh/XiangpengHao/liquid-cache)

LiquidCache is an object store cache designed for [DataFusion](https://github.com/apache/datafusion) based systems. Simply register LiquidCache as the `TableProvider` to enjoy the performance boost. 
Depending on your usage, LiquidCache can easily achieve 10x lower cost and latency.

## Architecture

Both LiquidCache and DataFusion run on cloud servers within the same region, but are configured differently:

- LiquidCache often has a memory/CPU ratio of 16:1 (e.g., 64GB memory and 4 cores)
- DataFusion often has a memory/CPU ratio of 2:1 (e.g., 32GB memory and 16 cores)

Multiple DataFusion nodes share the same LiquidCache instance through network connections. 
Each component can be scaled independently as the workload grows. 

Under the hood, LiquidCache transcodes and caches Parquet data from object storage, and evaluates filters before sending data to DataFusion,
effectively reducing both CPU utilization and network data transfer on cache servers.

<img src="https://raw.githubusercontent.com/XiangpengHao/liquid-cache/main/dev/doc/arch.png" alt="architecture" width="400"/>



## Integrate LiquidCache in 5 Minutes
Check out the `examples` folder for more details. 

#### 1. Include the dependencies:
In your client's `Cargo.toml`:
```toml
[dependencies]
liquid-cache-client = "0.1.0"
```

In your cache server's `Cargo.toml`:
```toml
[dependencies]
liquid-cache-server = "0.1.0"
```

#### 2. Start a Cache Server:
```rust
use arrow_flight::flight_service_server::FlightServiceServer;
use datafusion::prelude::SessionContext;
use liquid_cache_server::LiquidCacheService;
use tonic::transport::Server;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let liquid_cache = LiquidCacheService::new(
        SessionContext::new(),
        Some(1024 * 1024 * 1024),               // max memory cache size 1GB
        Some(tempfile::tempdir()?.into_path()), // disk cache dir
    );

    let flight = FlightServiceServer::new(liquid_cache);

    Server::builder()
        .add_service(flight)
        .serve("0.0.0.0:50051".parse()?)
        .await?;

    Ok(())
}
```

Or use our pre-built docker image:
```bash
docker run -p 50051:50051 -v ~/liquid_cache:/cache \
  ghcr.io/xiangpenghao/liquid-cache/liquid-cache-server:latest \
  /app/bench_server \
  --address 0.0.0.0:50051 \
  --disk-cache-dir /cache
```

#### 3. Setup client:
```rust
use datafusion::{
    error::Result,
    prelude::{SessionConfig, SessionContext},
};
use liquid_cache_client::LiquidCacheTableBuilder;
use std::sync::Arc;
use url::Url;

#[tokio::main]
pub async fn main() -> Result<()> {
    let mut session_config = SessionConfig::from_env()?;
    session_config
        .options_mut()
        .execution
        .parquet
        .pushdown_filters = true;
    let ctx = Arc::new(SessionContext::new_with_config(session_config));

    let cache_server = "http://localhost:50051";
    let table_name = "aws_locations";
    let url = Url::parse(
        "https://raw.githubusercontent.com/tobilg/aws-edge-locations/main/data/aws-edge-locations.parquet",
    )
    .unwrap();
    let sql = "SELECT COUNT(*) FROM aws_locations WHERE \"countryCode\" = 'US';";

    let table = LiquidCacheTableBuilder::new(cache_server, table_name, url.as_ref())
        .with_object_store(
            format!("{}://{}", url.scheme(), url.host_str().unwrap_or_default()),
            None,
        )
        .build()
        .await?;
    ctx.register_table(table_name, Arc::new(table))?;

    ctx.sql(sql).await?.show().await?;

    Ok(())
}
```

## Community server

We run a community server for LiquidCache at <https://hex.tail0766e4.ts.net:50051> (hosted on Xiangpeng's NAS, use at your own risk).

You can try it out by running:
```bash
cargo run --bin example_client --release -- \
    --cache-server https://hex.tail0766e4.ts.net:50051 \
    --file "https://huggingface.co/datasets/HuggingFaceFW/fineweb/resolve/main/data/CC-MAIN-2024-51/000_00042.parquet" \
    --query "SELECT COUNT(*) FROM \"000_00042\" WHERE \"token_count\" < 100"
```

Expected output (within a second):
```
+----------+
| count(*) |
+----------+
| 44805    |
+----------+
```


## Run ClickBench 

#### 1. Setup the Repository
```bash
git clone https://github.com/XiangpengHao/liquid-cache.git
cd liquid-cache
```

#### 2. Run a LiquidCache Server
```bash
cargo run --bin bench_server --release
```

#### 3. Run a ClickBench Client
In a different terminal, run the ClickBench client:
```bash
cargo run --bin clickbench_client --release -- --query-path benchmark/clickbench/queries.sql --file examples/nano_hits.parquet
```
(Note: replace `nano_hits.parquet` with the [real ClickBench dataset](https://github.com/ClickHouse/ClickBench) for full benchmarking)


## Development

See [dev/README.md](./dev/README.md)

## Benchmark

See [benchmark/README.md](./benchmark/README.md)

## FAQ

#### Can I use LiquidCache in production today?

Not yet. While production readiness is our goal, we are still implementing features and polishing the system.
LiquidCache began as a research project exploring new approaches to build cost-effective caching systems. Like most research projects, it takes time to mature, and we welcome your help!

#### Does LiquidCache cache data or results?

LiquidCache is a data cache. It caches logically equivalent but physically different data from object storage.

LiquidCache does not cache query results - it only caches data, allowing the same cache to be used for different queries.

#### Nightly Rust, seriously?

We will transition to stable Rust once we believe the project is ready for production.

#### How does LiquidCache work?

Check out our [paper](/dev/doc/liquid-cache-vldb.pdf) (under submission to VLDB) for more details. Meanwhile, we are working on a technical blog to introduce LiquidCache in a more accessible way.

#### How can I get involved?

We are always looking for contributors! Any feedback or improvements are welcome. Feel free to explore the issue list and contribute to the project.
If you want to get involved in the research process, feel free to [reach out](https://haoxp.xyz/work-with-me/).

#### Who is behind LiquidCache?

LiquidCache is a research project funded by:
- [InfluxData](https://www.influxdata.com/)
- Taxpayers of the state of Wisconsin and the federal government. 

As such, LiquidCache is and will always be open source and free to use.

Your support for science is greatly appreciated!

## License

[Apache License 2.0](./LICENSE)
